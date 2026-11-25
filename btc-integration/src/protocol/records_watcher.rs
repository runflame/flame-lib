//! Following Bitcoin as Flame sees it: the burns and the endorsements of the
//! best chain.

use thiserror::Error;

use crate::protocol::{BtcBlockRecords, BtcRecordsError, BtcRecordsEvent};
use crate::watcher::{BlockId, BtcEvent, BtcStorage, BtcView, BtcWatcher, ChainSource, WatchError};

/// A [`BtcWatcher`] that gives out the Flame transactions of the new blocks
/// with every event: [`BtcRecordsEvent`] in place of [`BtcEvent`].
///
/// Like the watcher, it has no task and no clock: it gives out events on
/// request, and waiting and retrying belong to the shell that holds it.
pub struct BtcRecordsWatcher<S, B> {
    watcher: BtcWatcher<S, B>,
}

impl<S: ChainSource, B: BtcStorage> BtcRecordsWatcher<S, B> {
    /// Continues from the view of `watcher`.
    pub fn new(watcher: BtcWatcher<S, B>) -> Self {
        BtcRecordsWatcher { watcher }
    }

    /// The Bitcoin view: where the next event starts.
    pub fn view(&self) -> &BtcView {
        self.watcher.view()
    }

    /// Moves the view to `block`: the next event is the difference from it.
    /// See [`BtcWatcher::rewind_to`].
    pub fn rewind_to(&mut self, block: BlockId) {
        self.watcher.rewind_to(block);
    }

    /// The next change of the view: [`BtcWatcher::next`], with the new blocks
    /// read from the storage.
    ///
    /// The view moves before the blocks are read. If they cannot be read, or
    /// the future is dropped while they are, the view is past the event: the
    /// caller gets it again with [`BtcRecordsWatcher::rewind_to`].
    pub async fn next(&mut self) -> Result<BtcRecordsEvent, BtcRecordsWatchError> {
        let event = self.watcher.next().await?;
        let mut blocks = Vec::with_capacity(event.blocks().len());
        for &id in event.blocks() {
            blocks.push(BtcBlockRecords::from_storage(self.watcher.storage(), id).await?);
        }
        Ok(match event {
            BtcEvent::NewBlocks { .. } => BtcRecordsEvent::NewBlocks { blocks },
            BtcEvent::Reorg { fork, removed, .. } => BtcRecordsEvent::Reorg {
                fork,
                removed,
                blocks,
            },
        })
    }
}

/// Why [`BtcRecordsWatcher::next`] gave no event.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BtcRecordsWatchError {
    /// Bitcoin could not be followed.
    #[error(transparent)]
    Watch(#[from] WatchError),
    /// The Flame transactions of a new block could not be read.
    #[error(transparent)]
    Records(#[from] BtcRecordsError),
}

impl BtcRecordsWatchError {
    /// Retrying after a fatal error is pointless: the node stops.
    ///
    /// A block that the storage did not give can be read after
    /// [`BtcRecordsWatcher::rewind_to`]. Endorsements that cannot be checked
    /// stay that way.
    pub fn is_fatal(&self) -> bool {
        match self {
            BtcRecordsWatchError::Watch(error) => error.is_fatal(),
            BtcRecordsWatchError::Records(BtcRecordsError::Endorsements(_)) => true,
            BtcRecordsWatchError::Records(_) => false,
        }
    }
}
