use bitcoin::Txid;
use btc_integration::{
    BtcWallet, IdentityKey, MinterWallet, MintingEndorsementData, WalletError, WatchOnlyWallet,
};
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

impl<W, K> EndorsementSender for MinterWallet<W, K>
where
    W: WatchOnlyWallet + 'static,
    K: IdentityKey + 'static,
{
    type Error = WalletError;
    type TransactionId = Txid;

    async fn send_endorsement(&self, height: u32, hash: BlockHash) -> Result<Txid, WalletError> {
        let endorsement = MintingEndorsementData::V1 {
            flame_block_height: height,
            flame_block_hash: hash,
        };
        self.send(endorsement.to_output()).await
    }
}
