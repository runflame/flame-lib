//! Bitcoin Core RPC abstractions.

use std::sync::Arc;

use async_trait::async_trait;
use corepc_client::{
    bitcoin::{Amount, BlockHash, OutPoint, Transaction, TxOut, Txid, consensus::encode},
    client_sync::{Auth, Error, Result, v31::Client},
    types::v31::{FundRawTransaction, SendRawTransaction, WaitForNewBlock},
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcTransactionWithPrevouts {
    pub transaction: Transaction,
    pub prevouts: Vec<Option<TxOut>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcFundingInput {
    pub outpoint: OutPoint,
    pub prevout: TxOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcFundedTransaction {
    pub transaction: Transaction,
    /// Previous outputs aligned with `transaction.input`.
    pub inputs: Vec<BtcFundingInput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcUnspentOutput {
    pub outpoint: OutPoint,
    pub prevout: TxOut,
    pub address: String,
    pub confirmations: u64,
    pub spendable: bool,
    pub safe: bool,
}

#[derive(Clone, Debug, Default)]
pub struct BtcFundOptions {
    pub add_inputs: Option<bool>,
    pub change_address: Option<String>,
    pub include_watching: Option<bool>,
    pub input_weights: Vec<(OutPoint, u64)>,
}

#[derive(Clone, Debug)]
pub struct BtcSignedTransaction {
    pub transaction: Transaction,
    pub complete: bool,
    pub errors: Vec<String>,
}

/// Each method performs one Bitcoin Core RPC call and converts its arguments/result.
/// Coin selection, retries and operations combining calls belong to BitcoinFacade.
#[async_trait]
pub trait RpcApi: Send + Sync {
    async fn best_block_tip(&self) -> Result<BtcBlockTip>;
    async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo>;
    async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>>;
    async fn transactions_with_prevouts_in_block(
        &self,
        block_hash: BlockHash,
    ) -> Result<Vec<BtcTransactionWithPrevouts>>;
    async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash>;
    /// A single long-poll request; a timeout may return the unchanged tip.
    async fn wait_for_new_block(
        &self,
        current_tip: BlockHash,
        timeout_ms: u64,
    ) -> Result<BtcBlockTip>;
    async fn send_raw_transaction(
        &self,
        transaction: &Transaction,
        max_burn_amount: Option<Amount>,
    ) -> Result<Txid>;
    async fn list_unspent(&self) -> Result<Vec<BtcUnspentOutput>>;
    async fn fund_raw_transaction(
        &self,
        transaction: &Transaction,
        options: &BtcFundOptions,
    ) -> Result<Transaction>;
    async fn sign_raw_transaction_with_wallet(
        &self,
        transaction: &Transaction,
    ) -> Result<BtcSignedTransaction>;
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

    pub(crate) async fn network(&self) -> Result<corepc_client::bitcoin::Network> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let info = client.get_blockchain_info()?;
            corepc_client::bitcoin::Network::from_core_arg(&info.chain)
                .map_err(|error| Error::Returned(format!("invalid Bitcoin network: {error}")))
        })
        .await
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

    async fn transactions_with_prevouts_in_block(
        &self,
        block_hash: BlockHash,
    ) -> Result<Vec<BtcTransactionWithPrevouts>> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let block = client
                .get_block_verbose_three(block_hash)?
                .into_model()
                .map_err(|error| {
                    Error::Returned(format!("failed to decode verbose Bitcoin block: {error}"))
                })?;

            block
                .tx
                .into_iter()
                .map(|entry| {
                    let transaction = entry.transaction.transaction;
                    let prevouts = entry
                        .prevouts
                        .into_iter()
                        .map(|prevout| {
                            prevout.map(|prevout| TxOut {
                                value: prevout.value,
                                script_pubkey: prevout.script_pubkey.script_pubkey,
                            })
                        })
                        .collect::<Vec<_>>();
                    if prevouts.len() != transaction.input.len() {
                        return Err(Error::UnexpectedStructure);
                    }

                    Ok(BtcTransactionWithPrevouts {
                        transaction,
                        prevouts,
                    })
                })
                .collect()
        })
        .await
    }

    async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || Ok(client.get_block_hash(height)?.block_hash()?)).await
    }

    async fn wait_for_new_block(
        &self,
        current_tip: BlockHash,
        timeout_ms: u64,
    ) -> Result<BtcBlockTip> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let response: WaitForNewBlock = client.call(
                "waitfornewblock",
                &[json!(timeout_ms), json!(current_tip.to_string())],
            )?;
            Ok(BtcBlockTip {
                hash: response.hash.parse()?,
                height: u64::try_from(response.height).map_err(|_| Error::UnexpectedStructure)?,
            })
        })
        .await
    }

    async fn send_raw_transaction(
        &self,
        transaction: &Transaction,
        max_burn_amount: Option<Amount>,
    ) -> Result<Txid> {
        let client = Arc::clone(&self.client);
        let hex = encode::serialize_hex(transaction);
        spawn_core_call(move || {
            let mut args = vec![json!(hex)];
            if let Some(amount) = max_burn_amount {
                args.extend([serde_json::Value::Null, json!(amount.to_btc())]);
            }
            let response: SendRawTransaction = client.call("sendrawtransaction", &args)?;
            Ok(response.txid()?)
        })
        .await
    }

    async fn list_unspent(&self) -> Result<Vec<BtcUnspentOutput>> {
        let client = Arc::clone(&self.client);
        spawn_core_call(move || {
            let response = client.list_unspent()?.into_model().map_err(|error| {
                Error::Returned(format!("failed to decode listunspent response: {error}"))
            })?;
            Ok(response
                .0
                .into_iter()
                .map(|utxo| BtcUnspentOutput {
                    outpoint: OutPoint::new(utxo.txid, utxo.vout),
                    prevout: TxOut {
                        value: utxo.amount,
                        script_pubkey: utxo.script_pubkey,
                    },
                    address: utxo.address.assume_checked().to_string(),
                    confirmations: u64::from(utxo.confirmations),
                    spendable: utxo.spendable,
                    safe: utxo.safe,
                })
                .collect())
        })
        .await
    }

    async fn fund_raw_transaction(
        &self,
        transaction: &Transaction,
        options: &BtcFundOptions,
    ) -> Result<Transaction> {
        let client = Arc::clone(&self.client);
        let hex = encode::serialize_hex(transaction);
        let mut args = serde_json::Map::new();
        if let Some(value) = options.add_inputs {
            args.insert("add_inputs".into(), json!(value));
        }
        if let Some(value) = &options.change_address {
            args.insert("changeAddress".into(), json!(value));
        }
        if let Some(value) = options.include_watching {
            args.insert("includeWatching".into(), json!(value));
        }
        if !options.input_weights.is_empty() {
            args.insert("input_weights".into(), json!(options.input_weights.iter().map(|(outpoint, weight)| {
                json!({ "txid": outpoint.txid.to_string(), "vout": outpoint.vout, "weight": weight })
            }).collect::<Vec<_>>()));
        }
        spawn_core_call(move || {
            let response: FundRawTransaction =
                client.call("fundrawtransaction", &[json!(hex), args.into()])?;
            Ok(encode::deserialize_hex(&response.hex)?)
        })
        .await
    }

    async fn sign_raw_transaction_with_wallet(
        &self,
        transaction: &Transaction,
    ) -> Result<BtcSignedTransaction> {
        let client = Arc::clone(&self.client);
        let transaction = transaction.clone();
        spawn_core_call(move || {
            let response = client.sign_raw_transaction_with_wallet(&transaction)?;
            Ok(BtcSignedTransaction {
                transaction: encode::deserialize_hex(&response.hex)?,
                complete: response.complete,
                errors: response
                    .errors
                    .unwrap_or_default()
                    .into_iter()
                    .map(|error| format!("{error:?}"))
                    .collect(),
            })
        })
        .await
    }
}

pub(crate) fn rpc_error_code(error: &Error) -> Option<i32> {
    match error {
        Error::JsonRpc(jsonrpc::Error::Rpc(error)) => Some(error.code),
        _ => None,
    }
}
