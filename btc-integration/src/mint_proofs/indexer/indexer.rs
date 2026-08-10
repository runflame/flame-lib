//! Mint Proof Indexer is a background task that performs 2 key functions:
//! - Indexing minting proofs
//! - Indexing reorgs
//!
//! The service does not have any persistent state, as only last 10 bitcoin blocks are usually needed
//! to be indexed. It is the task of indexer user to store the last bitcoin tip and pass it to the
//! indexer on startup. This ensures that only one btc block is used in the node, and there is no
//! conflict between indexer btc block tip and indexer user btc block tip.

use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::{Mutex, broadcast, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use types::{BlockHash, FlameNetwork};

use crate::MintingProof;
use crate::mint_proofs::indexer::worker::IndexerWorker;
use crate::mint_proofs::minting_proof_storage::{MintingProofStorage, MintingProofsByBitcoinBlock};
use crate::rpc::RpcApi;

pub type NewMintingProofs = BTreeMap<BlockHash, Vec<MintingProof>>;

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("mint-proof indexer is already running")]
    AlreadyRunningError,
    #[error("mint-proof indexer worker stopped before startup completed")]
    WorkerStopped,
}

#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    #[error("mint-proof indexer task failed while shutting down: {0}")]
    WorkerJoinError(#[from] tokio::task::JoinError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MintingProofUpdate {
    NewBlocks(NewMintingProofs),
    Reorg {
        deleted_proofs: MintingProofsByBitcoinBlock,
        new_proofs: NewMintingProofs,
    },
}

struct IndexerWorkerHandle {
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct MintProofIndexer<R, S> {
    rpc: Arc<R>,
    storage: Arc<S>,
    network: FlameNetwork,
    subscribers: broadcast::Sender<Arc<MintingProofUpdate>>,
    worker: Mutex<Option<IndexerWorkerHandle>>,
}

impl<R, S> MintProofIndexer<R, S>
where
    R: RpcApi,
    S: MintingProofStorage,
{
    pub fn new(rpc: Arc<R>, storage: Arc<S>, network: FlameNetwork) -> Self {
        let (subscribers, _) = broadcast::channel(16);

        Self {
            rpc,
            storage,
            network,
            subscribers,
            worker: Mutex::new(None),
        }
    }

    pub async fn get_proofs(&self, flame_block_hash: BlockHash) -> Vec<MintingProof> {
        self.storage.get(flame_block_hash).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<MintingProofUpdate>> {
        self.subscribers.subscribe()
    }

    pub async fn startup(&self) -> Result<(), StartupError>
    where
        R: 'static,
        S: 'static,
    {
        let (bootstrap_result, worker_id) = {
            let mut worker_slot = self.worker.lock().await;
            if worker_slot.is_some() {
                return Err(StartupError::AlreadyRunningError);
            }

            let cancellation_token = CancellationToken::new();
            let indexer_worker = IndexerWorker::new(
                Arc::clone(&self.rpc),
                Arc::clone(&self.storage),
                self.network,
                self.subscribers.clone(),
                cancellation_token.clone(),
            );
            let (bootstrap_sender, bootstrap_result) = oneshot::channel();
            let join_handle = tokio::spawn(indexer_worker.run(bootstrap_sender));
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
                *worker_slot = None;
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
        worker.join_handle.await?;
        drop(worker_slot);

        Ok(())
    }
}

impl<R, S> Drop for MintProofIndexer<R, S> {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut().take() {
            worker.cancellation_token.cancel();
        }
    }
}

#[cfg(test)]
#[path = "unit-tests/indexer_tests.rs"]
mod tests;
