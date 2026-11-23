//! The RPC calls the wallets of Bitcoin Core share.

use std::fmt::Display;

use bitcoin::consensus::encode;
use bitcoin::hex::DisplayHex;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use corepc_client::types::v31::{FundRawTransaction, ListUnspent, SendRawTransaction};
use corepc_client::{Auth, Client, Error};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::wallet::WalletError;

/// A client of one RPC endpoint of Bitcoin Core: the node
/// (`http://127.0.0.1:18443`) or one of its wallets
/// (`http://127.0.0.1:18443/wallet/<name>`). Tested with version 31.0.
///
/// The client does its network I/O on tokio, so its futures need a tokio
/// runtime.
pub(super) struct RpcClient {
    client: Client,
}

/// An unspent output, as `listunspent` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BtcUnspentOutput {
    pub outpoint: OutPoint,
    pub prevout: TxOut,
    pub address: String,
    pub confirmations: u64,
    pub safe: bool,
}

/// The options of `fundrawtransaction`.
#[derive(Clone, Debug, Default)]
pub(super) struct BtcFundOptions {
    pub add_inputs: Option<bool>,
    pub change_address: Option<String>,
    pub include_watching: Option<bool>,
    pub input_weights: Vec<(OutPoint, u64)>,
}

impl RpcClient {
    /// No connection is made here: an unreachable node shows up as an error of
    /// the first call.
    pub(super) fn new(url: &str, auth: Auth) -> Result<Self, WalletError> {
        Ok(RpcClient {
            client: Client::new_with_auth(url, auth).map_err(no_answer)?,
        })
    }

    /// One RPC call.
    pub(super) async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        args: &[Value],
    ) -> Result<T, WalletError> {
        let answer = self.client.call(method, args).await;
        answer.map_err(|failure| match failure {
            // Bitcoin Core answered, and its answer is an error.
            Error::JsonRpc(jsonrpc::Error::Rpc(refusal)) => {
                WalletError::Refused(format!("{method}: {}", refusal.message))
            }
            other => WalletError::Unreachable(format!("{method}: {other}")),
        })
    }

    /// `listunspent`.
    pub(super) async fn list_unspent(&self) -> Result<Vec<BtcUnspentOutput>, WalletError> {
        let unspent: ListUnspent = self.call("listunspent", &[]).await?;
        let unspent = unspent.into_model().map_err(no_answer)?;
        Ok(unspent
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
                safe: utxo.safe,
            })
            .collect())
    }

    /// `fundrawtransaction` for a transaction that is not signed yet.
    pub(super) async fn fund_raw_transaction(
        &self,
        transaction: &Transaction,
        options: &BtcFundOptions,
    ) -> Result<Transaction, WalletError> {
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
            let weights = options.input_weights.iter().map(|(outpoint, weight)| {
                json!({ "txid": outpoint.txid.to_string(), "vout": outpoint.vout, "weight": weight })
            });
            args.insert("input_weights".into(), weights.collect());
        }
        // The last argument says that the hex has no segwit marker.
        let args = [json!(unsigned_hex(transaction)), args.into(), json!(false)];
        let funded: FundRawTransaction = self.call("fundrawtransaction", &args).await?;
        encode::deserialize_hex(&funded.hex).map_err(no_answer)
    }

    /// `sendrawtransaction <hex> <maxfeerate> <maxburnamount>`, with the
    /// default fee rate limit. `max_burn` is how much the outputs that cannot
    /// be spent may hold.
    pub(super) async fn send_raw_transaction(
        &self,
        transaction: &Transaction,
        max_burn: Amount,
    ) -> Result<Txid, WalletError> {
        let hex = encode::serialize_hex(transaction);
        let args = [json!(hex), Value::Null, json!(max_burn.to_btc())];
        let sent: SendRawTransaction = self.call("sendrawtransaction", &args).await?;
        sent.txid().map_err(no_answer)
    }
}

/// An unsigned transaction as hex, without the segwit marker.
///
/// `bitcoin` writes a transaction that has no inputs with the marker, and
/// Bitcoin Core does not decode that. An unsigned transaction has no
/// witnesses, so nothing is lost without it.
fn unsigned_hex(transaction: &Transaction) -> String {
    let mut bytes = encode::serialize(&transaction.version);
    bytes.extend(encode::serialize(&transaction.input));
    bytes.extend(encode::serialize(&transaction.output));
    bytes.extend(encode::serialize(&transaction.lock_time));
    bytes.to_lower_hex_string()
}

/// The node cannot be reached, or what it answered cannot be read.
pub(super) fn no_answer(error: impl Display) -> WalletError {
    WalletError::Unreachable(error.to_string())
}

pub(super) fn unsigned_input(previous_output: OutPoint) -> TxIn {
    TxIn {
        previous_output,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    }
}
