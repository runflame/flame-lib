use bitcoin::consensus::encode;
use bitcoin::{Amount, Transaction, TxOut, Txid, absolute, transaction};
use corepc_client::types::v31::SignRawTransactionWithWallet;
use serde_json::json;
use tokio::sync::Mutex;

use super::rpc_client::{BtcFundOptions, RpcClient, no_answer};
use crate::BitcoinConfig;
use crate::wallet::{BtcWallet, WalletError};

pub struct RpcWallet {
    client: RpcClient,
    sending: Mutex<()>,
}

impl RpcWallet {
    pub fn new(config: &BitcoinConfig, wallet: &str) -> Result<Self, WalletError> {
        Ok(RpcWallet {
            client: RpcClient::new(&config.wallet_rpc_url(wallet), config.auth.clone())?,
            sending: Mutex::new(()),
        })
    }

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
        let _sending = self.sending.lock().await;
        // An output that cannot be spent is a burn. Bitcoin Core sends it
        // only when it is told how much may be burned.
        let max_burn = if output.script_pubkey.is_op_return() {
            output.value
        } else {
            Amount::ZERO
        };
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
