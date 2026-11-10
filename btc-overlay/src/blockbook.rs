//! Mainnet headers from a Blockbook REST API (`/api/v2`).

use async_trait::async_trait;
use corepc_client::bitcoin::{BlockHash, CompactTarget, TxMerkleNode, block, hashes::Hash};
use serde_json::Value;

use crate::headers::{Header, HeaderError, HeaderSource};

const TIMEOUT_SECS: u64 = 30;

pub struct Blockbook {
    base_url: String,
}

impl Blockbook {
    /// `base_url` is the Blockbook root, e.g. `http://127.0.0.1:8080/indexer/mainnet`.
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    async fn get(&self, path: &str) -> Result<Option<Value>, HeaderError> {
        let url = format!("{}{path}", self.base_url);
        tokio::task::spawn_blocking(move || {
            let response = bitreq::get(&url)
                .with_timeout(TIMEOUT_SECS)
                .send()
                .map_err(|error| HeaderError::Request(format!("{url}: {error}")))?;
            let body = response
                .as_str()
                .map_err(|error| HeaderError::Malformed(format!("{url}: {error}")))?;
            match response.status_code {
                200 => serde_json::from_str(body)
                    .map(Some)
                    .map_err(|error| HeaderError::Malformed(format!("{url}: {error}"))),
                // Blockbook answers 400 for an unknown block.
                400 | 404 => Ok(None),
                status => {
                    let body: String = body.chars().take(200).collect();
                    Err(HeaderError::Request(format!(
                        "{url}: HTTP {status}: {body}"
                    )))
                }
            }
        })
        .await
        .map_err(|error| HeaderError::Request(error.to_string()))?
    }

    async fn block(&self, id: &str) -> Result<Option<Header>, HeaderError> {
        // Only the header fields are used; one transaction per page keeps the response small.
        let Some(block) = self.get(&format!("/api/v2/block/{id}?pageSize=1")).await? else {
            return Ok(None);
        };
        parse_header(&block).map(Some)
    }
}

#[async_trait]
impl HeaderSource for Blockbook {
    async fn tip(&self) -> Result<Header, HeaderError> {
        let status = self
            // `/api` redirects to `/api/` without the proxy's path prefix.
            .get("/api/")
            .await?
            .ok_or_else(|| HeaderError::Request("no Blockbook status".into()))?;
        let height = number(&status["blockbook"]["bestHeight"], "bestHeight")?;
        let tip = self
            .block(&height.to_string())
            .await?
            .ok_or_else(|| HeaderError::Request(format!("tip {height} is not available")))?;
        if tip.height != height {
            return Err(HeaderError::Malformed(format!(
                "asked for block {height}, got {}",
                tip.height
            )));
        }
        Ok(tip)
    }

    async fn header(&self, hash: BlockHash) -> Result<Option<Header>, HeaderError> {
        // Blockbook reads an all-digit hash as a height and answers with that block.
        Ok(self
            .block(&hash.to_string())
            .await?
            .filter(|header| header.hash() == hash))
    }
}

fn parse_header(block: &Value) -> Result<Header, HeaderError> {
    let hash: BlockHash = parse_str(&block["hash"], "hash")?;
    let prev_blockhash = match block.get("previousBlockHash") {
        Some(Value::String(hash)) => parse(hash, "previousBlockHash")?,
        _ => BlockHash::all_zeros(),
    };
    let bits = block["bits"]
        .as_str()
        .ok_or_else(|| malformed("bits"))
        .and_then(|bits| CompactTarget::from_unprefixed_hex(bits).map_err(|_| malformed("bits")))?;
    let header = Header {
        height: number(&block["height"], "height")?,
        header: block::Header {
            version: block::Version::from_consensus(
                i32::try_from(number(&block["version"], "version")?)
                    .map_err(|_| malformed("version"))?,
            ),
            prev_blockhash,
            merkle_root: parse_str::<TxMerkleNode>(&block["merkleRoot"], "merkleRoot")?,
            time: u32::try_from(number(&block["time"], "time")?).map_err(|_| malformed("time"))?,
            bits,
            nonce: u32::try_from(number(&block["nonce"], "nonce")?)
                .map_err(|_| malformed("nonce"))?,
        },
    };
    // The real hash must follow from the parsed fields, or one of them was read wrong.
    if header.hash() != hash {
        return Err(HeaderError::Malformed(format!(
            "header fields of block {hash} hash to {}",
            header.hash()
        )));
    }
    Ok(header)
}

/// Blockbook sends some numbers as strings.
fn number(value: &Value, field: &str) -> Result<u64, HeaderError> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| malformed(field))
}

fn parse_str<T: std::str::FromStr>(value: &Value, field: &str) -> Result<T, HeaderError> {
    parse(value.as_str().ok_or_else(|| malformed(field))?, field)
}

fn parse<T: std::str::FromStr>(text: &str, field: &str) -> Result<T, HeaderError> {
    text.parse().map_err(|_| malformed(field))
}

fn malformed(field: &str) -> HeaderError {
    HeaderError::Malformed(format!("missing or invalid {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Mainnet block 840000 as Blockbook reports it, minus the transactions.
    fn block_840000() -> Value {
        json!({
            "hash": "0000000000000000000320283a032748cef8227873ff4872689bf23f1cda83a5",
            "previousBlockHash": "0000000000000000000172014ba58d66455762add0512355ad651207918494ab",
            "height": 840000,
            "version": 710926336,
            "merkleRoot": "031b417c3a1828ddf3d6527fc210daafcc9218e81f98257f88d4d43bd7a5894f",
            "time": 1713571767,
            "nonce": "3932395645",
            "bits": "17034219",
            "difficulty": "86388558925171.02",
            "txCount": 3050
        })
    }

    #[test]
    fn parses_a_real_header() {
        let header = parse_header(&block_840000()).unwrap();
        assert_eq!(header.height, 840_000);
        assert_eq!(
            header.hash().to_string(),
            "0000000000000000000320283a032748cef8227873ff4872689bf23f1cda83a5"
        );
    }

    #[test]
    fn rejects_fields_that_do_not_hash_to_the_block() {
        let mut block = block_840000();
        block["nonce"] = json!(1);
        assert!(matches!(
            parse_header(&block),
            Err(HeaderError::Malformed(_))
        ));
    }

    /// Against a live Blockbook: `BLOCKBOOK_URL=http://127.0.0.1:8080/indexer/mainnet
    /// cargo test -p btc-overlay -- --ignored`
    fn live() -> Blockbook {
        Blockbook::new(&std::env::var("BLOCKBOOK_URL").expect("BLOCKBOOK_URL is set"))
    }

    #[tokio::test]
    #[ignore = "needs BLOCKBOOK_URL"]
    async fn live_header() {
        let hash = "0000000000000000000320283a032748cef8227873ff4872689bf23f1cda83a5";
        let header = live().header(hash.parse().unwrap()).await.unwrap().unwrap();
        assert_eq!(header.height, 840_000);
        let unknown = BlockHash::all_zeros();
        assert!(live().header(unknown).await.unwrap().is_none());
    }

    #[tokio::test]
    #[ignore = "needs BLOCKBOOK_URL"]
    async fn live_tip_links_to_its_parent() {
        let tip = live().tip().await.unwrap();
        let parent = live()
            .header(tip.header.prev_blockhash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(parent.height + 1, tip.height);
    }
}
