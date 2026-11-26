use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash, TxOut};
use corepc_client::types::model;
use corepc_client::types::v31::{GetBlockVerboseThree, WaitForNewBlock};
use corepc_client::{Client, GetBlockError};
use serde_json::json;

use crate::BitcoinConfig;
use crate::watcher::index::BlockIndex;
use crate::watcher::source::{ChainSource, SourceError};
use crate::watcher::types::{BlockId, Prevouts};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const LONG_POLL: Duration = Duration::from_secs(10);

const HEIGHT_OUT_OF_RANGE: i64 = -8;

/// [`ChainSource`] to one Bitcoin Core node, version 30 or newer: an older
/// one does not take the known block in `waitfornewblock`. Tested with
/// version 31.0.
///
/// The client is async and does its network I/O on tokio, so the futures of
/// this source need a tokio runtime.
pub struct RpcSource {
    client: Client,
}

impl RpcSource {
    pub fn new(config: &BitcoinConfig) -> Result<Self, SourceError> {
        let client = Client::new_with_auth(&config.node_rpc_url, config.auth.clone());
        Ok(RpcSource {
            client: client.map_err(error)?,
        })
    }
}

impl BlockIndex for RpcSource {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        match within(REQUEST_TIMEOUT, self.client.get_block_header(&id.hash)).await? {
            Ok(header) => Ok(Some(header)),
            Err(not_found) if not_found.is_not_found_error() => Ok(None),
            Err(other) => Err(error(other)),
        }
    }
}

impl ChainSource for RpcSource {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        let args = [json!(hash.to_string()), json!(3)];
        let call = self.client.call::<GetBlockVerboseThree>("getblock", &args);
        let verbose = match within(REQUEST_TIMEOUT, call)
            .await?
            .map_err(GetBlockError::from)
        {
            Ok(verbose) => verbose.into_model().map_err(error)?,
            Err(not_found) if not_found.is_not_found_error() => return Ok(None),
            Err(other) => return Err(error(other)),
        };
        let (block, prevouts) = block_with_prevouts(verbose);
        // The block was put together from the fields of the answer. Its hash
        // is checked by the watcher, the transactions are checked here.
        if !block.check_merkle_root() {
            return Err(SourceError(format!(
                "the transactions of block {hash} do not match its merkle root"
            )));
        }
        Ok(Some((block, prevouts)))
    }

    /// `waitfornewblock <timeout> <known block>`. If the best block is not
    /// `known` any more, Bitcoin Core answers at once; when the timeout
    /// expires, it returns the same block.
    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        let timeout_ms = LONG_POLL.as_millis() as u64;
        let args = [json!(timeout_ms), json!(known.to_string())];
        let wait = self.client.call("waitfornewblock", &args);
        let best: WaitForNewBlock = within(LONG_POLL + REQUEST_TIMEOUT, wait)
            .await?
            .map_err(error)?;
        let best = best.into_model().map_err(error)?;
        Ok(BlockId {
            height: best.height.into(),
            hash: best.hash,
        })
    }

    async fn block_at(&self, height: u64) -> Result<Option<BlockId>, SourceError> {
        // Bitcoin Core counts heights in 32 bits: a height that does not fit
        // is above its chain.
        let Ok(height_in_core) = u32::try_from(height) else {
            return Ok(None);
        };
        let hash = self.client.get_block_hash(height_in_core);
        match within(REQUEST_TIMEOUT, hash).await? {
            Ok(hash) => Ok(Some(BlockId { height, hash })),
            Err(too_high) if too_high.rpc_code() == Some(HEIGHT_OUT_OF_RANGE) => Ok(None),
            Err(other) => Err(error(other)),
        }
    }
}

fn block_with_prevouts(verbose: model::GetBlockVerboseThree) -> (Block, Prevouts) {
    let header = Header {
        version: verbose.version,
        // Only the block at height 0 has no previous block.
        prev_blockhash: verbose
            .previous_block_hash
            .unwrap_or_else(BlockHash::all_zeros),
        merkle_root: verbose.merkle_root,
        time: verbose.time,
        bits: verbose.bits,
        nonce: verbose.nonce,
    };
    let (txdata, prevouts) = verbose
        .tx
        .into_iter()
        .map(|entry| {
            let spent = entry.prevouts.into_iter().map(|prevout| {
                prevout.map(|prevout| TxOut {
                    value: prevout.value,
                    script_pubkey: prevout.script_pubkey.script_pubkey,
                })
            });
            (entry.transaction.transaction, spent.collect())
        })
        .unzip();
    (Block { header, txdata }, Prevouts::new(prevouts))
}

async fn within<T>(limit: Duration, call: impl Future<Output = T>) -> Result<T, SourceError> {
    tokio::time::timeout(limit, call)
        .await
        .map_err(|_| SourceError(format!("Bitcoin Core did not answer in {limit:?}")))
}

fn error(error: impl Display) -> SourceError {
    SourceError(error.to_string())
}
