//! Bitcoin Core RPC abstractions.

use corepc_client::{
    bitcoin::{BlockHash, Transaction, Txid},
    client_sync::{Auth, Error, Result, v31::Client},
    types::v31::WaitForNewBlock,
};
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockTip {
    pub hash: BlockHash,
    pub height: u64,
}

pub trait RpcApi {
    fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>>;

    fn transactions_at_height(&self, height: u64) -> Result<(BlockHash, Vec<Transaction>)>;

    fn wait_for_next_block(&self, prev_block: BlockTip) -> Result<BlockTip>;

    fn publish_transaction(&self, signed_transaction: &Transaction) -> Result<Txid>;
}

#[derive(Debug)]
pub struct Core31RpcApi {
    client: Client,
}

impl Core31RpcApi {
    pub fn new(url: &str, auth: Auth) -> Result<Self> {
        Ok(Self {
            client: Client::new_with_auth(url, auth)?,
        })
    }
}

impl RpcApi for Core31RpcApi {
    fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>> {
        Ok(self.client.get_block(block_hash)?.txdata)
    }

    fn transactions_at_height(&self, height: u64) -> Result<(BlockHash, Vec<Transaction>)> {
        let block_hash = self.client.get_block_hash(height)?.block_hash()?;
        let transactions = self.transactions_in_block(block_hash)?;

        Ok((block_hash, transactions))
    }

    fn wait_for_next_block(&self, prev_block: BlockTip) -> Result<BlockTip> {
        loop {
            // Keep the server-side wait below corepc-client's 60-second HTTP timeout.
            // Passing current_tip prevents missing a tip change between requests.
            let response: WaitForNewBlock = self.client.call(
                "waitfornewblock",
                &[json!(45_000), json!(prev_block.hash.to_string())],
            )?;
            let hash = response.hash.parse::<BlockHash>()?;

            if hash != prev_block.hash {
                let height =
                    u64::try_from(response.height).map_err(|_| Error::UnexpectedStructure)?;
                return Ok(BlockTip { hash, height });
            }
        }
    }

    fn publish_transaction(&self, signed_transaction: &Transaction) -> Result<Txid> {
        Ok(self
            .client
            .send_raw_transaction(signed_transaction)?
            .txid()?)
    }
}
