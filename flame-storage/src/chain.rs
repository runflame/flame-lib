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

    /// Returns the full header of the Core Block identified by a vote.
    /// Both the hash and the Core Flame height must match.
    fn get_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> impl Future<Output = Result<Option<BlockHeader>, Self::Error>> + Send;

    /// Returns the header and all known descendants at every depth, including
    /// forks and non-Core blocks. Each descendant appears once; the requested
    /// block is excluded from the descendants. Their order is unspecified.
    /// Returns `None` when the requested block is unknown; a leaf has no children.
    fn get_block_header_with_children(
        &self,
        tip: BlockTip,
    ) -> impl Future<Output = Result<Option<(BlockHeader, Vec<BlockHeader>)>, Self::Error>> + Send;
}
