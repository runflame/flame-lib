use super::{BlockProcessor, FlameProcessor, journal::consensus::ConsensusJournal};
use crate::ConsensusHandle;
use crate::block_processing::BtcBlockRecords;
use crate::block_processing::journal::bitcoin::bitcoin_operation::BitcoinOperationKind;
use crate::block_processing::journal::operation::ConsensusOperation;
use btc_integration::watcher::BlockId;
use flamechain::BlockTip;
use std::sync::Arc;
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests;

pub const CONSENSUS_QUEUE_CAPACITY: usize = 1024;

#[derive(Clone, Debug)]
enum ConsensusEvent {
    NewBitcoinBlock(BtcBlockRecords),
    NewFlameBlockArrival(BlockTip),
    BitcoinReorg {
        removed_block_tips: Vec<BlockId>,
        new_blocks: Vec<BtcBlockRecords>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsensusStatus {
    NotReady,
    Running,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConsensusSendError {
    #[error("consensus manager is not ready")]
    NotReady,
    #[error("consensus manager has stopped")]
    Stopped,
    #[error("consensus worker has failed")]
    WorkerFailed,
}

struct ConsensusDeliveryState {
    status: ConsensusStatus,
    sender: Option<mpsc::Sender<ConsensusEvent>>,
    cancellation: Option<CancellationToken>,
}

type ManagerError<B, F, J> = ConsensusManagerError<
    <B as BlockProcessor>::Error,
    <F as FlameProcessor>::Error,
    <J as ConsensusJournal>::Error,
>;

struct ProcessingWorker<E> {
    state: watch::Sender<ConsensusDeliveryState>,
    task: JoinHandle<Result<(), E>>,
}

impl<E> Drop for ProcessingWorker<E> {
    fn drop(&mut self) {
        stop_worker(&self.state);
    }
}

pub struct ConsensusManager<B: BlockProcessor, F: FlameProcessor, J: ConsensusJournal> {
    bitcoin_processor: Arc<B>,
    flame_processor: Arc<F>,
    journal: Arc<J>,
    state: watch::Sender<ConsensusDeliveryState>,
    worker: Mutex<Option<ProcessingWorker<ManagerError<B, F, J>>>>,
}

impl<B: BlockProcessor, F: FlameProcessor, J: ConsensusJournal> ConsensusManager<B, F, J> {
    pub fn new(bitcoin_processor: B, flame_processor: F, journal: J) -> Self {
        let (state, _) = watch::channel(ConsensusDeliveryState {
            status: ConsensusStatus::NotReady,
            sender: None,
            cancellation: None,
        });
        Self {
            bitcoin_processor: Arc::new(bitcoin_processor),
            flame_processor: Arc::new(flame_processor),
            journal: Arc::new(journal),
            state,
            worker: Mutex::new(None),
        }
    }

    pub fn consensus_handle(self: &Arc<Self>) -> ConsensusHandle<B, F, J> {
        ConsensusHandle::new(self.clone())
    }

    pub(crate) fn new_bitcoin_block(
        &self,
        block: BtcBlockRecords,
    ) -> Result<(), ConsensusSendError> {
        self.enqueue(ConsensusEvent::NewBitcoinBlock(block))
    }

    pub(crate) fn new_flame_block(&self, tip: BlockTip) -> Result<(), ConsensusSendError> {
        self.enqueue(ConsensusEvent::NewFlameBlockArrival(tip))
    }

    pub(crate) fn bitcoin_reorg(
        &self,
        removed_block_tips: Vec<BlockId>,
        new_blocks: Vec<BtcBlockRecords>,
    ) -> Result<(), ConsensusSendError> {
        self.enqueue(ConsensusEvent::BitcoinReorg {
            removed_block_tips,
            new_blocks,
        })
    }

    fn enqueue(&self, event: ConsensusEvent) -> Result<(), ConsensusSendError> {
        let result = {
            let state = self.state.borrow();
            match state.status {
                ConsensusStatus::NotReady => return Err(ConsensusSendError::NotReady),
                ConsensusStatus::Stopped => return Err(ConsensusSendError::Stopped),
                ConsensusStatus::Failed => return Err(ConsensusSendError::WorkerFailed),
                ConsensusStatus::Running => {}
            }
            state
                .sender
                .as_ref()
                .expect("running manager has a queue")
                .try_send(event)
        };
        match result {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => queue_overflow(),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ConsensusSendError::WorkerFailed),
        }
    }

    pub fn status(&self) -> ConsensusStatus {
        self.state.borrow().status
    }

    pub async fn stopped(&self) -> ConsensusStatus {
        let mut state = self.state.subscribe();
        let stopped = state
            .wait_for(|state| state.status != ConsensusStatus::Running)
            .await
            .expect("manager owns the state sender");
        stopped.status
    }

    fn prepare_startup(&self) {
        self.state.send_modify(|state| {
            state.status = ConsensusStatus::NotReady;
            state.sender = None;
            state.cancellation = None;
        });
    }

    fn start(
        &self,
        sender: mpsc::Sender<ConsensusEvent>,
        cancellation: CancellationToken,
    ) -> Result<(), ConsensusSendError> {
        let mut result = Err(ConsensusSendError::Stopped);
        self.state.send_modify(|state| {
            if state.status == ConsensusStatus::NotReady {
                state.status = ConsensusStatus::Running;
                state.sender = Some(sender);
                state.cancellation = Some(cancellation);
                result = Ok(());
            }
        });
        result
    }

    pub(crate) fn bitcoin_processor(&self) -> &B {
        &self.bitcoin_processor
    }

    pub async fn startup(&self) -> Result<BlockId, ManagerError<B, F, J>> {
        let mut slot = self.worker.lock().await;
        if slot.is_some() {
            return Err(ConsensusManagerError::AlreadyRunning);
        }
        self.prepare_startup();
        let cursor = self.recover().await?;
        let (sender, receiver) = mpsc::channel(CONSENSUS_QUEUE_CAPACITY);
        let cancellation = CancellationToken::new();
        self.start(sender, cancellation.clone())
            .map_err(ConsensusManagerError::Delivery)?;
        let bitcoin_processor = self.bitcoin_processor.clone();
        let flame_processor = self.flame_processor.clone();
        let state = self.state.clone();
        let exit = ProcessingWorkerExit(state.clone());
        let task = tokio::spawn(async move {
            let _exit = exit;
            Self::run(
                bitcoin_processor,
                flame_processor,
                state,
                receiver,
                cancellation,
                cursor,
            )
            .await
        });
        *slot = Some(ProcessingWorker {
            state: self.state.clone(),
            task,
        });
        Ok(cursor)
    }

    pub fn request_shutdown(&self) {
        stop_worker(&self.state);
    }

    pub async fn shutdown(&self) -> Result<(), ManagerError<B, F, J>> {
        self.request_shutdown();
        let mut slot = self.worker.lock().await;
        let Some(worker) = slot.as_mut() else {
            return Ok(());
        };
        let result = (&mut worker.task).await;
        *slot = None;
        match result {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(ConsensusManagerError::WorkerJoin(error)),
        }
    }

    async fn run(
        bitcoin_processor: Arc<B>,
        flame_processor: Arc<F>,
        state: watch::Sender<ConsensusDeliveryState>,
        mut receiver: mpsc::Receiver<ConsensusEvent>,
        cancellation: CancellationToken,
        mut cursor: BlockId,
    ) -> Result<(), ManagerError<B, F, J>> {
        loop {
            let event = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(()),
                event = receiver.recv() => event.ok_or(ConsensusManagerError::QueueClosed)?,
            };
            if state.borrow().status != ConsensusStatus::Running {
                return Ok(());
            }
            if let Err(error) = Self::process_event(
                bitcoin_processor.as_ref(),
                flame_processor.as_ref(),
                event,
                &mut cursor,
            )
            .await
            {
                log::error!("consensus worker stopped after an operation error");
                return Err(error);
            }
        }
    }
}

struct ProcessingWorkerExit(watch::Sender<ConsensusDeliveryState>);

impl Drop for ProcessingWorkerExit {
    fn drop(&mut self) {
        worker_exited(&self.0);
    }
}

#[derive(Debug)]
pub enum ConsensusManagerError<B, F, J> {
    AlreadyRunning,
    Bitcoin(B),
    Flame(F),
    Journal(J),
    Delivery(ConsensusSendError),
    QueueClosed,
    WorkerJoin(tokio::task::JoinError),
}

fn stop_worker(state: &watch::Sender<ConsensusDeliveryState>) {
    state.send_modify(|state| {
        state.status = ConsensusStatus::Stopped;
        state.sender = None;
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
    });
}

fn worker_exited(state: &watch::Sender<ConsensusDeliveryState>) {
    state.send_modify(|state| {
        if state.status == ConsensusStatus::Running {
            state.status = ConsensusStatus::Failed;
            state.sender = None;
            state.cancellation = None;
        }
    });
}

fn queue_overflow() -> ! {
    let _ = std::panic::catch_unwind(|| {
        panic!("consensus queue capacity of {CONSENSUS_QUEUE_CAPACITY} exceeded");
    });
    std::process::abort()
}

impl<B: BlockProcessor, F: FlameProcessor, J: ConsensusJournal> ConsensusManager<B, F, J> {
    async fn recover(&self) -> Result<BlockId, ManagerError<B, F, J>> {
        let latest = self
            .journal
            .latest_entry()
            .await
            .map_err(ConsensusManagerError::Journal)?;
        let Some(entry) = latest else {
            return self
                .bitcoin_processor
                .resume()
                .await
                .map_err(ConsensusManagerError::Bitcoin);
        };
        if !entry.is_complete() {
            return match entry.operation {
                ConsensusOperation::Bitcoin(_) => self
                    .bitcoin_processor
                    .resume()
                    .await
                    .map_err(ConsensusManagerError::Bitcoin),
                ConsensusOperation::Flame(operation) => {
                    self.flame_processor
                        .resume()
                        .await
                        .map_err(ConsensusManagerError::Flame)?;
                    Ok(operation.btc_tip)
                }
            };
        }
        Ok(match entry.operation {
            ConsensusOperation::Bitcoin(operation) => match operation.kind {
                BitcoinOperationKind::Apply => operation.entry.btc_tip,
                BitcoinOperationKind::Rollback => operation.entry.previous_btc_tip,
            },
            ConsensusOperation::Flame(operation) => operation.btc_tip,
        })
    }

    async fn process_event(
        bitcoin_processor: &B,
        flame_processor: &F,
        event: ConsensusEvent,
        cursor: &mut BlockId,
    ) -> Result<(), ManagerError<B, F, J>> {
        match event {
            ConsensusEvent::NewBitcoinBlock(block) => {
                *cursor = bitcoin_processor
                    .apply(block)
                    .await
                    .map_err(ConsensusManagerError::Bitcoin)?;
            }
            ConsensusEvent::NewFlameBlockArrival(tip) => {
                flame_processor
                    .process(tip)
                    .await
                    .map_err(ConsensusManagerError::Flame)?;
            }
            ConsensusEvent::BitcoinReorg {
                removed_block_tips,
                new_blocks,
            } => {
                for tip in removed_block_tips {
                    *cursor = bitcoin_processor
                        .rollback(tip)
                        .await
                        .map_err(ConsensusManagerError::Bitcoin)?;
                }
                for block in new_blocks {
                    *cursor = bitcoin_processor
                        .apply(block)
                        .await
                        .map_err(ConsensusManagerError::Bitcoin)?;
                }
            }
        }
        Ok(())
    }
}
