use std::sync::Arc;

use async_trait::async_trait;
use corepc_client::{bitcoin::Transaction, client_sync::Error as RpcError};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;

use crate::btc::{bitcoin_facade::BitcoinFacade, rpc::RpcApi};
use crate::protocol::{AcquisitionData, MinterIdentity};

use super::{
    SecretStorage, SignTransactionRequest, TransactionSigner, VoteSigner, VoteSignerError,
};

/// Regtest signer: Core wallet funding for acquisitions, P2WSH signing for votes.
pub struct TestSigner<R: ?Sized, S> {
    acquisition_wallet: Arc<BitcoinFacade<R>>,
    vote: VoteSigner<R, S>,
    access_predicate: Predicate,
    validator_pubkey: VerifyingKey,
}

impl<R: ?Sized, S> TestSigner<R, S> {
    pub(crate) fn new(
        acquisition_wallet: Arc<BitcoinFacade<R>>,
        vote: VoteSigner<R, S>,
        access_predicate: Predicate,
        validator_pubkey: VerifyingKey,
    ) -> Self {
        Self {
            acquisition_wallet,
            vote,
            access_predicate,
            validator_pubkey,
        }
    }

    pub(super) fn acquisition_data(&self, minter: &MinterIdentity) -> AcquisitionData {
        AcquisitionData::new(
            minter.p2wsh(),
            self.access_predicate.clone(),
            self.validator_pubkey,
        )
    }
}

#[async_trait]
impl<R: RpcApi + ?Sized, S: SecretStorage> TransactionSigner for TestSigner<R, S> {
    type Error = TestSignerError<S::Error>;

    async fn fund_and_sign(
        &self,
        request: SignTransactionRequest,
    ) -> Result<Transaction, Self::Error> {
        if request.required_inputs.is_empty() {
            return Ok(self
                .acquisition_wallet
                .fund_and_sign_with_wallet(&request.transaction)
                .await?);
        }
        Ok(self.vote.fund_and_sign(request).await?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TestSignerError<E: std::error::Error + Send + Sync + 'static> {
    #[error("acquisition wallet RPC error: {0}")]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Vote(#[from] VoteSignerError<E>),
}
