use crate::block_processing::{
    BlockProcessor, ConsensusManager, ConsensusManagerError, FlameProcessor,
    journal::consensus::ConsensusJournal,
};
use crate::{ConsensusHandle, ConsensusSendError};
use btc_integration::{
    BtcRecordsError, BtcRecordsEvent, BtcRecordsWatchError, BtcRecordsWatcher,
    watcher::{
        BlockId, BtcStorage, BtcView, BtcWatcher, ChainSource, WatchError, impls::RecentBlocks,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests;

type ProcessingError<B, F, J> = ConsensusManagerError<
    <B as BlockProcessor>::Error,
    <F as FlameProcessor>::Error,
    <J as ConsensusJournal>::Error,
>;

type LoopResult<E> = Result<(), ConsensusLoopError<E>>;

/// The pause before the first retry after Bitcoin did not answer. It doubles
/// with every failure in a row, up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(50);
const LONGEST_PAUSE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum ConsensusLoopError<E> {
    AlreadyRunning,
    /// Bitcoin cannot be followed any more.
    Watch(WatchError),
    /// A block of an event could not be read from the storage of the watcher.
    Records(BtcRecordsError),
    Process(E),
    Delivery(ConsensusSendError),
    WorkerJoin(tokio::task::JoinError),
}

impl<E> From<BtcRecordsWatchError> for ConsensusLoopError<E> {
    fn from(error: BtcRecordsWatchError) -> Self {
        match error {
            BtcRecordsWatchError::Watch(error) => ConsensusLoopError::Watch(error),
            BtcRecordsWatchError::Records(error) => ConsensusLoopError::Records(error),
        }
    }
}

struct BitcoinSyncWorker<E> {
    cancellation: CancellationToken,
    task: JoinHandle<LoopResult<E>>,
}

impl<E> Drop for BitcoinSyncWorker<E> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub struct ConsensusLoop<B: BlockProcessor, F: FlameProcessor, J: ConsensusJournal> {
    manager: Arc<ConsensusManager<B, F, J>>,
    /// The lowest Bitcoin block consensus knows: a reorg never goes below it.
    genesis_id: BlockId,
    /// The best Bitcoin block the loop has seen, for the minter.
    best: watch::Sender<Option<BlockId>>,
    worker: Mutex<Option<BitcoinSyncWorker<ProcessingError<B, F, J>>>>,
}

impl<B: BlockProcessor, F: FlameProcessor, J: ConsensusJournal> ConsensusLoop<B, F, J> {
    pub fn new(manager: ConsensusManager<B, F, J>, genesis_id: BlockId) -> Self {
        Self {
            manager: Arc::new(manager),
            genesis_id,
            best: watch::channel(None).0,
            worker: Mutex::new(None),
        }
    }

    pub(crate) fn manager(&self) -> &Arc<ConsensusManager<B, F, J>> {
        &self.manager
    }

    pub fn consensus_handle(&self) -> ConsensusHandle<B, F, J> {
        self.manager.consensus_handle()
    }

    /// The best Bitcoin block: published as soon as an event names it, before
    /// consensus has applied the blocks of the event.
    pub fn subscribe(&self) -> watch::Receiver<Option<BlockId>> {
        self.best.subscribe()
    }

    /// Recovers consensus, then follows `source` from the block consensus
    /// stopped at.
    pub async fn startup<S: ChainSource + 'static>(
        &self,
        source: S,
    ) -> LoopResult<ProcessingError<B, F, J>> {
        let mut worker = self.worker.lock().await;
        if worker.is_some() {
            return Err(ConsensusLoopError::AlreadyRunning);
        }
        let cursor = self
            .manager
            .startup()
            .await
            .map_err(ConsensusLoopError::Process)?;
        let watcher = BtcRecordsWatcher::new(BtcWatcher::resume(
            source,
            RecentBlocks::default(),
            BtcView::restore(self.genesis_id, cursor),
        ));
        self.best.send_replace(Some(cursor));
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let manager = self.manager.clone();
        let best = self.best.clone();
        let task = tokio::spawn(async move {
            let result = Self::run(manager.as_ref(), watcher, &best, task_cancellation).await;
            let consensus_shutdown = manager
                .shutdown()
                .await
                .map_err(ConsensusLoopError::Process);
            consensus_shutdown.and(result)
        });
        *worker = Some(BitcoinSyncWorker { cancellation, task });
        Ok(())
    }

    pub async fn shutdown(&self) -> LoopResult<ProcessingError<B, F, J>> {
        let mut slot = self.worker.lock().await;
        let Some(worker) = slot.as_mut() else {
            return Ok(());
        };
        worker.cancellation.cancel();
        self.manager.request_shutdown();
        let result = (&mut worker.task).await;
        *slot = None;
        match result {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(ConsensusLoopError::WorkerJoin(error)),
        }
    }

    /// Hands the events of the watcher to consensus until it is cancelled,
    /// consensus stops, or Bitcoin cannot be followed any more. A source that
    /// did not answer is asked again after a pause.
    async fn run<S: ChainSource, T: BtcStorage>(
        manager: &ConsensusManager<B, F, J>,
        mut watcher: BtcRecordsWatcher<S, T>,
        best: &watch::Sender<Option<BlockId>>,
        cancellation: CancellationToken,
    ) -> LoopResult<ProcessingError<B, F, J>> {
        let mut pause = FIRST_PAUSE;
        loop {
            let event = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(()),
                _ = manager.stopped() => return manager.shutdown().await.map_err(ConsensusLoopError::Process),
                event = watcher.next() => event,
            };
            match event {
                Ok(event) => {
                    pause = FIRST_PAUSE;
                    // The minter hears of the block at once: it endorses a
                    // core block before consensus is through with the event.
                    best.send_replace(Some(watcher.view().best()));
                    Self::deliver(manager, event, &cancellation)?;
                }
                Err(error) if error.is_fatal() => return Err(error.into()),
                Err(error) => {
                    log::warn!("bitcoin source failed, retry in {pause:?}: {error}");
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Ok(()),
                        _ = manager.stopped() => return manager.shutdown().await.map_err(ConsensusLoopError::Process),
                        _ = tokio::time::sleep(pause) => {}
                    }
                    pause = (pause * 2).min(LONGEST_PAUSE);
                }
            }
        }
    }

    /// Queues one event of the watcher to consensus.
    fn deliver(
        manager: &ConsensusManager<B, F, J>,
        event: BtcRecordsEvent,
        cancellation: &CancellationToken,
    ) -> LoopResult<ProcessingError<B, F, J>> {
        let delivered = match event {
            BtcRecordsEvent::NewBlocks { blocks } => blocks
                .into_iter()
                .try_for_each(|block| manager.new_bitcoin_block(block)),
            BtcRecordsEvent::Reorg {
                removed, blocks, ..
            } => manager.bitcoin_reorg(removed, blocks),
        };
        match delivered {
            Err(ConsensusSendError::Stopped) if cancellation.is_cancelled() => Ok(()),
            delivered => delivered.map_err(ConsensusLoopError::Delivery),
        }
    }
}
