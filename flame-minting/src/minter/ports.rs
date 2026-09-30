use std::future::Future;

use btc_integration::{MintingSendError, Sender, TransactionSigner, btc::rpc::RpcApi};
use corepc_client::bitcoin::Txid;
use flamechain::BlockHash;

pub trait VoteSender: Send + Sync + 'static {
    type Error: Send;
    type TransactionId: Send + Sync;

    fn send_vote(
        &self,
        height: u32,
        hash: BlockHash,
    ) -> impl Future<Output = Result<Self::TransactionId, Self::Error>> + Send;
}

impl<R, S> VoteSender for Sender<R, S>
where
    R: RpcApi + ?Sized + 'static,
    S: TransactionSigner + ?Sized + 'static,
{
    type Error = MintingSendError<S::Error>;
    type TransactionId = Txid;

    async fn send_vote(&self, height: u32, hash: BlockHash) -> Result<Txid, Self::Error> {
        self.send_vote(height, hash).await
    }
}
