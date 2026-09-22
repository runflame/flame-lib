use std::future::Future;

use btc_integration::BtcBlockTip;
use flamechain::{BlockHash, BlockTip};

pub trait CanonicalStorage {
    type State;
    type Error;

    fn get_tip(&self) -> impl Future<Output = Result<Option<BlockTip>, Self::Error>> + Send;

    fn store_btc_cursor(
        &self,
        cursor: BtcBlockTip,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_state(
        &self,
    ) -> impl Future<Output = Result<Option<(BlockHash, Self::State)>, Self::Error>> + Send;
    fn commit_state(
        &self,
        tip: BlockHash,
        state: &Self::State,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
