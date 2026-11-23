//! A wallet over Bitcoin in memory.

use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
use bitcoin::sighash::{EcdsaSighashType, SighashCache};
use bitcoin::{
    Amount, CompressedPublicKey, ScriptBuf, Transaction, TxIn, TxOut, Txid, Witness, absolute,
    ecdsa, transaction,
};

use crate::wallet::{
    BtcFundedTransaction, BtcFundingInput, BtcWallet, WalletError, WatchOnlyWallet,
};
use crate::watcher::impls::MemoryBtc;

/// What the wallet pays for a transaction.
// TODO: a fee by the weight of the transaction, as a real wallet pays.
const FEE: Amount = Amount::from_sat(1_000);

/// A wallet over [`MemoryBtc`], with one key.
///
/// As a [`BtcWallet`] it pays with its own coins: the outputs that pay to its
/// key, [`MemoryWallet::script_pubkey`]. A new wallet has none; a test gives
/// it some with [`MemoryBtc::mine_coins`]. As a [`WatchOnlyWallet`] it finds
/// the outputs of any script and signs nothing.
///
/// It spends confirmed outputs only, and never one that a waiting
/// transaction spends. That is stricter than a wallet of Bitcoin Core, which
/// also spends its own change before a block confirms it.
#[derive(Clone)]
pub struct MemoryWallet {
    btc: MemoryBtc,
    key: SecretKey,
}

impl MemoryWallet {
    pub fn new(btc: MemoryBtc, key: SecretKey) -> Self {
        MemoryWallet { btc, key }
    }

    fn public_key(&self) -> CompressedPublicKey {
        CompressedPublicKey(self.key.public_key(&Secp256k1::signing_only()))
    }

    /// The script the coins of the wallet pay to: P2WPKH of its key.
    pub fn script_pubkey(&self) -> ScriptBuf {
        ScriptBuf::new_p2wpkh(&self.public_key().wpubkey_hash())
    }

    /// Adds inputs that spend confirmed outputs of the script until they pay
    /// for the outputs and the fee. The change goes back to the script.
    fn fund(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
    ) -> Result<BtcFundedTransaction, WalletError> {
        if !transaction.input.is_empty() {
            return Err(WalletError::Refused(
                "funding requires a transaction without inputs".to_owned(),
            ));
        }
        let pays: Amount = transaction.output.iter().map(|output| output.value).sum();
        let mut funded = transaction.clone();
        let mut inputs = Vec::new();
        let mut spends = Amount::ZERO;
        for (outpoint, prevout) in self.btc.unspent(script_pubkey) {
            spends += prevout.value;
            funded.input.push(TxIn {
                previous_output: outpoint,
                ..TxIn::default()
            });
            inputs.push(BtcFundingInput { outpoint, prevout });
            let Some(change) = spends.checked_sub(pays + FEE) else {
                continue;
            };
            if change > Amount::ZERO {
                funded.output.push(TxOut {
                    value: change,
                    script_pubkey: script_pubkey.clone(),
                });
            }
            return Ok(BtcFundedTransaction {
                transaction: funded,
                inputs,
            });
        }
        if inputs.is_empty() {
            return Err(WalletError::NoConfirmedOutputs);
        }
        Err(WalletError::Refused(
            "the confirmed outputs of the script do not pay for the transaction".to_owned(),
        ))
    }

    /// A transaction with `output`, paid with the coins of the wallet and
    /// signed with its key. It is not sent.
    pub fn payment(&self, output: TxOut) -> Result<Transaction, WalletError> {
        let unsigned = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![output],
        };
        let own = self.script_pubkey();
        let funded = self.fund(&unsigned, &own)?;
        let mut signed = funded.transaction;
        let secp = Secp256k1::signing_only();
        let public_key = self.public_key();
        for (index, input) in funded.inputs.iter().enumerate() {
            let sighash = SighashCache::new(&signed)
                .p2wpkh_signature_hash(index, &own, input.prevout.value, EcdsaSighashType::All)
                .map_err(|error| WalletError::Refused(error.to_string()))?;
            let signature = secp.sign_ecdsa(&Message::from(sighash), &self.key);
            let signature = ecdsa::Signature::sighash_all(signature);
            signed.input[index].witness = Witness::p2wpkh(&signature, &public_key.0);
        }
        Ok(signed)
    }

    /// Hands the transaction to Bitcoin, which checks it.
    fn submit(&self, transaction: Transaction) -> Result<Txid, WalletError> {
        self.btc
            .submit(transaction)
            .map_err(|rejected| WalletError::Refused(rejected.to_string()))
    }
}

impl BtcWallet for MemoryWallet {
    async fn send(&self, output: TxOut) -> Result<Txid, WalletError> {
        self.submit(self.payment(output)?)
    }
}

impl WatchOnlyWallet for MemoryWallet {
    async fn fund_from_script(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
        _input_weight: u64,
    ) -> Result<BtcFundedTransaction, WalletError> {
        self.fund(transaction, script_pubkey)
    }

    async fn publish(&self, transaction: &Transaction) -> Result<Txid, WalletError> {
        self.submit(transaction.clone())
    }
}
