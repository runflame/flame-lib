use crate::btc::bitcoin_facade::BitcoinFacade;
use std::{error::Error, sync::Arc};

use corepc_client::{
    bitcoin::{Amount, Transaction, TxOut, Txid, absolute, transaction},
    client_sync::Error as RpcError,
};
use flamechain::BlockHash;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::btc::rpc::RpcApi;
use crate::protocol::{
    minter_identity::MinterIdentity,
    sender::signer::{RequiredInput, SignTransactionRequest, TransactionSigner},
    vote::{MintingVoteAuth, MintingVoteData, MintingVoteOutput},
};
use crate::{Acquisition, AcquisitionData, SecretStorage, TestSigner, TestSignerError, VoteSigner};

pub struct Sender<R: ?Sized, S: ?Sized> {
    pub(super) rpc: Arc<BitcoinFacade<R>>,
    pub(super) signer: Arc<S>,
    pub(super) minter: MinterIdentity,
    pub(super) sending: Mutex<()>,
}

impl<R: ?Sized, S: ?Sized> Sender<R, S> {
    pub(crate) fn new(rpc: Arc<BitcoinFacade<R>>, signer: Arc<S>, minter: MinterIdentity) -> Self {
        Self {
            rpc,
            signer,
            minter,
            sending: Mutex::new(()),
        }
    }

    pub const fn minter(&self) -> &MinterIdentity {
        &self.minter
    }
}

impl<R, S> Sender<R, S>
where
    R: RpcApi + ?Sized,
    S: TransactionSigner + ?Sized,
{
    /// Sends vote to the bitcoin network
    pub async fn send_vote(
        &self,
        height: u32,
        hash: BlockHash,
    ) -> Result<Txid, MintingSendError<S::Error>> {
        let _sending = self.sending.lock().await;
        let expected = MintingVoteData::V1 {
            flame_block_height: height,
            flame_block_hash: hash,
        };
        let unsigned = unsigned_transaction(TxOut {
            value: Amount::ZERO,
            script_pubkey: expected.to_script(),
        });
        let request = SignTransactionRequest::new(unsigned).requiring(RequiredInput::P2wsh {
            witness_script: self.minter.witness_script().clone(),
        });
        let signed = self
            .signer
            .fund_and_sign(request)
            .await
            .map_err(MintingSendError::Signing)?;

        validate_signed_vote(&signed, &expected, &self.minter)?;

        self.rpc
            .publish_transaction_with_burn(&signed, Amount::ZERO)
            .await
            .map_err(MintingSendError::Rpc)
    }
}

/// A sender supporting only Minting votes.
pub type VoteSender<R, S> = Sender<R, VoteSigner<R, S>>;

/// A regtest sender supporting both `send_vote` and `send_acquisition`.
pub type TestSender<R, S> = Sender<R, TestSigner<R, S>>;

impl<R: RpcApi + ?Sized, S: SecretStorage> Sender<R, TestSigner<R, S>> {
    pub async fn send_acquisition(
        &self,
        amount: Amount,
    ) -> Result<Txid, MintingSendError<TestSignerError<S::Error>>> {
        if amount == Amount::ZERO {
            return Err(MintingSendError::ZeroAcquisitionAmount);
        }
        let _sending = self.sending.lock().await;
        let expected = self.signer.acquisition_data(&self.minter);
        let unsigned = unsigned_transaction(TxOut {
            value: amount,
            script_pubkey: expected.to_script(),
        });
        let signed = self
            .signer
            .fund_and_sign(SignTransactionRequest::new(unsigned))
            .await
            .map_err(MintingSendError::Signing)?;
        validate_signed_acquisition(&signed, amount, &expected)?;
        self.rpc
            .publish_transaction_with_burn(&signed, amount)
            .await
            .map_err(MintingSendError::Rpc)
    }
}

pub(super) fn unsigned_transaction(output: TxOut) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: Vec::new(),
        output: vec![output],
    }
}

fn validate_signed_acquisition(
    transaction: &Transaction,
    expected_amount: Amount,
    expected_data: &AcquisitionData,
) -> Result<(), SignerContractViolation> {
    validate_transaction_header(transaction)?;
    if transaction.input.is_empty() {
        return Err(SignerContractViolation::NoInputs);
    }

    let acquisitions = Acquisition::from_tx(transaction);
    match acquisitions.as_slice() {
        [] => Err(SignerContractViolation::MissingAcquisitionOutput),
        [acquisition]
            if acquisition.amount() == expected_amount && acquisition.data() == expected_data =>
        {
            Ok(())
        }
        [_] => Err(SignerContractViolation::ChangedAcquisitionOutput),
        _ => Err(SignerContractViolation::MultipleAcquisitionOutputs),
    }
}

fn validate_signed_vote(
    transaction: &Transaction,
    expected_data: &MintingVoteData,
    expected_minter: &MinterIdentity,
) -> Result<(), SignerContractViolation> {
    validate_transaction_header(transaction)?;
    if transaction.input.is_empty() {
        return Err(SignerContractViolation::NoInputs);
    }

    let outputs = transaction
        .output
        .iter()
        .enumerate()
        .filter_map(|(index, output)| MintingVoteOutput::from_output(index, output))
        .collect::<Vec<_>>();
    match outputs.as_slice() {
        [] => return Err(SignerContractViolation::MissingVoteOutput),
        [output]
            if output.data == *expected_data
                && transaction.output[output.output_index as usize].value == Amount::ZERO => {}
        [_] => return Err(SignerContractViolation::ChangedVoteOutput),
        _ => return Err(SignerContractViolation::MultipleVoteOutputs),
    }

    let mut found_expected_minter = false;
    for (input_index, input) in transaction.input.iter().enumerate() {
        let Some(auth) = MintingVoteAuth::from_input(input_index, input) else {
            continue;
        };

        if auth.minter() != expected_minter {
            return Err(SignerContractViolation::UnexpectedMinterAuthentication { input_index });
        }
        found_expected_minter = true;
    }

    if !found_expected_minter {
        return Err(SignerContractViolation::MissingMinterAuthentication);
    }

    Ok(())
}

pub(super) fn validate_transaction_header(
    transaction: &Transaction,
) -> Result<(), SignerContractViolation> {
    if transaction.version != transaction::Version::TWO
        || transaction.lock_time != absolute::LockTime::ZERO
    {
        return Err(SignerContractViolation::ChangedTransactionHeader);
    }
    Ok(())
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SignerContractViolation {
    #[error("transaction signer changed the transaction version or lock time")]
    ChangedTransactionHeader,
    #[error("transaction signer returned a transaction without inputs")]
    NoInputs,
    #[error("transaction signer removed the requested Acquisition output")]
    MissingAcquisitionOutput,
    #[error("transaction signer changed the requested Acquisition output")]
    ChangedAcquisitionOutput,
    #[error("transaction signer added another Acquisition output")]
    MultipleAcquisitionOutputs,
    #[error("transaction signer removed the requested Minting vote output")]
    MissingVoteOutput,
    #[error("transaction signer changed the requested Minting vote output")]
    ChangedVoteOutput,
    #[error("transaction signer added another Minting vote output")]
    MultipleVoteOutputs,
    #[error("transaction signer did not add the required Minter P2WSH authentication")]
    MissingMinterAuthentication,
    #[error("input {input_index} contains authentication for an unexpected Minter")]
    UnexpectedMinterAuthentication { input_index: usize },
}

#[derive(Debug, Error)]
pub enum MintingSendError<E>
where
    E: Error + Send + Sync + 'static,
{
    #[error("Acquisition amount must be greater than zero")]
    ZeroAcquisitionAmount,
    #[error("transaction signing failed: {0}")]
    Signing(#[source] E),
    #[error(transparent)]
    SignerContract(#[from] SignerContractViolation),
    #[error("Bitcoin Core RPC error: {0}")]
    Rpc(#[source] RpcError),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use corepc_client::{
        bitcoin::{
            Amount, BlockHash as BitcoinBlockHash, OutPoint, ScriptBuf, Sequence, Transaction,
            TxIn, TxOut, Txid, Witness,
        },
        client_sync::Result as RpcResult,
    };
    use flamechain::BlockHash;
    use flamevm::Predicate;
    use thiserror::Error;

    use super::{MintingSendError, Sender, SignerContractViolation};
    use crate::btc::bitcoin_facade::BitcoinFacade;
    use crate::{
        btc::rpc::{BtcBlockHeaderInfo, BtcBlockTip, RpcApi},
        protocol::{
            MinterIdentity, MintingVoteData, RequiredInput, SignTransactionRequest,
            TransactionSigner, minter_witness_script,
        },
    };

    #[derive(Clone, Copy)]
    enum SignerMode {
        Valid,
        MissingAuthentication,
        RemoveProtocolOutput,
    }

    struct RecordingSigner {
        mode: SignerMode,
        requests: Mutex<Vec<SignTransactionRequest>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl RecordingSigner {
        fn new(mode: SignerMode) -> Self {
            Self {
                mode,
                requests: Mutex::new(Vec::new()),
                events: Arc::default(),
            }
        }
    }

    #[derive(Debug, Error)]
    #[error("test signer failed")]
    struct TestSignerError;

    #[async_trait]
    impl TransactionSigner for RecordingSigner {
        type Error = TestSignerError;

        async fn fund_and_sign(
            &self,
            request: SignTransactionRequest,
        ) -> Result<Transaction, Self::Error> {
            self.events.lock().unwrap().push("sign");
            tokio::task::yield_now().await;
            self.requests.lock().unwrap().push(request.clone());

            let mut transaction = request.transaction;
            let witness = match (self.mode, request.required_inputs.first()) {
                (SignerMode::Valid, Some(RequiredInput::P2wsh { witness_script })) => {
                    Witness::from_slice(&[vec![0x30, 0x01], witness_script.as_bytes().to_vec()])
                }
                _ => Witness::new(),
            };
            transaction.input.push(TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness,
            });
            if matches!(self.mode, SignerMode::RemoveProtocolOutput) {
                transaction.output.clear();
            } else {
                transaction.output.push(TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new(),
                });
            }
            self.events.lock().unwrap().push("signed");
            Ok(transaction)
        }
    }

    #[derive(Default)]
    struct RecordingRpc {
        published: Mutex<Vec<(Transaction, Amount)>>,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_next_publish: AtomicBool,
    }

    #[async_trait]
    impl RpcApi for RecordingRpc {
        async fn transactions_with_prevouts_in_block(
            &self,
            _: BitcoinBlockHash,
        ) -> RpcResult<Vec<crate::btc::rpc::BtcTransactionWithPrevouts>> {
            unreachable!("not used by sender tests")
        }

        async fn list_unspent(
            &self,
        ) -> corepc_client::client_sync::Result<Vec<crate::btc::rpc::BtcUnspentOutput>> {
            unreachable!("wallet RPC not used by this test")
        }
        async fn fund_raw_transaction(
            &self,
            _: &Transaction,
            _: &crate::btc::rpc::BtcFundOptions,
        ) -> corepc_client::client_sync::Result<Transaction> {
            unreachable!("wallet RPC not used by this test")
        }
        async fn sign_raw_transaction_with_wallet(
            &self,
            _: &Transaction,
        ) -> corepc_client::client_sync::Result<crate::btc::rpc::BtcSignedTransaction> {
            unreachable!("wallet RPC not used by this test")
        }

        async fn best_block_tip(&self) -> RpcResult<BtcBlockTip> {
            unreachable!("not used by sender tests")
        }

        async fn block_header_info(
            &self,
            _block_hash: BitcoinBlockHash,
        ) -> RpcResult<BtcBlockHeaderInfo> {
            unreachable!("not used by sender tests")
        }

        async fn transactions_in_block(
            &self,
            _block_hash: BitcoinBlockHash,
        ) -> RpcResult<Vec<Transaction>> {
            unreachable!("not used by sender tests")
        }

        async fn block_hash_at_height(&self, _: u64) -> RpcResult<BitcoinBlockHash> {
            unreachable!("not used by sender tests")
        }

        async fn wait_for_new_block(&self, _: BitcoinBlockHash, _: u64) -> RpcResult<BtcBlockTip> {
            unreachable!("not used by sender tests")
        }

        async fn send_raw_transaction(
            &self,
            transaction: &Transaction,
            max_burn_amount: Option<Amount>,
        ) -> RpcResult<Txid> {
            self.events.lock().unwrap().push("publish");
            tokio::task::yield_now().await;
            if self.fail_next_publish.swap(false, Ordering::SeqCst) {
                return Err(corepc_client::client_sync::Error::Returned(
                    "test publish failed".into(),
                ));
            }
            self.published
                .lock()
                .unwrap()
                .push((transaction.clone(), max_burn_amount.unwrap_or(Amount::ZERO)));
            self.events.lock().unwrap().push("published");
            Ok(transaction.compute_txid())
        }
    }

    #[tokio::test]
    async fn vote_requires_minter_p2wsh_then_publishes_without_a_burn_allowance() {
        let rpc = Arc::new(RecordingRpc::default());
        let signer = Arc::new(RecordingSigner::new(SignerMode::Valid));
        let script = witness_script(&predicate());
        let sender = Sender::new(
            Arc::new(BitcoinFacade::new(Arc::clone(&rpc))),
            Arc::clone(&signer),
            MinterIdentity::new(script.clone()).expect("valid Minter identity"),
        );
        let hash = BlockHash::from([0x44; 32]);

        sender.send_vote(123, hash).await.expect("send vote");

        let requests = signer.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].transaction.input.is_empty());
        assert_eq!(
            requests[0].required_inputs,
            vec![RequiredInput::P2wsh {
                witness_script: script,
            }]
        );
        assert_eq!(
            requests[0].transaction.output[0].script_pubkey,
            MintingVoteData::V1 {
                flame_block_height: 123,
                flame_block_hash: hash,
            }
            .to_script()
        );
        drop(requests);

        let published = rpc.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].1, Amount::ZERO);
    }

    #[tokio::test]
    async fn rejects_a_vote_without_the_required_authentication_before_rpc() {
        let rpc = Arc::new(RecordingRpc::default());
        let signer = Arc::new(RecordingSigner::new(SignerMode::MissingAuthentication));
        let sender = Sender::new(
            Arc::new(BitcoinFacade::new(Arc::clone(&rpc))),
            signer,
            minter_identity(&predicate()),
        );

        let error = sender
            .send_vote(123, BlockHash::from([0x44; 32]))
            .await
            .expect_err("missing authentication must fail");

        assert!(matches!(
            error,
            MintingSendError::SignerContract(SignerContractViolation::MissingMinterAuthentication)
        ));
        assert!(rpc.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_a_removed_vote_output_before_rpc() {
        let rpc = Arc::new(RecordingRpc::default());
        let signer = Arc::new(RecordingSigner::new(SignerMode::RemoveProtocolOutput));
        let sender = Sender::new(
            Arc::new(BitcoinFacade::new(Arc::clone(&rpc))),
            signer,
            minter_identity(&predicate()),
        );

        let error = sender
            .send_vote(125, BlockHash::from([0x66; 32]))
            .await
            .expect_err("removed vote must fail");

        assert!(matches!(
            error,
            MintingSendError::SignerContract(SignerContractViolation::MissingVoteOutput)
        ));
        assert!(rpc.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn votes_are_serialized_through_publication() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let rpc = Arc::new(RecordingRpc {
            events: Arc::clone(&events),
            ..Default::default()
        });
        let signer = Arc::new(RecordingSigner {
            events: Arc::clone(&events),
            ..RecordingSigner::new(SignerMode::Valid)
        });
        let sender = Sender::new(
            Arc::new(BitcoinFacade::new(Arc::clone(&rpc))),
            signer,
            minter_identity(&predicate()),
        );

        let results = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(
                sender.send_vote(125, BlockHash::from([0x66; 32])),
                sender.send_vote(123, BlockHash::from([0x44; 32])),
                sender.send_vote(124, BlockHash::from([0x55; 32])),
                sender.send_vote(126, BlockHash::from([0x77; 32])),
            )
        })
        .await
        .expect("send queue must make progress");
        results.0.unwrap();
        results.1.unwrap();
        results.2.unwrap();
        results.3.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["sign", "signed", "publish", "published"].repeat(4),
        );
        assert_eq!(rpc.published.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn publish_error_releases_the_send_queue() {
        let rpc = Arc::new(RecordingRpc {
            fail_next_publish: AtomicBool::new(true),
            ..Default::default()
        });
        let sender = Sender::new(
            Arc::new(BitcoinFacade::new(Arc::clone(&rpc))),
            Arc::new(RecordingSigner::new(SignerMode::Valid)),
            minter_identity(&predicate()),
        );
        let (failed, next) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(
                biased;
                sender.send_vote(125, BlockHash::from([0x66; 32])),
                sender.send_vote(123, BlockHash::from([0x44; 32])),
            )
        })
        .await
        .expect("publish error must release the queue");
        assert!(matches!(failed, Err(MintingSendError::Rpc(_))));
        next.unwrap();
        assert_eq!(rpc.published.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn validation_error_releases_the_send_queue() {
        let rpc = Arc::new(RecordingRpc::default());
        let sender = Sender::new(
            Arc::new(BitcoinFacade::new(Arc::clone(&rpc))),
            Arc::new(RecordingSigner::new(SignerMode::MissingAuthentication)),
            minter_identity(&predicate()),
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let failed = sender.send_vote(123, BlockHash::from([0x44; 32])).await;
            assert!(matches!(failed, Err(MintingSendError::SignerContract(_))));
            let next = sender.send_vote(124, BlockHash::from([0x55; 32])).await;
            assert!(matches!(next, Err(MintingSendError::SignerContract(_))));
        })
        .await
        .expect("validation error must release the queue");
        assert!(rpc.published.lock().unwrap().is_empty());
    }

    fn predicate() -> Predicate {
        Predicate::opaque(Predicate::unspendable_key())
    }

    fn minter_identity(predicate: &Predicate) -> MinterIdentity {
        MinterIdentity::new(witness_script(predicate)).expect("valid Minter identity")
    }

    fn witness_script(predicate: &Predicate) -> ScriptBuf {
        minter_witness_script::build_with_authorization(
            predicate,
            corepc_client::bitcoin::Script::from_bytes(&[0x51]),
        )
    }
}
