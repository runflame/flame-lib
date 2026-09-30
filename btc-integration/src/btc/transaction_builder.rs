use crate::btc::rpc::{
    BtcFundOptions, BtcFundedTransaction, BtcFundingInput, BtcUnspentOutput, RpcApi,
};
use corepc_client::{
    bitcoin::{OutPoint, ScriptBuf, Sequence, Transaction, TxIn, Witness},
    client_sync::{Error, Result},
};
use std::sync::Arc;

/// Prepares funded transactions
///
/// Inputs are not reserved. Callers must serialize the full funding, signing and
/// publishing workflow (as MintingSender does).
pub struct BitcoinTransactionBuilder<R: ?Sized> {
    rpc: Arc<R>,
}

impl<R: ?Sized> BitcoinTransactionBuilder<R> {
    pub fn new(rpc: Arc<R>) -> Self {
        Self { rpc }
    }
}

impl<R: RpcApi + ?Sized> BitcoinTransactionBuilder<R> {
    pub async fn fund_and_sign_with_wallet(
        &self,
        transaction: &Transaction,
    ) -> Result<Transaction> {
        let mut transaction = transaction.clone();
        if transaction.input.is_empty() {
            // Seed the template so Core can decode it before funding: zero-input raw
            // transactions have special/ambiguous encoding around the SegWit marker.
            let utxo = self
                .rpc
                .list_unspent()
                .await?
                .into_iter()
                .find(|utxo| utxo.spendable && utxo.safe)
                .ok_or_else(|| Error::Returned("wallet has no spendable UTXOs".to_owned()))?;
            transaction.input.push(unsigned_input(utxo.outpoint));
        }
        let funded = self
            .rpc
            .fund_raw_transaction(&transaction, &BtcFundOptions::default())
            .await?;
        let signed = self.rpc.sign_raw_transaction_with_wallet(&funded).await?;
        if !signed.complete {
            return Err(Error::Returned(format!(
                "wallet did not completely sign the transaction: {:?}",
                signed.errors
            )));
        }
        Ok(signed.transaction)
    }

    /// Select confirmed outputs belonging to the script and return change to it.
    /// Inputs are not reserved. The caller supplies signatures.
    pub async fn fund_transaction_from_script(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
        input_weight: u64,
    ) -> Result<BtcFundedTransaction> {
        if !transaction.input.is_empty() {
            return Err(Error::Returned(
                "custom-script funding requires a transaction without inputs".to_owned(),
            ));
        }
        let utxos = self.confirmed_utxos_for_script(script_pubkey).await?;
        let change_address = get_change_address(&utxos)?;

        let mut last_error = None;
        for count in 1..=utxos.len() {
            let selected = &utxos[..count];
            let outpoints = selected
                .iter()
                .map(|utxo| utxo.outpoint)
                .collect::<Vec<_>>();
            let candidate = add_inputs(transaction, &outpoints);
            let options = script_funding_options(&outpoints, change_address, input_weight);

            let funded = match self.rpc.fund_raw_transaction(&candidate, &options).await {
                Ok(funded) => funded,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            return validate_and_attach_prevouts(funded, selected);
        }
        Err(last_error.unwrap_or_else(|| {
            Error::Returned("wallet has no UTXOs for the requested script".to_owned())
        }))
    }

    async fn confirmed_utxos_for_script(
        &self,
        script_pubkey: &ScriptBuf,
    ) -> Result<Vec<BtcUnspentOutput>> {
        let mut utxos = self
            .rpc
            .list_unspent()
            .await?
            .into_iter()
            .filter(|utxo| {
                utxo.confirmations > 0 && utxo.safe && utxo.prevout.script_pubkey == *script_pubkey
            })
            .collect::<Vec<_>>();
        utxos.sort_by(|left, right| right.prevout.value.cmp(&left.prevout.value));
        Ok(utxos)
    }
}

fn get_change_address(utxos: &[BtcUnspentOutput]) -> Result<&String> {
    Ok(&utxos
        .first()
        .ok_or_else(|| {
            Error::Returned(
                "wallet has no confirmed safe UTXOs for the requested script".to_owned(),
            )
        })?
        .address)
}

fn add_inputs(transaction: &Transaction, outpoints: &[OutPoint]) -> Transaction {
    let mut candidate = transaction.clone();
    candidate
        .input
        .extend(outpoints.iter().copied().map(unsigned_input));
    candidate
}

fn script_funding_options(
    outpoints: &[OutPoint],
    change_address: &str,
    input_weight: u64,
) -> BtcFundOptions {
    BtcFundOptions {
        add_inputs: Some(false),
        change_address: Some(change_address.to_owned()),
        include_watching: Some(true),
        input_weights: outpoints
            .iter()
            .map(|outpoint| (*outpoint, input_weight))
            .collect(),
    }
}

fn validate_and_attach_prevouts(
    transaction: Transaction,
    selected: &[BtcUnspentOutput],
) -> Result<BtcFundedTransaction> {
    let exact_inputs = transaction.input.len() == selected.len()
        && selected.iter().all(|utxo| {
            transaction
                .input
                .iter()
                .filter(|input| input.previous_output == utxo.outpoint)
                .count()
                == 1
        });
    if !exact_inputs {
        return Err(Error::UnexpectedStructure);
    }
    let inputs = transaction
        .input
        .iter()
        .map(|input| {
            selected
                .iter()
                .find(|utxo| utxo.outpoint == input.previous_output)
                .map(|utxo| BtcFundingInput {
                    outpoint: utxo.outpoint,
                    prevout: utxo.prevout.clone(),
                })
                .ok_or(Error::UnexpectedStructure)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(BtcFundedTransaction {
        transaction,
        inputs,
    })
}

fn unsigned_input(previous_output: OutPoint) -> TxIn {
    TxIn {
        previous_output,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    }
}
