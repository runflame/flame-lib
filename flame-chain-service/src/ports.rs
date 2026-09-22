use std::future::Future;

use flamechain::{Block, BlockHash, BlockTip};

use crate::chain::{ChainPath, ChangesOutcome, ImportOutcome};

pub trait ChainAccess {
    type Error;

    fn set_as_child(
        &self,
        parent: BlockTip,
        child: BlockTip,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_block(
        &self,
        tip: BlockTip,
    ) -> impl Future<Output = Result<Option<Block>, Self::Error>> + Send;

    fn get_chain_path(
        &self,
        from: BlockTip,
        to: BlockTip,
    ) -> impl Future<Output = Result<ChainPath, Self::Error>> + Send;

    fn import_block(
        &mut self,
        block: Block,
    ) -> impl Future<Output = Result<ImportOutcome, Self::Error>> + Send;

    fn select_tip(
        &mut self,
        tip: BlockHash,
    ) -> impl Future<Output = Result<ChangesOutcome, Self::Error>> + Send;
}

pub trait CoreBlockSource {
    type Error;

    fn get_core_block(
        &self,
        height: u64,
    ) -> impl Future<Output = Result<Option<Block>, Self::Error>> + Send;

    fn wait_core_block(
        &self,
        height: u64,
    ) -> impl Future<Output = Result<Option<Block>, Self::Error>> + Send;
}
