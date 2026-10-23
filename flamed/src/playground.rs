//! A mint playground as the core chain.
//!
//! The playground simulates Bitcoin: one block a second, and burns that
//! weigh in each block's allotment. `PlaygroundIndexer` reads both over
//! its HTTP API, so the node mints on the playground's blocks and every
//! block that moves to a later one issues what the playground's minters
//! burned for — the same split the playground's own ledger shows.
//!
//! The playground has no block hashes, so a core block's hash is this
//! devnet's [`core_block_hash`] of its height, as on the mock chain. A
//! playground that is reset under a running node falls behind the Flame
//! anchor and minting stops with an error until both start over.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use flamechain::Burn;
use flamekd::Network;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::indexer::{core_block_hash, CoreBlock, CoreBurns, CoreError, CoreIndexer};
use crate::node::predicate_of;

/// How long one request may take, connecting and reading together.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The largest answer read, far above 1,024 blocks of burns.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// Follows a mint playground at `http://host:port`.
pub struct PlaygroundIndexer {
    url: String,
    authority: String,
    network: Network,
}

#[derive(Deserialize)]
struct Tip {
    height: u64,
    time: u64,
}

#[derive(Deserialize)]
struct Block {
    height: u64,
    time: u64,
    burns: Vec<PlaygroundBurn>,
}

#[derive(Deserialize)]
struct PlaygroundBurn {
    address: String,
    sats: u64,
}

impl PlaygroundIndexer {
    /// A playground at `url`, an `http://host:port` with no path, whose
    /// minters' addresses are read for `network`.
    pub fn new(url: &str, network: Network) -> Result<Self, CoreError> {
        let url = url.trim_end_matches('/').to_owned();
        let authority = url
            .strip_prefix("http://")
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .ok_or_else(|| CoreError::Playground {
                url: url.clone(),
                reason: "expected http://host:port".into(),
            })?
            .to_owned();
        Ok(Self {
            url,
            authority,
            network,
        })
    }

    fn error(&self, reason: impl ToString) -> CoreError {
        CoreError::Playground {
            url: self.url.clone(),
            reason: reason.to_string(),
        }
    }

    /// One `GET`, on a connection of its own. The playground answers with a
    /// `Content-Length`, and closing after each answer keeps this to
    /// reading until the end.
    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CoreError> {
        let address = self
            .authority
            .to_socket_addrs()
            .map_err(|error| self.error(error))?
            .next()
            .ok_or_else(|| self.error("the host resolves to nothing"))?;
        let mut stream =
            TcpStream::connect_timeout(&address, TIMEOUT).map_err(|error| self.error(error))?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(TIMEOUT)))
            .map_err(|error| self.error(error))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
            self.authority
        )
        .map_err(|error| self.error(error))?;
        let mut response = Vec::new();
        stream
            .take(MAX_RESPONSE_BYTES)
            .read_to_end(&mut response)
            .map_err(|error| self.error(error))?;

        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| self.error("the answer has no end of headers"))?;
        let head = String::from_utf8_lossy(&response[..split]);
        let body = &response[split + 4..];
        let status = head.lines().next().unwrap_or_default();
        if status.split(' ').nth(1) != Some("200") {
            return Err(self.error(format!(
                "GET {path} answered `{status}`: {}",
                String::from_utf8_lossy(body)
            )));
        }
        if head
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("transfer-encoding:"))
        {
            return Err(self.error("a chunked answer, which this reader does not take"));
        }
        serde_json::from_slice(body).map_err(|error| self.error(format!("GET {path}: {error}")))
    }
}

impl CoreIndexer for PlaygroundIndexer {
    /// The playground's last committed block. Before its first, the Flame
    /// genesis anchor at 0 is the tip, and the node stays there.
    fn tip(&mut self) -> Result<CoreBlock, CoreError> {
        let tip: Tip = self.get("/api/core/tip")?;
        Ok(CoreBlock {
            height: tip.height,
            hash: if tip.height == 0 {
                [0; 32]
            } else {
                core_block_hash(tip.height)
            },
            time: tip.time,
        })
    }

    fn blocks(&mut self, from: u64, to: u64) -> Result<Vec<CoreBurns>, CoreError> {
        let blocks: Vec<Block> = self.get(&format!("/api/core/blocks?from={from}&to={to}"))?;
        blocks
            .into_iter()
            .map(|block| {
                let burns = block
                    .burns
                    .into_iter()
                    .map(|burn| {
                        // The playground checked every address when it took
                        // the contribution; one that does not read here is a
                        // playground on another network.
                        let reward_predicate = predicate_of(&burn.address, self.network)
                            .map_err(|_| {
                                self.error(format!(
                                    "{} is not a receiving address on this network",
                                    burn.address
                                ))
                            })?;
                        Ok(Burn {
                            reward_predicate,
                            sats: burn.sats,
                        })
                    })
                    .collect::<Result<_, CoreError>>()?;
                Ok(CoreBurns {
                    block: CoreBlock {
                        height: block.height,
                        hash: core_block_hash(block.height),
                        time: block.time,
                    },
                    burns,
                })
            })
            .collect()
    }
}
