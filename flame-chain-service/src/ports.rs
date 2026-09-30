use std::{future::Future, sync::Arc};

use flamechain::{Block, BlockTip};

use crate::chain::{ChainPath, ImportOutcome};

pub trait ChainAccess {
    type Error;

    fn get_block(
        &self,
        tip: BlockTip,
    ) -> impl Future<Output = Result<Option<Arc<Block>>, Self::Error>> + Send;

    fn get_chain_path(
        &self,
        from: BlockTip,
        to: BlockTip,
    ) -> impl Future<Output = Result<ChainPath, Self::Error>> + Send;

    fn import_block(
        &mut self,
        block: Arc<Block>,
    ) -> impl Future<Output = Result<ImportOutcome, Self::Error>> + Send;
}

pub trait CoreBlockSource {
    type Error;

    fn get_core_block(
        &self,
        target_height: u64,
    ) -> impl Future<Output = Result<Option<Arc<Block>>, Self::Error>> + Send;

    fn wait_core_block(
        &self,
        height: u64,
    ) -> impl Future<Output = Result<Option<Arc<Block>>, Self::Error>> + Send;
}
