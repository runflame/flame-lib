use std::{num::NonZeroUsize, sync::Arc};

use btc_integration::{BtcBlockTip, HistoryChange, HistoryError, ShutdownError, StartupError};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    cursor_storage::CursorStorage,
    ports::{ConsensusApplier, ConsensusEngine, ConsensusIndexer},
};

type LoopResult<E, A, K> = Result<(), ConsensusLoopError<E, A, K>>;

#[derive(Debug)]
pub enum ConsensusLoopError<E, A, K> {
    AlreadyRunning,
    IndexerStartup(StartupError),
    IndexerShutdown(ShutdownError),
    History(HistoryError),
    SubscriptionClosed,
    ReorgUnsupported,
    Engine(E),
    Apply(A),
    CursorStorage(K),
    WorkerJoin(tokio::task::JoinError),
}

struct Worker<E, A, K> {
    cancellation: CancellationToken,
    task: JoinHandle<LoopResult<E, A, K>>,
}

impl<E, A, K> Drop for Worker<E, A, K> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct Processor<E, A, K> {
    engine: E,
    applier: Mutex<A>,
    cursor_storage: K,
}

pub struct ConsensusLoop<E: ConsensusEngine, A: ConsensusApplier, K: CursorStorage> {
    processor: Arc<Processor<E, A, K>>,
    worker: Mutex<Option<Worker<E::Error, A::Error, K::Error>>>,
}

impl<E: ConsensusEngine, A: ConsensusApplier, K: CursorStorage> ConsensusLoop<E, A, K> {
    pub fn new(engine: E, applier: A, cursor_storage: K) -> Self {
        Self {
            processor: Arc::new(Processor {
                engine,
                applier: Mutex::new(applier),
                cursor_storage,
            }),
            worker: Mutex::new(None),
        }
    }

    pub async fn startup<I: ConsensusIndexer>(
        &self,
        indexer: Arc<I>,
    ) -> LoopResult<E::Error, A::Error, K::Error> {
        let mut worker = self.worker.lock().await;
        if worker.is_some() {
            return Err(ConsensusLoopError::AlreadyRunning);
        }
        let cursor = self
            .processor
            .cursor_storage
            .get_cursor()
            .await
            .map_err(ConsensusLoopError::CursorStorage)?;
        indexer
            .startup()
            .await
            .map_err(ConsensusLoopError::IndexerStartup)?;
        let subscription = indexer.subscribe();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let processor = self.processor.clone();
        let task = tokio::spawn(async move {
            let result = processor
                .run(indexer.as_ref(), subscription, cursor, task_cancellation)
                .await;
            let shutdown = indexer
                .shutdown()
                .await
                .map_err(ConsensusLoopError::IndexerShutdown);
            result.and(shutdown)
        });
        *worker = Some(Worker { cancellation, task });
        Ok(())
    }

    pub async fn shutdown(&self) -> LoopResult<E::Error, A::Error, K::Error> {
        let mut slot = self.worker.lock().await;
        let Some(worker) = slot.as_mut() else {
            return Ok(());
        };
        worker.cancellation.cancel();
        let result = (&mut worker.task)
            .await
            .map_err(ConsensusLoopError::WorkerJoin);
        *slot = None;
        result?
    }
}

impl<E: ConsensusEngine, A: ConsensusApplier, K: CursorStorage> Processor<E, A, K> {
    async fn run<I: ConsensusIndexer>(
        &self,
        indexer: &I,
        mut subscription: watch::Receiver<Option<BtcBlockTip>>,
        mut cursor: BtcBlockTip,
        cancellation: CancellationToken,
    ) -> LoopResult<E::Error, A::Error, K::Error> {
        let mut applier = self.applier.lock().await;
        loop {
            let tip = *subscription.borrow_and_update();
            if tip.is_some_and(|tip| tip != cursor) {
                loop {
                    let history = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Ok(()),
                        result = indexer.get_history(cursor, NonZeroUsize::new(1000).unwrap()) => {
                            result.map_err(ConsensusLoopError::History)?
                        }
                    };
                    let HistoryChange::Extension { new_blocks } = history.change else {
                        todo!("Reorgs unsupported yet")
                    };
                    for block in new_blocks {
                        let next_cursor = block.btc_block_tip;
                        let outcome = tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => return Ok(()),
                            result = self.engine.get_minting_outcome(block) => {
                                result.map_err(ConsensusLoopError::Engine)?
                            }
                        };
                        // TODO: these operations are not atomic
                        applier
                            .apply(&outcome)
                            .await
                            .map_err(ConsensusLoopError::Apply)?;
                        self.cursor_storage
                            .store_cursor(next_cursor)
                            .await
                            .map_err(ConsensusLoopError::CursorStorage)?;
                        cursor = next_cursor;
                    }
                    if cursor == history.target_tip {
                        break;
                    }
                }
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(()),
                result = subscription.changed() => {
                    result.map_err(|_| ConsensusLoopError::SubscriptionClosed)?;
                }
            }
        }
    }
}
