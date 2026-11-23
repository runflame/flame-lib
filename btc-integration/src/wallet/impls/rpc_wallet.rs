//! An ordinary wallet of Bitcoin Core over RPC.

use bitcoin::consensus::encode;
use bitcoin::{Amount, Transaction, TxOut, Txid, absolute, transaction};
use corepc_client::types::v31::SignRawTransactionWithWallet;
use serde_json::json;

use super::rpc_client::{BtcFundOptions, RpcClient, no_answer};
use crate::BitcoinConfig;
use crate::wallet::{BtcWallet, WalletError};

/// [`BtcWallet`]: a wallet of Bitcoin Core that holds its keys and its coins.
pub struct RpcWallet {
    client: RpcClient,
}

impl RpcWallet {
    /// The wallet `wallet` of the node. No connection is made here: an
    /// unreachable node or a wallet it does not have shows up as an error of
    /// the first call.
    pub fn new(config: &BitcoinConfig, wallet: &str) -> Result<Self, WalletError> {
        Ok(RpcWallet {
            client: RpcClient::new(&config.wallet_rpc_url(wallet), config.auth.clone())?,
        })
    }

    /// `signrawtransactionwithwallet`.
    async fn sign(&self, transaction: &Transaction) -> Result<Transaction, WalletError> {
        let hex = encode::serialize_hex(transaction);
        let signed: SignRawTransactionWithWallet = self
            .client
            .call("signrawtransactionwithwallet", &[json!(hex)])
            .await?;
        if !signed.complete {
            return Err(WalletError::Refused(format!(
                "the wallet did not sign every input: {:?}",
                signed.errors
            )));
        }
        encode::deserialize_hex(&signed.hex).map_err(no_answer)
    }
}

impl BtcWallet for RpcWallet {
    async fn send(&self, output: TxOut) -> Result<Txid, WalletError> {
        // An output that cannot be spent is a burn. Bitcoin Core sends it
        // only when it is told how much may be burned.
        let max_burn = if output.script_pubkey.is_op_return() {
            output.value
        } else {
            Amount::ZERO
        };
        // Bitcoin Core picks the coins and adds the change.
        let transaction = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![output],
        };
        let funded = self
            .client
            .fund_raw_transaction(&transaction, &BtcFundOptions::default())
            .await?;
        let signed = self.sign(&funded).await?;
        self.client.send_raw_transaction(&signed, max_burn).await
    }
}
