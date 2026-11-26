use thiserror::Error;

use crate::protocol::{BtcBlockRecords, BtcRecordsError, BtcRecordsEvent};
use crate::watcher::{BlockId, BtcEvent, BtcStorage, BtcView, BtcWatcher, ChainSource, WatchError};

pub struct BtcRecordsWatcher<S, B> {
    watcher: BtcWatcher<S, B>,
}

impl<S: ChainSource, B: BtcStorage> BtcRecordsWatcher<S, B> {
    pub fn new(watcher: BtcWatcher<S, B>) -> Self {
        BtcRecordsWatcher { watcher }
    }

    pub fn view(&self) -> &BtcView {
        self.watcher.view()
    }

    pub fn rewind_to(&mut self, block: BlockId) {
        self.watcher.rewind_to(block);
    }

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

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BtcRecordsWatchError {
    #[error(transparent)]
    Watch(#[from] WatchError),
    #[error(transparent)]
    Records(#[from] BtcRecordsError),
}

impl BtcRecordsWatchError {
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
