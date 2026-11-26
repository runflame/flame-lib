use bitcoin::secp256k1::Message;
use bitcoin::sighash::{EcdsaSighashType, SighashCache};
use bitcoin::{ScriptBuf, Transaction, TxOut, Txid, Witness, absolute, ecdsa, transaction};

use crate::identity::IdentityKey;
use crate::protocol::{MinterIdentity, minter_witness_script};
use crate::wallet::{BtcFundedTransaction, BtcWallet, WalletError, WatchOnlyWallet};

/// The calls of [`BtcWallet::send`] must not overlap. The watch-only wallet
/// does not reserve the outputs it picked: two transactions built at the same
/// time spend the same output, and Bitcoin takes one of them.
pub struct MinterWallet<W, K> {
    watch_only: W,
    key: K,
    minter: MinterIdentity,
}

impl<W: WatchOnlyWallet, K: IdentityKey> MinterWallet<W, K> {
    pub fn new(watch_only: W, key: K, minter: MinterIdentity) -> Result<Self, WalletError> {
        let in_script = minter_witness_script::parse(minter.witness_script())
            .ok()
            .and_then(|script| minter_witness_script::single_key_public_key(script.authorization));
        if in_script != Some(key.public_key()) {
            return Err(WalletError::WrongKey);
        }
        Ok(MinterWallet {
            watch_only,
            key,
            minter,
        })
    }

    pub fn minter(&self) -> &MinterIdentity {
        &self.minter
    }

    async fn sign(&self, funded: BtcFundedTransaction) -> Result<Transaction, WalletError> {
        let witness_script = self.minter.witness_script();
        let mut signed = funded.transaction;
        for (index, input) in funded.inputs.iter().enumerate() {
            let sighash = SighashCache::new(&signed)
                .p2wsh_signature_hash(
                    index,
                    witness_script,
                    input.prevout.value,
                    EcdsaSighashType::All,
                )
                .map_err(|_| WalletError::UnexpectedTransaction)?;
            let signature = self.key.sign(Message::from(sighash)).await?;
            let signature = ecdsa::Signature::sighash_all(signature);
            signed.input[index].witness =
                Witness::from_slice(&[signature.to_vec(), witness_script.to_bytes()]);
        }
        Ok(signed)
    }
}

impl<W: WatchOnlyWallet, K: IdentityKey> BtcWallet for MinterWallet<W, K> {
    async fn send(&self, output: TxOut) -> Result<Txid, WalletError> {
        let script_pubkey = self.minter.witness_script().to_p2wsh();
        let unsigned = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![output],
        };
        let funded = self.watch_only.fund_from_script(
            &unsigned,
            &script_pubkey,
            minter_witness_script::INPUT_WEIGHT,
        );
        let funded = funded.await?;
        check_funded(&unsigned, &funded, &script_pubkey)?;
        let signed = self.sign(funded).await?;
        self.watch_only.publish(&signed).await
    }
}

/// The key signs what another process built, so the transaction is checked
/// first: it has the output that was asked for, it spends outputs of the
/// minter script only, and the rest goes back to that script. Without this a
/// wallet that went wrong could make the minter sign a second endorsement.
fn check_funded(
    unsigned: &Transaction,
    funded: &BtcFundedTransaction,
    script_pubkey: &ScriptBuf,
) -> Result<(), WalletError> {
    let transaction = &funded.transaction;
    let asked = &unsigned.output[0];
    let same_header =
        transaction.version == unsigned.version && transaction.lock_time == unsigned.lock_time;
    let spends_the_script = !transaction.input.is_empty()
        && transaction.input.len() == funded.inputs.len()
        && transaction
            .input
            .iter()
            .zip(&funded.inputs)
            .all(|(input, spent)| {
                input.previous_output == spent.outpoint
                    && spent.prevout.script_pubkey == *script_pubkey
            });
    let pays_what_was_asked = transaction.output.len() <= 2
        && transaction
            .output
            .iter()
            .filter(|output| *output == asked)
            .count()
            == 1
        && transaction
            .output
            .iter()
            .all(|output| output == asked || output.script_pubkey == *script_pubkey);
    if same_header && spends_the_script && pays_what_was_asked {
        Ok(())
    } else {
        Err(WalletError::UnexpectedTransaction)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::SecretKey;
    use bitcoin::{Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid};
    use flamevm::Predicate;
    use futures::FutureExt;

    use super::MinterWallet;
    use crate::identity::IdentityConfig;
    use crate::identity::impls::LocalKey;
    use crate::protocol::MinterIdentity;
    use crate::wallet::{
        BtcFundedTransaction, BtcFundingInput, BtcWallet, WalletError, WatchOnlyWallet,
    };

    /// A watch-only wallet that funds a transaction with the outputs the test
    /// gave it, whatever was asked, and writes down what it is asked.
    struct Recording {
        outputs: Vec<TxOut>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl WatchOnlyWallet for &Recording {
        async fn fund_from_script(
            &self,
            transaction: &Transaction,
            script_pubkey: &ScriptBuf,
            _input_weight: u64,
        ) -> Result<BtcFundedTransaction, WalletError> {
            self.calls.lock().unwrap().push("fund");
            let spent = BtcFundingInput {
                outpoint: OutPoint::new(Txid::all_zeros(), 0),
                prevout: TxOut {
                    value: Amount::from_sat(10_000),
                    script_pubkey: script_pubkey.clone(),
                },
            };
            let mut funded = transaction.clone();
            funded.input.push(TxIn {
                previous_output: spent.outpoint,
                ..TxIn::default()
            });
            funded.output = self.outputs.clone();
            Ok(BtcFundedTransaction {
                transaction: funded,
                inputs: vec![spent],
            })
        }

        async fn publish(&self, transaction: &Transaction) -> Result<Txid, WalletError> {
            self.calls.lock().unwrap().push("publish");
            Ok(transaction.compute_txid())
        }
    }

    /// The key of a minter and its identity, the same on every call.
    fn minter() -> (LocalKey, MinterIdentity) {
        let key = LocalKey::new(SecretKey::from_slice(&[0x41; 32]).unwrap());
        let nobody = Predicate::opaque(Predicate::unspendable_key());
        let minter = IdentityConfig {
            flame_predicate: nobody.clone(),
            access_predicate: nobody,
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        }
        .identity(&key);
        (key, minter)
    }

    /// Sends `asked` through the wallet of the minter, over a watch-only
    /// wallet that funds it with `outputs`. Returns what came of it and what
    /// the watch-only wallet was asked.
    fn send(asked: &TxOut, outputs: Vec<TxOut>) -> (Result<Txid, WalletError>, Vec<&'static str>) {
        let watch_only = Recording {
            outputs,
            calls: Mutex::default(),
        };
        let (key, minter) = minter();
        let wallet = MinterWallet::new(&watch_only, key, minter).unwrap();
        let sent = wallet.send(asked.clone()).now_or_never();
        let sent = sent.expect("the wallet answers at once");
        let calls = watch_only.calls.lock().unwrap().clone();
        (sent, calls)
    }

    fn data(byte: u8) -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return([byte]),
        }
    }

    #[test]
    fn a_wallet_signs_only_the_transaction_it_asked_for() {
        let asked = data(1);
        let pays_to = |script_pubkey| TxOut {
            value: Amount::from_sat(9_000),
            script_pubkey,
        };
        let change = pays_to(minter().1.witness_script().to_p2wsh());
        let stranger = pays_to(ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()));

        // The output that was asked for and the change to the minter script:
        // the transaction is signed and sent.
        let (sent, calls) = send(&asked, vec![asked.clone(), change.clone()]);
        assert!(sent.is_ok());
        assert_eq!(calls, ["fund", "publish"]);

        let not_asked_for = [
            // One more data output: a second endorsement, say.
            vec![asked.clone(), data(2)],
            // Another output in place of the one that was asked for.
            vec![data(2), change.clone()],
            // The output that was asked for is gone.
            vec![change],
            // It is there twice.
            vec![asked.clone(), asked.clone()],
            // The change goes to somebody else.
            vec![asked.clone(), stranger],
        ];
        for outputs in not_asked_for {
            let (sent, calls) = send(&asked, outputs);
            assert_eq!(sent, Err(WalletError::UnexpectedTransaction));
            assert_eq!(calls, ["fund"]);
        }
    }
}
