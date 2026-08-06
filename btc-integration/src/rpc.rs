//! Bitcoin Core RPC abstractions.

use std::sync::Arc;

use async_trait::async_trait;
use corepc_client::{
    bitcoin::{
        Amount, BlockHash, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, Txid, Witness,
        consensus::encode,
    },
    client_sync::{Auth, Error, Result, v31::Client},
    types::v31::{SendRawTransaction, WaitForNewBlock},
};
use serde_json::json;
use tokio::task;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BtcBlockTip {
    pub hash: BlockHash,
    pub height: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BtcBlockHeaderInfo {
    pub tip: BtcBlockTip,
    pub previous_block_hash: Option<BlockHash>,
}

#[async_trait]
pub trait RpcApi: Send + Sync {
    async fn best_block_tip(&self) -> Result<BtcBlockTip>;

    async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo>;

    async fn previous_header(&self, header: BtcBlockHeaderInfo) -> Result<BtcBlockHeaderInfo> {
        let previous_hash = header.previous_block_hash.ok_or_else(|| {
            Error::Returned(format!(
                "block {:?} does not have a previous block",
                header.tip
            ))
        })?;

        self.block_header_info(previous_hash).await
    }

    async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>>;

    async fn transactions_at_height(&self, height: u64) -> Result<(BlockHash, Vec<Transaction>)>;

    async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash>;

    async fn wait_for_next_block(&self, prev_block: BtcBlockTip) -> Result<BtcBlockTip>;

    async fn publish_transaction(&self, signed_transaction: &Transaction) -> Result<Txid>;

    async fn fund_and_sign_transaction(&self, transaction: &Transaction) -> Result<Transaction>;

    async fn publish_mint_transaction(
        &self,
        transaction: &Transaction,
        max_burn_amount: Amount,
    ) -> Result<Txid>;
}

#[derive(Debug)]
pub struct Core31RpcApi {
    client: Arc<Client>,
}

impl Core31RpcApi {
    pub fn new(url: &str, auth: Auth) -> Result<Self> {
        Ok(Self {
            client: Arc::new(Client::new_with_auth(url, auth)?),
        })
    }
}

async fn spawn_core_call<T, F>(call: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    task::spawn_blocking(call)
        .await
        .map_err(|error| Error::Returned(format!("Bitcoin Core RPC task failed: {error}")))?
}

#[async_trait]
impl RpcApi for Core31RpcApi {
    async fn best_block_tip(&self) -> Result<BtcBlockTip> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let info = client.get_blockchain_info()?;
            let height = u64::try_from(info.blocks).map_err(|_| Error::UnexpectedStructure)?;

            Ok(BtcBlockTip {
                hash: info.best_block_hash.parse()?,
                height,
            })
        })
        .await
    }

    async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let info = client.get_block_header_verbose(&block_hash)?;
            let height = u64::try_from(info.height).map_err(|_| Error::UnexpectedStructure)?;

            Ok(BtcBlockHeaderInfo {
                tip: BtcBlockTip {
                    hash: info.hash.parse()?,
                    height,
                },
                previous_block_hash: info
                    .previous_block_hash
                    .map(|hash| hash.parse())
                    .transpose()?,
            })
        })
        .await
    }

    async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || Ok(client.get_block(block_hash)?.txdata)).await
    }

    async fn transactions_at_height(&self, height: u64) -> Result<(BlockHash, Vec<Transaction>)> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let block_hash = client.get_block_hash(height)?.block_hash()?;
            let transactions = client.get_block(block_hash)?.txdata;

            Ok((block_hash, transactions))
        })
        .await
    }

    async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || Ok(client.get_block_hash(height)?.block_hash()?)).await
    }

    async fn wait_for_next_block(&self, prev_block: BtcBlockTip) -> Result<BtcBlockTip> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            loop {
                // Keep the server-side wait below corepc-client's 60-second HTTP timeout.
                // Passing current_tip prevents missing a tip change between requests.
                let response: WaitForNewBlock = client.call(
                    "waitfornewblock",
                    &[json!(45_000), json!(prev_block.hash.to_string())],
                )?;
                let hash = response.hash.parse::<BlockHash>()?;

                if hash != prev_block.hash {
                    let height =
                        u64::try_from(response.height).map_err(|_| Error::UnexpectedStructure)?;
                    return Ok(BtcBlockTip { hash, height });
                }
            }
        })
        .await
    }

    async fn publish_transaction(&self, signed_transaction: &Transaction) -> Result<Txid> {
        let client = Arc::clone(&self.client);
        let signed_transaction = signed_transaction.clone();
        spawn_core_call(move || Ok(client.send_raw_transaction(&signed_transaction)?.txid()?)).await
    }

    async fn fund_and_sign_transaction(&self, transaction: &Transaction) -> Result<Transaction> {
        let client = Arc::clone(&self.client);
        let mut transaction = transaction.clone();
        spawn_core_call(move || {
            if transaction.input.is_empty() {
                let utxo = client
                    .list_unspent()?
                    .0
                    .into_iter()
                    .find(|utxo| utxo.spendable && utxo.safe)
                    .ok_or_else(|| Error::Returned("wallet has no spendable UTXOs".to_owned()))?;
                let vout = u32::try_from(utxo.vout).map_err(|_| Error::UnexpectedStructure)?;

                transaction.input.push(TxIn {
                    previous_output: OutPoint::new(utxo.txid.parse()?, vout),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::default(),
                });
            }

            let funded = client.fund_raw_transaction(&transaction)?;
            let funded_transaction = encode::deserialize_hex(&funded.hex)?;
            let signed = client.sign_raw_transaction_with_wallet(&funded_transaction)?;

            if !signed.complete {
                return Err(Error::Returned(format!(
                    "wallet did not completely sign the transaction: {:?}",
                    signed.errors.unwrap_or_default()
                )));
            }

            Ok(encode::deserialize_hex(&signed.hex)?)
        })
        .await
    }

    async fn publish_mint_transaction(
        &self,
        transaction: &Transaction,
        max_burn_amount: Amount,
    ) -> Result<Txid> {
        let client = Arc::clone(&self.client);
        let transaction_hex = encode::serialize_hex(transaction);
        spawn_core_call(move || {
            // Bitcoin Node has default value of max_burn_amount=0, so we need to pass this
            // parameter explicitly. corepc-client wrapper does not support this parameter,
            // so we need to call .call() manually.
            let response: SendRawTransaction = client.call(
                "sendrawtransaction",
                &[
                    transaction_hex.into(),
                    serde_json::Value::Null,
                    max_burn_amount.to_btc().into(),
                ],
            )?;

            Ok(response.txid()?)
        })
        .await
    }
}
