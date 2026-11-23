//! A storage in memory that keeps only the latest Bitcoin blocks.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use bitcoin::Block;
use bitcoin::block::Header;

use crate::watcher::index::BlockIndex;
use crate::watcher::storage::{BtcStorage, StorageError};
use crate::watcher::types::{BlockId, Prevouts};

/// How many blocks [`RecentBlocks`] keeps by default.
pub const DEFAULT_RECENT_BLOCKS: usize = 30;

#[derive(Default)]
struct Recent {
    blocks: HashMap<BlockId, (Block, Prevouts)>,
    /// The stored blocks in the order they were stored, the first one at the
    /// front.
    order: VecDeque<BlockId>,
}

/// Storage in memory for the latest blocks only. When it is full, a new block
/// pushes out the block that was stored first.
///
/// With the watcher as the writer these are the top blocks of the chain of the
/// view. Older blocks are not kept twice: Bitcoin Core has them, and whoever
/// needs one asks the source. The watcher does that itself when a reorg goes
/// deeper than the storage.
///
/// Cheap to clone: all clones share the same blocks.
#[derive(Clone)]
pub struct RecentBlocks {
    recent: Arc<Mutex<Recent>>,
    capacity: usize,
}

impl RecentBlocks {
    /// A storage that keeps `capacity` blocks.
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

    /// The number of stored blocks.
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
    /// A storage that keeps [`DEFAULT_RECENT_BLOCKS`] blocks.
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
            // Stored before: its place in the order stays.
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
