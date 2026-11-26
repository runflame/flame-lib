use bitcoin::secp256k1::Message;
use bitcoin::sighash::{EcdsaSighashType, SighashCache};
use bitcoin::{ScriptBuf, Transaction, TxOut, Txid, Witness, absolute, ecdsa, transaction};
use tokio::sync::Mutex;

use crate::identity::IdentityKey;
use crate::protocol::{MinterIdentity, minter_witness_script};
use crate::wallet::{BtcFundedTransaction, BtcWallet, WalletError, WatchOnlyWallet};

pub struct MinterWallet<W, K> {
    watch_only: W,
    key: K,
    minter: MinterIdentity,
    sending: Mutex<()>,
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
            sending: Mutex::new(()),
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
        let _sending = self.sending.lock().await;
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::SecretKey;
    use bitcoin::{Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid};
    use flamevm::Predicate;

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
        events: Mutex<Vec<&'static str>>,
        fail_next_publish: AtomicBool,
    }

    impl Recording {
        fn new(outputs: Vec<TxOut>) -> Self {
            Recording {
                outputs,
                events: Mutex::default(),
                fail_next_publish: AtomicBool::new(false),
            }
        }
    }

    impl WatchOnlyWallet for &Recording {
        async fn fund_from_script(
            &self,
            transaction: &Transaction,
            script_pubkey: &ScriptBuf,
            _input_weight: u64,
        ) -> Result<BtcFundedTransaction, WalletError> {
            self.events.lock().unwrap().push("fund");
            tokio::task::yield_now().await;
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
            self.events.lock().unwrap().push("funded");
            Ok(BtcFundedTransaction {
                transaction: funded,
                inputs: vec![spent],
            })
        }

        async fn publish(&self, transaction: &Transaction) -> Result<Txid, WalletError> {
            self.events.lock().unwrap().push("publish");
            tokio::task::yield_now().await;
            if self.fail_next_publish.swap(false, Ordering::SeqCst) {
                return Err(WalletError::Unreachable("test publish failed".into()));
            }
            self.events.lock().unwrap().push("published");
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
    async fn send(
        asked: &TxOut,
        outputs: Vec<TxOut>,
    ) -> (Result<Txid, WalletError>, Vec<&'static str>) {
        let watch_only = Recording::new(outputs);
        let (key, minter) = minter();
        let wallet = MinterWallet::new(&watch_only, key, minter).unwrap();
        let sent = wallet.send(asked.clone()).await;
        let events = watch_only.events.lock().unwrap().clone();
        (sent, events)
    }

    fn data(byte: u8) -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return([byte]),
        }
    }

    fn pays_to(script_pubkey: ScriptBuf) -> TxOut {
        TxOut {
            value: Amount::from_sat(9_000),
            script_pubkey,
        }
    }

    fn change() -> TxOut {
        pays_to(minter().1.witness_script().to_p2wsh())
    }

    #[tokio::test]
    async fn a_wallet_signs_only_the_transaction_it_asked_for() {
        let asked = data(1);
        let change = change();
        let stranger = pays_to(ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()));

        // The output that was asked for and the change to the minter script:
        // the transaction is signed and sent.
        let (sent, events) = send(&asked, vec![asked.clone(), change.clone()]).await;
        assert!(sent.is_ok());
        assert_eq!(events, ["fund", "funded", "publish", "published"]);

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
            let (sent, events) = send(&asked, outputs).await;
            assert_eq!(sent, Err(WalletError::UnexpectedTransaction));
            assert_eq!(events, ["fund", "funded"]);
        }
    }

    #[tokio::test]
    async fn sends_are_serialized_through_publication() {
        let asked = data(1);
        let watch_only = Recording::new(vec![asked.clone(), change()]);
        let (key, minter) = minter();
        let wallet = MinterWallet::new(&watch_only, key, minter).unwrap();

        let results = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(
                wallet.send(asked.clone()),
                wallet.send(asked.clone()),
                wallet.send(asked.clone()),
                wallet.send(asked.clone()),
            )
        })
        .await
        .expect("send queue must make progress");
        results.0.unwrap();
        results.1.unwrap();
        results.2.unwrap();
        results.3.unwrap();
        assert_eq!(
            *watch_only.events.lock().unwrap(),
            ["fund", "funded", "publish", "published"].repeat(4),
        );
    }

    #[tokio::test]
    async fn publish_error_releases_the_send_queue() {
        let asked = data(1);
        let watch_only = Recording {
            fail_next_publish: AtomicBool::new(true),
            ..Recording::new(vec![asked.clone(), change()])
        };
        let (key, minter) = minter();
        let wallet = MinterWallet::new(&watch_only, key, minter).unwrap();

        let (failed, next) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(
                biased;
                wallet.send(asked.clone()),
                wallet.send(asked.clone()),
            )
        })
        .await
        .expect("publish error must release the queue");
        assert!(matches!(failed, Err(WalletError::Unreachable(_))));
        next.unwrap();
        let events = watch_only.events.lock().unwrap();
        let published = events.iter().filter(|event| **event == "published");
        assert_eq!(published.count(), 1);
    }

    #[tokio::test]
    async fn validation_error_releases_the_send_queue() {
        let asked = data(1);
        let watch_only = Recording::new(vec![asked.clone(), data(2)]);
        let (key, minter) = minter();
        let wallet = MinterWallet::new(&watch_only, key, minter).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let failed = wallet.send(asked.clone()).await;
            assert_eq!(failed, Err(WalletError::UnexpectedTransaction));
            let next = wallet.send(asked.clone()).await;
            assert_eq!(next, Err(WalletError::UnexpectedTransaction));
        })
        .await
        .expect("validation error must release the queue");
        assert!(!watch_only.events.lock().unwrap().contains(&"publish"));
    }
}
