use std::sync::Arc;

use async_trait::async_trait;
use corepc_client::{bitcoin::Transaction, client_sync::Error as RpcError};
use flamevm::Predicate;

use crate::btc::{bitcoin_facade::BitcoinFacade, rpc::SenderRpc};
use crate::protocol::{AcquisitionData, MinterIdentity};

use super::{
    EndorsementSigner, EndorsementSignerError, SecretStorage, SignTransactionRequest,
    TransactionSigner,
};

/// Regtest signer: Core wallet funding for acquisitions, P2WSH signing for endorsements.
pub struct TestSigner<R: ?Sized, S> {
    acquisition_wallet: Arc<BitcoinFacade<R>>,
    endorsement: EndorsementSigner<R, S>,
    access_predicate: Predicate,
}

impl<R: ?Sized, S> TestSigner<R, S> {
    pub(crate) fn new(
        acquisition_wallet: Arc<BitcoinFacade<R>>,
        endorsement: EndorsementSigner<R, S>,
        access_predicate: Predicate,
    ) -> Self {
        Self {
            acquisition_wallet,
            endorsement,
            access_predicate,
        }
    }

    pub(super) fn acquisition_data(&self, minter: &MinterIdentity) -> AcquisitionData {
        AcquisitionData::new(minter.p2wsh(), self.access_predicate.clone())
    }
}

#[async_trait]
impl<R: SenderRpc + ?Sized, S: SecretStorage> TransactionSigner for TestSigner<R, S> {
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
        Ok(self.endorsement.fund_and_sign(request).await?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TestSignerError<E: std::error::Error + Send + Sync + 'static> {
    #[error("acquisition wallet RPC error: {0}")]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Endorsement(#[from] EndorsementSignerError<E>),
}
