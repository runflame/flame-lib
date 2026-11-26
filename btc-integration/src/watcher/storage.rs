use std::future::Future;

use bitcoin::Block;
use thiserror::Error;

use crate::watcher::index::BlockIndex;
use crate::watcher::types::{BlockId, Prevouts};

pub trait BtcStorage: BlockIndex<Error = StorageError> {
    fn block(
        &self,
        id: BlockId,
    ) -> impl Future<Output = Result<Option<Block>, StorageError>> + Send;

    fn prevouts(
        &self,
        id: BlockId,
    ) -> impl Future<Output = Result<Option<Prevouts>, StorageError>> + Send;

    /// Stores a block with its prevouts. Storing the same block again changes
    /// nothing.
    fn put(
        &self,
        id: BlockId,
        block: Block,
        prevouts: Prevouts,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Removes blocks, all at once. A block that is not stored is skipped: that
    /// is not an error.
    fn remove_many(&self, ids: &[BlockId])
    -> impl Future<Output = Result<(), StorageError>> + Send;

    fn remove(&self, id: BlockId) -> impl Future<Output = Result<(), StorageError>> + Send {
        async move { self.remove_many(&[id]).await }
    }

    /// How many blocks the storage keeps, if it drops old blocks to make room
    /// for new ones. `None`: it keeps every block until the block is removed.
    fn capacity(&self) -> Option<usize> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct StorageError(pub String);
