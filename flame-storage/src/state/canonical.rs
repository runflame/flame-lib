use std::future::Future;

use btc_integration::BtcBlockTip;
use flamechain::{BlockHash, BlockTip, Blockchain};

pub trait CanonicalStorage {
    type Error;

    fn get_tip(&self) -> impl Future<Output = Result<Option<BlockTip>, Self::Error>> + Send;

    fn store_btc_cursor(
        &self,
        cursor: BtcBlockTip,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_state(
        &self,
    ) -> impl Future<Output = Result<Option<(BlockHash, Blockchain)>, Self::Error>> + Send;
    fn commit_state(
        &self,
        tip: BlockHash,
        state: &Blockchain,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
