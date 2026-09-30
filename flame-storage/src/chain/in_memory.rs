use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::{Arc, RwLock},
};

use flamechain::{Block, BlockHash, BlockHeader, BlockTip, CoreBlockHeader, CoreBlockTip};

use super::ChainStorage;

#[derive(Clone, Default)]
pub struct InMemoryChainStorage {
    state: Arc<RwLock<StorageState>>,
}

#[derive(Default)]
struct StorageState {
    blocks: BTreeMap<BlockHash, Arc<Block>>,
    children: BTreeMap<BlockHash, BTreeSet<BlockHash>>,
}

impl StorageState {
    fn core_header(&self, tip: CoreBlockTip) -> Option<&BlockHeader> {
        self.blocks
            .get(&tip.hash)
            .map(|block| &block.header)
            .filter(|header| {
                header
                    .core_block
                    .as_ref()
                    .is_some_and(|core| core.height == tip.height)
            })
    }

    fn descendants(&self, root: BlockHash) -> Vec<BlockHeader> {
        let mut pending = VecDeque::from([root]);
        let mut descendants = Vec::new();
        while let Some(parent) = pending.pop_front() {
            if let Some(children) = self.children.get(&parent) {
                for hash in children {
                    let block = &self.blocks[hash];
                    descendants.push(block.header.clone());
                    pending.push_back(*hash);
                }
            }
        }
        descendants
    }
}

impl InMemoryChainStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChainStorage for InMemoryChainStorage {
    type Error = InMemoryChainStorageError;

    async fn add_block(&self, block: Arc<Block>) -> Result<(), Self::Error> {
        let hash = block.header.id();
        let mut state = self.state.write().map_err(|_| Self::Error::LockPoisoned)?;
        if state.blocks.contains_key(&hash) {
            return Ok(());
        }
        let parent = block.header.parent;
        state.blocks.insert(hash, block);
        state.children.entry(parent).or_default().insert(hash);
        Ok(())
    }

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Arc<Block>>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state
            .blocks
            .get(&tip.hash)
            .filter(|block| block.header.height == tip.height.as_u64())
            .cloned())
    }

    async fn get_core_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Option<CoreBlockHeader>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state
            .core_header(tip)
            .and_then(|header| header.core_block.clone()))
    }

    async fn get_block_header_by_block_tip(
        &self,
        tip: BlockTip,
    ) -> Result<Option<BlockHeader>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state
            .blocks
            .get(&tip.hash)
            .filter(|block| block.header.height == tip.height.as_u64())
            .map(|block| block.header.clone()))
    }

    async fn get_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Option<BlockHeader>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state.core_header(tip).cloned())
    }

    async fn get_core_block_header_with_core_descendants(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Option<(BlockHeader, Vec<BlockHeader>)>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state
            .core_header(tip)
            .map(|header| (header.clone(), state.descendants(tip.hash))))
    }
}

#[derive(Debug)]
pub enum InMemoryChainStorageError {
    LockPoisoned,
}

impl fmt::Display for InMemoryChainStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "chain storage lock poisoned"),
        }
    }
}

impl std::error::Error for InMemoryChainStorageError {}
