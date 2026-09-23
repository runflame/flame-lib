use std::future::Future;

use flamechain::{Block, BlockHeader, BlockTip, CoreBlockHeader, CoreBlockTip};

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

    fn get_block_header_by_block_tip(
        &self,
        tip: BlockTip,
    ) -> impl Future<Output = Result<Option<BlockHeader>, Self::Error>> + Send;

    fn get_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> impl Future<Output = Result<Option<BlockHeader>, Self::Error>> + Send;

    fn get_core_block_header_with_core_descendants(
        &self,
        tip: CoreBlockTip,
    ) -> impl Future<Output = Result<Option<(BlockHeader, Vec<BlockHeader>)>, Self::Error>> + Send;
}
