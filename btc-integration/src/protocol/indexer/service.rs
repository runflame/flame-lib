//! Bitcoin tip notifications and independently paged protocol history.

use crate::btc::bitcoin_facade::BitcoinFacade;

use std::{num::NonZeroUsize, sync::Arc};

use tokio::{
    sync::{Mutex, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::btc::rpc::{BtcBlockTip, RpcApi};

use super::{
    error::HistoryError, history::HistoryReader, indexed_block_source::IndexedBlockSource,
    types::HistoryUpdate, worker::IndexerWorker,
};

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("protocol indexer is already running")]
    AlreadyRunning,
    #[error("protocol indexer worker stopped before startup completed")]
    WorkerStopped,
}

#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    #[error("protocol indexer task failed while shutting down: {0}")]
    WorkerJoin(#[from] tokio::task::JoinError),
}

struct IndexerWorkerHandle {
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct ProtocolIndexer<R> {
    rpc: Arc<BitcoinFacade<R>>,
    subscribers: watch::Sender<Option<BtcBlockTip>>,
    history: HistoryReader<R>,
    blocks: Arc<IndexedBlockSource<R>>,
    worker: Mutex<Option<IndexerWorkerHandle>>,
}

impl<R> ProtocolIndexer<R>
where
    R: RpcApi,
{
    pub fn new(rpc: Arc<BitcoinFacade<R>>) -> Self {
        let (subscribers, _) = watch::channel(None);
        let blocks = Arc::new(IndexedBlockSource::new(Arc::clone(&rpc)));
        Self {
            history: HistoryReader::new(Arc::clone(&rpc), Arc::clone(&blocks)),
            blocks,
            rpc,
            subscribers,
            worker: Mutex::new(None),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<BtcBlockTip>> {
        self.subscribers.subscribe()
    }

    pub async fn get_history(
        &self,
        cursor: BtcBlockTip,
        max_blocks: NonZeroUsize,
    ) -> Result<HistoryUpdate, HistoryError> {
        let cancellation = {
            let worker = self.worker.lock().await;
            let handle = worker.as_ref().ok_or(HistoryError::NotRunning)?;
            if self.subscribers.borrow().is_none() || handle.join_handle.is_finished() {
                return Err(HistoryError::NotRunning);
            }
            handle.cancellation_token.clone()
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(HistoryError::NotRunning),
            result = self.history.get_history(cursor, max_blocks) => result,
        }
    }

    pub async fn startup(&self) -> Result<(), StartupError>
    where
        R: 'static,
    {
        let (bootstrap_result, worker_id) = {
            let mut worker_slot = self.worker.lock().await;
            if worker_slot.is_some() {
                return Err(StartupError::AlreadyRunning);
            }

            let cancellation_token = CancellationToken::new();
            let worker = IndexerWorker::new(
                Arc::clone(&self.rpc),
                Arc::clone(&self.blocks),
                self.subscribers.clone(),
                cancellation_token.clone(),
            );
            let (bootstrap_sender, bootstrap_result) = oneshot::channel();
            let join_handle = tokio::spawn(worker.run(bootstrap_sender));
            let worker_id = join_handle.id();
            *worker_slot = Some(IndexerWorkerHandle {
                cancellation_token,
                join_handle,
            });
            (bootstrap_result, worker_id)
        };

        let result = bootstrap_result
            .await
            .map_err(|_| StartupError::WorkerStopped);
        if result.is_err() {
            let mut worker_slot = self.worker.lock().await;
            if matches!(
                worker_slot.as_ref(),
                Some(worker) if worker.join_handle.id() == worker_id
            ) {
                if let Some(worker) = worker_slot.take() {
                    worker.cancellation_token.cancel();
                }
                self.subscribers.send_replace(None);
                self.blocks.clear_cache();
            }
        }
        result
    }

    pub async fn shutdown(&self) -> Result<(), ShutdownError> {
        let mut worker_slot = self.worker.lock().await;
        let Some(worker) = worker_slot.take() else {
            return Ok(());
        };

        worker.cancellation_token.cancel();
        self.subscribers.send_replace(None);
        let result = worker.join_handle.await;
        self.subscribers.send_replace(None);
        self.blocks.clear_cache();
        result?;
        Ok(())
    }
}

impl<R> Drop for ProtocolIndexer<R> {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut().take() {
            worker.cancellation_token.cancel();
        }
        self.subscribers.send_replace(None);
    }
}

#[cfg(test)]
#[path = "unit-tests/indexer_tests.rs"]
mod tests;
