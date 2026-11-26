use crate::block_processing::BtcBlockRecords;
use btc_integration::watcher::BlockId;
use std::future::Future;

pub trait BlockProcessor: Send + Sync + 'static {
    type Error: Send + 'static;

    fn apply(
        &self,
        block: BtcBlockRecords,
    ) -> impl Future<Output = Result<BlockId, Self::Error>> + Send;
    fn resume(&self) -> impl Future<Output = Result<BlockId, Self::Error>> + Send;
    fn rollback(&self, tip: BlockId) -> impl Future<Output = Result<BlockId, Self::Error>> + Send;
}
