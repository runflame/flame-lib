use std::future::Future;

use flamechain::{Block, BlockHash};

pub trait ChainStorage {
    type Error;

    fn add_block(&self, block: &Block) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn get_block(
        &self,
        hash: BlockHash,
    ) -> impl Future<Output = Result<Option<Block>, Self::Error>> + Send;
}
