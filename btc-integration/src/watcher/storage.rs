//! The storage of Bitcoin blocks.

use std::future::Future;

use bitcoin::Block;
use thiserror::Error;

use crate::watcher::index::BlockIndex;
use crate::watcher::types::{BlockId, Prevouts};

/// The Bitcoin blocks the node has received: [`BlockId`] -> [`Block`] and its
/// [`Prevouts`].
///
/// The storage does not know the chain: a block only points to the previous
/// one. Which block is the best one is known to whoever holds a
/// [`crate::watcher::BtcView`]; from that block down the chain is walked through
/// `prev_blockhash` (see [`BlockIndex::ancestor`]).
///
/// [`crate::watcher::BtcWatcher`] is the writer:
/// - it puts a block before it names the block to the consumer in an event;
/// - it removes the blocks a reorg has undone.
///
/// So the storage holds the chain of the view, from the block above the genesis
/// block up to the best block, or the top of that chain if the storage
/// is bounded ([`BtcStorage::capacity`]). The watcher does not ask the source
/// for a block that is already here.
///
/// After [`crate::watcher::BtcWatcher::rewind_to`] the storage also holds the
/// blocks above the block the view went back to. The events name them again,
/// unless Bitcoin has replaced them by then: such a block stays until a
/// bounded storage drops it.
///
/// Many callers need only the header, so it has a method of its own,
/// [`BlockIndex::header`]: a database can keep headers apart from
/// transactions. The implementations are in [`crate::watcher::impls`], both in memory;
/// the methods are async and fallible so that a database can be one too.
pub trait BtcStorage: BlockIndex<Error = StorageError> {
    /// A stored block with its transactions. `None` means it is not stored.
    fn block(
        &self,
        id: BlockId,
    ) -> impl Future<Output = Result<Option<Block>, StorageError>> + Send;

    /// The outputs the inputs of a stored block spend. `None` means the block
    /// is not stored.
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

    /// Removes one block. Removing a block that is not stored is not an error.
    fn remove(&self, id: BlockId) -> impl Future<Output = Result<(), StorageError>> + Send {
        async move { self.remove_many(&[id]).await }
    }

    /// How many blocks the storage keeps, if it drops old blocks to make room
    /// for new ones. `None`: it keeps every block until the block is removed.
    ///
    /// The watcher puts no more blocks than this into one event. Otherwise the
    /// storage would drop some of them before the consumer reads them.
    fn capacity(&self) -> Option<usize> {
        None
    }
}

/// The storage did not answer. A retry may help.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct StorageError(pub String);
