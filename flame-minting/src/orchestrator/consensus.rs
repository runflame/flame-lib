use std::{num::NonZeroUsize, sync::Arc};

use btc_integration::{
    BtcBlockTip, HistoryChange, HistoryError, IndexedBlock, ShutdownError, StartupError,
};
use flame_storage::ChainStorage;
use flamechain::BlockHeader;
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::consensus::ConsensusStorage;
use crate::rewards::{RewardsApplier, RewardsEngine, RewardsEngineError, RewardsStorage};

use super::{
    cursor_storage::CursorStorage,
    ports::{ConsensusApplier, ConsensusEngine, ConsensusIndexer},
};

#[cfg(test)]
mod tests;

type LoopResult<E, A, K, R, H, C> = Result<(), ConsensusLoopError<E, A, K, R, H, C>>;

#[derive(Debug)]
pub enum ConsensusLoopError<E, A, K, R, H, C> {
    AlreadyRunning,
    IndexerStartup(StartupError),
    IndexerShutdown(ShutdownError),
    History(HistoryError),
    SubscriptionClosed,
    ReorgUnsupported,
    Engine(E),
    Apply(A),
    RewardsEngine(RewardsEngineError<H, C>),
    RewardsApply(R),
    CursorStorage(K),
    WorkerJoin(tokio::task::JoinError),
}

struct Worker<E, A, K, R, H, C> {
    cancellation: CancellationToken,
    task: JoinHandle<LoopResult<E, A, K, R, H, C>>,
}

impl<E, A, K, R, H, C> Drop for Worker<E, A, K, R, H, C> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct Processor<E, A, K, R: RewardsStorage, H: ChainStorage, C: ConsensusStorage> {
    engine: E,
    applier: Mutex<A>,
    cursor_storage: K,
    rewards_engine: RewardsEngine<H, C>,
    rewards_applier: RewardsApplier<R>,
}

pub struct ConsensusLoop<
    E: ConsensusEngine,
    A: ConsensusApplier,
    K: CursorStorage,
    R: RewardsStorage,
    H: ChainStorage,
    C: ConsensusStorage,
> {
    processor: Arc<Processor<E, A, K, R, H, C>>,
    worker: Mutex<Option<Worker<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error>>>,
}

impl<E, A, K, R, H, C> ConsensusLoop<E, A, K, R, H, C>
where
    E: ConsensusEngine,
    A: ConsensusApplier,
    K: CursorStorage,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    H: ChainStorage + Send + Sync + 'static,
    H::Error: Send + 'static,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
{
    pub fn new(
        engine: E,
        applier: A,
        cursor_storage: K,
        rewards_engine: RewardsEngine<H, C>,
        rewards_applier: RewardsApplier<R>,
    ) -> Self {
        Self {
            processor: Arc::new(Processor {
                engine,
                applier: Mutex::new(applier),
                cursor_storage,
                rewards_engine,
                rewards_applier,
            }),
            worker: Mutex::new(None),
        }
    }

    pub async fn startup<I: ConsensusIndexer>(
        &self,
        indexer: Arc<I>,
    ) -> LoopResult<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error> {
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

    pub async fn shutdown(
        &self,
    ) -> LoopResult<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error> {
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

impl<E, A, K, R, H, C> Processor<E, A, K, R, H, C>
where
    E: ConsensusEngine,
    A: ConsensusApplier,
    K: CursorStorage,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    H: ChainStorage + Send + Sync + 'static,
    H::Error: Send + 'static,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
{
    async fn run<I: ConsensusIndexer>(
        &self,
        indexer: &I,
        mut subscription: watch::Receiver<Option<BtcBlockTip>>,
        mut cursor: BtcBlockTip,
        cancellation: CancellationToken,
    ) -> LoopResult<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error> {
        loop {
            let tip = *subscription.borrow_and_update();
            if tip.is_some_and(|tip| tip != cursor) {
                self.catch_up(indexer, &mut cursor, &cancellation).await?;
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

    async fn catch_up<I: ConsensusIndexer>(
        &self,
        indexer: &I,
        cursor: &mut BtcBlockTip,
        cancellation: &CancellationToken,
    ) -> LoopResult<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error> {
        loop {
            let history = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(()),
                result = indexer.get_history(*cursor, NonZeroUsize::new(1000).unwrap()) => {
                    result.map_err(ConsensusLoopError::History)?
                }
            };
            let HistoryChange::Extension { new_blocks } = history.change else {
                todo!("Reorgs unsupported yet")
            };
            for block in new_blocks {
                self.process_block(block, cursor, cancellation).await?;
                if cancellation.is_cancelled() {
                    return Ok(());
                }
            }
            if *cursor == history.target_tip {
                return Ok(());
            }
        }
    }

    async fn process_block(
        &self,
        block: IndexedBlock,
        cursor: &mut BtcBlockTip,
        cancellation: &CancellationToken,
    ) -> LoopResult<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error> {
        let next_cursor = block.btc_block_tip;
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(()),
            result = self.engine.get_minting_outcome(block) => {
                result.map_err(ConsensusLoopError::Engine)?
            }
        };
        let attached_core_headers = self
            .applier
            .lock()
            .await
            .apply(&outcome)
            .await
            .map_err(ConsensusLoopError::Apply)?;
        self.process_rewards(&attached_core_headers, next_cursor)
            .await?;
        self.cursor_storage
            .store_cursor(next_cursor)
            .await
            .map_err(ConsensusLoopError::CursorStorage)?;
        *cursor = next_cursor;
        Ok(())
    }

    async fn process_rewards(
        &self,
        headers: &[BlockHeader],
        btc_tip: BtcBlockTip,
    ) -> LoopResult<E::Error, A::Error, K::Error, R::Error, H::Error, C::Error> {
        for header in headers {
            let outcome = self
                .rewards_engine
                .calculate(header, btc_tip)
                .await
                .map_err(ConsensusLoopError::RewardsEngine)?;
            if let Some(outcome) = outcome {
                self.rewards_applier
                    .apply(&outcome)
                    .await
                    .map_err(ConsensusLoopError::RewardsApply)?;
            }
        }
        Ok(())
    }
}
