use std::{
    convert::Infallible,
    fmt,
    sync::{Arc, RwLock},
};

use flamechain::{Block, BlockTip};

use crate::{ChainAccess, ChainPath, CoreBlockSource, ImportOutcome};

#[derive(Clone, Default)]
pub struct InMemoryChain {
    blocks: Arc<RwLock<Vec<Arc<Block>>>>,
}

impl InMemoryChain {
    async fn parent_tip(&self, tip: BlockTip) -> Result<BlockTip, InMemoryChainError> {
        let height = tip
            .height
            .as_u64()
            .checked_sub(1)
            .ok_or(InMemoryChainError::NoCommonAncestor)?;
        let block = self
            .get_block(tip)
            .await?
            .ok_or(InMemoryChainError::MissingBlock(tip))?;
        Ok(BlockTip {
            hash: block.header.parent,
            height: height.into(),
        })
    }
}

impl ChainAccess for InMemoryChain {
    type Error = InMemoryChainError;

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Arc<Block>>, Self::Error> {
        Ok(self
            .blocks
            .read()
            .unwrap()
            .iter()
            .find(|block| block.header.block_tip() == tip)
            .cloned())
    }

    async fn get_chain_path(&self, from: BlockTip, to: BlockTip) -> Result<ChainPath, Self::Error> {
        let mut path = ChainPath::default();
        let mut from = from;
        let mut to = to;
        while from != to {
            if from.height >= to.height {
                path.detach.push(from);
                from = self.parent_tip(from).await?;
            } else {
                path.attach.push(to);
                to = self.parent_tip(to).await?;
            }
        }
        path.attach.reverse();
        Ok(path)
    }

    async fn import_block(&mut self, block: Arc<Block>) -> Result<ImportOutcome, Self::Error> {
        let hash = block.header.id();
        let mut blocks = self.blocks.write().unwrap();
        if blocks.iter().any(|stored| stored.header.id() == hash) {
            return Ok(ImportOutcome::AlreadyKnown(hash));
        }
        blocks.push(block);
        Ok(ImportOutcome::Imported(hash))
    }
}

impl CoreBlockSource for InMemoryChain {
    type Error = Infallible;

    async fn get_core_block(&self, target_height: u64) -> Result<Option<Arc<Block>>, Self::Error> {
        Ok(self
            .blocks
            .read()
            .unwrap()
            .iter()
            .find(|block| {
                block
                    .header
                    .core_block
                    .as_ref()
                    .is_some_and(|core| u64::from(core.target_btc_height) == target_height)
            })
            .cloned())
    }

    async fn wait_core_block(&self, _: u64) -> Result<Option<Arc<Block>>, Self::Error> {
        unimplemented!("waiting for core blocks is not supported")
    }
}

#[derive(Debug)]
pub enum InMemoryChainError {
    NoCommonAncestor,
    MissingBlock(BlockTip),
}

impl fmt::Display for InMemoryChainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCommonAncestor => write!(formatter, "path has no common ancestor"),
            Self::MissingBlock(tip) => write!(formatter, "missing path block: {tip:?}"),
        }
    }
}

impl std::error::Error for InMemoryChainError {}
