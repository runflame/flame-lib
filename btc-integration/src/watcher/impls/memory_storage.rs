//! The storage of Bitcoin blocks in memory.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use bitcoin::Block;
use bitcoin::block::Header;

use crate::watcher::index::BlockIndex;
use crate::watcher::storage::{BtcStorage, StorageError};
use crate::watcher::types::{BlockId, Prevouts};

/// Storage in memory. Cheap to clone: all clones share the same blocks, so one
/// clone goes to the watcher and another stays with the consumer.
#[derive(Clone, Default)]
pub struct MemoryStorage {
    blocks: Arc<Mutex<BTreeMap<BlockId, (Block, Prevouts)>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<BlockId, (Block, Prevouts)>> {
        self.blocks
            .lock()
            .expect("the storage lock is not poisoned")
    }

    /// The number of stored blocks.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    pub fn contains(&self, id: BlockId) -> bool {
        self.lock().contains_key(&id)
    }
}

impl BlockIndex for MemoryStorage {
    type Error = StorageError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, StorageError> {
        Ok(self.lock().get(&id).map(|(block, _)| block.header))
    }
}

impl BtcStorage for MemoryStorage {
    async fn block(&self, id: BlockId) -> Result<Option<Block>, StorageError> {
        Ok(self.lock().get(&id).map(|(block, _)| block.clone()))
    }

    async fn prevouts(&self, id: BlockId) -> Result<Option<Prevouts>, StorageError> {
        Ok(self.lock().get(&id).map(|(_, prevouts)| prevouts.clone()))
    }

    async fn put(&self, id: BlockId, block: Block, prevouts: Prevouts) -> Result<(), StorageError> {
        self.lock().insert(id, (block, prevouts));
        Ok(())
    }

    async fn remove_many(&self, ids: &[BlockId]) -> Result<(), StorageError> {
        let mut blocks = self.lock();
        for id in ids {
            blocks.remove(id);
        }
        Ok(())
    }
}
