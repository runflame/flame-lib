use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use bitcoin::Block;
use bitcoin::block::Header;

use crate::watcher::index::BlockIndex;
use crate::watcher::storage::{BtcStorage, StorageError};
use crate::watcher::types::{BlockId, Prevouts};

pub const DEFAULT_RECENT_BLOCKS: usize = 30;

#[derive(Default)]
struct Recent {
    blocks: HashMap<BlockId, (Block, Prevouts)>,
    order: VecDeque<BlockId>,
}

#[derive(Clone)]
pub struct RecentBlocks {
    recent: Arc<Mutex<Recent>>,
    capacity: usize,
}

impl RecentBlocks {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "a storage has to keep at least one block");
        RecentBlocks {
            recent: Arc::default(),
            capacity,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Recent> {
        self.recent
            .lock()
            .expect("the storage lock is not poisoned")
    }

    pub fn len(&self) -> usize {
        self.lock().blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().blocks.is_empty()
    }

    pub fn contains(&self, id: BlockId) -> bool {
        self.lock().blocks.contains_key(&id)
    }
}

impl Default for RecentBlocks {
    fn default() -> Self {
        RecentBlocks::new(DEFAULT_RECENT_BLOCKS)
    }
}

impl BlockIndex for RecentBlocks {
    type Error = StorageError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, StorageError> {
        Ok(self.lock().blocks.get(&id).map(|(block, _)| block.header))
    }
}

impl BtcStorage for RecentBlocks {
    async fn block(&self, id: BlockId) -> Result<Option<Block>, StorageError> {
        let recent = self.lock();
        Ok(recent.blocks.get(&id).map(|(block, _)| block.clone()))
    }

    async fn prevouts(&self, id: BlockId) -> Result<Option<Prevouts>, StorageError> {
        let recent = self.lock();
        Ok(recent.blocks.get(&id).map(|(_, prevouts)| prevouts.clone()))
    }

    async fn put(&self, id: BlockId, block: Block, prevouts: Prevouts) -> Result<(), StorageError> {
        let mut recent = self.lock();
        if recent.blocks.insert(id, (block, prevouts)).is_some() {
            return Ok(());
        }
        recent.order.push_back(id);
        if recent.order.len() > self.capacity {
            let first = recent.order.pop_front().expect("the order is not empty");
            recent.blocks.remove(&first);
        }
        Ok(())
    }

    async fn remove_many(&self, ids: &[BlockId]) -> Result<(), StorageError> {
        let mut recent = self.lock();
        for id in ids {
            if recent.blocks.remove(id).is_some() {
                recent.order.retain(|stored| stored != id);
            }
        }
        Ok(())
    }

    fn capacity(&self) -> Option<usize> {
        Some(self.capacity)
    }
}
