use btc_integration::{MintingSendError, Sender, TransactionSigner, btc::rpc::SenderRpc};
use corepc_client::bitcoin::Txid;
use flamechain::BlockHash;
use std::future::Future;

pub trait EndorsementSender: Send + Sync + 'static {
    type Error: Send;
    type TransactionId: Send + Sync;

    fn send_endorsement(
        &self,
        _height: u32,
        _hash: BlockHash,
    ) -> impl Future<Output = Result<Self::TransactionId, Self::Error>> + Send {
        async { unimplemented!() }
    }
}

impl<R, S> EndorsementSender for Sender<R, S>
where
    R: SenderRpc + ?Sized + 'static,
    S: TransactionSigner + ?Sized + 'static,
{
    type Error = MintingSendError<S::Error>;
    type TransactionId = Txid;

    async fn send_endorsement(&self, height: u32, hash: BlockHash) -> Result<Txid, Self::Error> {
        self.send_endorsement(height, hash).await
    }
}
