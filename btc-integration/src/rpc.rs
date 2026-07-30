//! Bitcoin Core RPC abstractions.

use corepc_client::{
    bitcoin::{
        Amount, BlockHash, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, Txid, Witness,
        consensus::encode,
    },
    client_sync::{Auth, Error, Result, v31::Client},
    types::v31::{SendRawTransaction, WaitForNewBlock},
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

    fn fund_and_sign_transaction(&self, transaction: &Transaction) -> Result<Transaction>;

    fn publish_mint_transaction(
        &self,
        transaction: &Transaction,
        max_burn_amount: Amount,
    ) -> Result<Txid>;
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

    fn fund_and_sign_transaction(&self, transaction: &Transaction) -> Result<Transaction> {
        let mut transaction = transaction.clone();

        if transaction.input.is_empty() {
            let utxo = self
                .client
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

        let funded = self.client.fund_raw_transaction(&transaction)?;
        let funded_transaction = encode::deserialize_hex(&funded.hex)?;
        let signed = self
            .client
            .sign_raw_transaction_with_wallet(&funded_transaction)?;

        if !signed.complete {
            return Err(Error::Returned(format!(
                "wallet did not completely sign the transaction: {:?}",
                signed.errors.unwrap_or_default()
            )));
        }

        Ok(encode::deserialize_hex(&signed.hex)?)
    }

    fn publish_mint_transaction(
        &self,
        transaction: &Transaction,
        max_burn_amount: Amount,
    ) -> Result<Txid> {
        let transaction_hex = encode::serialize_hex(transaction);
        // Bitcoin Node has default value of max_burn_amount=0, so we need to pass this parameter
        // explicitly. corepc-client wrapper does not support this parameter, so we need to call
        // .call() manually
        let response: SendRawTransaction = self.client.call(
            "sendrawtransaction",
            &[
                transaction_hex.into(),
                serde_json::Value::Null,
                max_burn_amount.to_btc().into(),
            ],
        )?;

        Ok(response.txid()?)
    }
}
