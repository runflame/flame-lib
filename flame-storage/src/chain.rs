use std::future::Future;

use flamechain::{Block, BlockTip, CoreBlockHeader, CoreBlockTip};

pub trait ChainStorage {
    type Error;

    fn add_block(&self, block: &Block) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_block(
        &self,
        tip: BlockTip,
    ) -> impl Future<Output = Result<Option<Block>, Self::Error>> + Send;

    fn get_core_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> impl Future<Output = Result<Option<CoreBlockHeader>, Self::Error>> + Send;
}
