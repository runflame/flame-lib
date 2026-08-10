use corepc_client::bitcoin::Transaction;
use corepc_client::client_sync::Error as BitcoinRpcError;
use futures_util::future::try_join_all;
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::MintingProof;
use crate::mint_proofs::MintingProofData;
use crate::mint_proofs::indexer::bitcoin_chain_update_planner::{
    BitcoinChainUpdatePlanner, ChainUpdatePlan,
};
use crate::mint_proofs::indexer::indexer::{MintingProofUpdate, NewMintingProofs};
use crate::mint_proofs::minting_proof_storage::MintingProofStorage;
use crate::rpc::{BtcBlockTip, RpcApi};

const BOOTSTRAP_INDEX_BLOCKS: usize = 20;
const RETRY_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum IndexerWorkerError {
    #[error("Bitcoin Core RPC error: {0}")]
    RpcError(#[from] BitcoinRpcError),
    #[error("mint-proof indexer worker was cancelled")]
    Cancelled,
}

type WorkerResult<T> = Result<T, IndexerWorkerError>;

pub(super) struct IndexerWorker<R, S> {
    rpc_api: Arc<R>,
    storage: Arc<S>,
    network_id: u8,
    subscribers: broadcast::Sender<Arc<MintingProofUpdate>>,
    cancellation_token: CancellationToken,
}

impl<R, S> IndexerWorker<R, S>
where
    R: RpcApi,
    S: MintingProofStorage,
{
    pub(super) fn new(
        rpc_api: Arc<R>,
        storage: Arc<S>,
        network_id: u8,
        subscribers: broadcast::Sender<Arc<MintingProofUpdate>>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            rpc_api,
            storage,
            network_id,
            subscribers,
            cancellation_token,
        }
    }

    pub(super) async fn run(self, bootstrap_result: oneshot::Sender<()>)
    where
        R: 'static,
        S: 'static,
    {
        let mut bootstrap_result = Some(bootstrap_result);

        loop {
            match self.run_inner(&mut bootstrap_result).await {
                Ok(()) | Err(IndexerWorkerError::Cancelled) => return,
                Err(error) => log::error!("mint-proof indexer worker failed: {error}"),
            }

            if !self.wait_for_retry().await {
                return;
            }
        }
    }

    async fn run_inner(
        &self,
        bootstrap_result: &mut Option<oneshot::Sender<()>>,
    ) -> WorkerResult<()> {
        let initial_tip = self.bootstrap().await?;
        if let Some(bootstrap_result) = bootstrap_result.take() {
            let _ = bootstrap_result.send(());
        }
        let mut update_planner =
            BitcoinChainUpdatePlanner::new(initial_tip, Arc::clone(&self.rpc_api));

        loop {
            let applied_tip = update_planner.get_applied_tip();
            let announced_tip = self
                .with_cancellation(|api| async move { api.wait_for_next_block(applied_tip).await })
                .await?;

            self.handle_new_tip(announced_tip, &mut update_planner)
                .await?;
        }
    }

    async fn handle_new_tip(
        &self,
        new_tip: BtcBlockTip,
        update_planner: &mut BitcoinChainUpdatePlanner<R>,
    ) -> WorkerResult<BtcBlockTip> {
        let mut tip = new_tip;

        loop {
            let update_plan = self
                .with_cancellation(|_| async { update_planner.plan_update(tip).await })
                .await?;

            let new_blocks = match &update_plan {
                ChainUpdatePlan::Extension { new_blocks }
                | ChainUpdatePlan::Reorg { new_blocks, .. } => new_blocks,
            };
            let new_proofs = self.gather_proofs(new_blocks).await?;

            let best_tip = self
                .with_cancellation(|api| async move { api.best_block_tip().await })
                .await?;

            if best_tip != tip {
                // In case when new block arrived or reorg occurred when update plan was being prepared
                tip = best_tip;
                continue;
            }

            let proof_update = self.commit_update_plan(update_plan, &new_proofs).await;
            update_planner.mark_applied(tip);
            self.publish(proof_update);

            return Ok(tip);
        }
    }

    async fn wait_for_retry(&self) -> bool {
        tokio::select! {
            _ = self.cancellation_token.cancelled() => false,
            _ = sleep(RETRY_TIMEOUT) => true,
        }
    }

    async fn bootstrap(&self) -> WorkerResult<BtcBlockTip> {
        loop {
            let initial_tip = self
                .with_cancellation(|api| async move { api.best_block_tip().await })
                .await?;
            let blocks = self.recent_chain(initial_tip).await?;
            let proofs = self.gather_proofs(&blocks).await?;

            let best_tip = self
                .with_cancellation(|api| async move { api.best_block_tip().await })
                .await?;

            if best_tip != initial_tip {
                continue;
            }

            if self.cancellation_token.is_cancelled() {
                return Err(IndexerWorkerError::Cancelled);
            }

            // Replacing proofs for the same block hashes makes bootstrap idempotent on restart.
            let indexed_block_hashes = blocks.iter().map(|block| block.hash).collect::<Vec<_>>();
            self.storage
                .apply_chain_update(&indexed_block_hashes, &proofs)
                .await;
            let new_proofs = group_proofs(&proofs);
            if !new_proofs.is_empty() {
                self.publish(MintingProofUpdate::NewBlocks(new_proofs));
            }

            return Ok(initial_tip);
        }
    }

    async fn recent_chain(&self, tip: BtcBlockTip) -> WorkerResult<Vec<BtcBlockTip>> {
        let mut header = self
            .with_cancellation(|api| async move { api.block_header_info(tip.hash).await })
            .await?;

        let mut blocks_reverse = Vec::with_capacity(BOOTSTRAP_INDEX_BLOCKS);
        blocks_reverse.push(header.tip);

        let Some(initial_block_hash) = header.previous_block_hash else {
            return Ok(blocks_reverse);
        };

        let mut block_hash = initial_block_hash;
        for _ in 0..BOOTSTRAP_INDEX_BLOCKS - 1 {
            header = self
                .with_cancellation(|api| async move { api.block_header_info(block_hash).await })
                .await?;
            blocks_reverse.push(header.tip);
            let Some(previous_hash) = header.previous_block_hash else {
                break;
            };
            block_hash = previous_hash;
        }

        blocks_reverse.reverse();
        Ok(blocks_reverse)
    }

    async fn gather_proofs(&self, blocks: &[BtcBlockTip]) -> WorkerResult<Vec<MintingProof>> {
        let requests = blocks
            .iter()
            .copied()
            .map(|block| async move {
                let transactions = self.rpc_api.transactions_in_block(block.hash).await?;
                Ok::<_, BitcoinRpcError>(get_minting_proofs_from_transactions(
                    &transactions,
                    block,
                    self.network_id,
                ))
            })
            .collect::<Vec<_>>();

        let proofs_by_block = self
            .with_cancellation(|_| async move { try_join_all(requests).await })
            .await?;

        Ok(proofs_by_block.into_iter().flatten().collect())
    }

    async fn commit_update_plan(
        &self,
        update_plan: ChainUpdatePlan,
        new_proofs: &[MintingProof],
    ) -> MintingProofUpdate {
        let discarded_block_hashes = match &update_plan {
            ChainUpdatePlan::Extension { .. } => Vec::new(),
            ChainUpdatePlan::Reorg {
                discarded_blocks, ..
            } => discarded_blocks
                .iter()
                .map(|block| block.hash)
                .collect::<Vec<_>>(),
        };
        let deleted_proofs = self
            .storage
            .apply_chain_update(&discarded_block_hashes, new_proofs)
            .await;
        let grouped_new_proofs = group_proofs(new_proofs);

        match update_plan {
            ChainUpdatePlan::Extension { .. } => MintingProofUpdate::NewBlocks(grouped_new_proofs),
            ChainUpdatePlan::Reorg { .. } => MintingProofUpdate::Reorg {
                deleted_proofs,
                new_proofs: grouped_new_proofs,
            },
        }
    }

    async fn with_cancellation<F, Fut, T>(&self, f: F) -> WorkerResult<T>
    where
        F: FnOnce(Arc<R>) -> Fut,
        Fut: Future<Output = Result<T, BitcoinRpcError>>,
    {
        tokio::select! {
            _ = self.cancellation_token.cancelled() => {
                Err(IndexerWorkerError::Cancelled)
            },
            result = f(self.rpc_api.clone()) => Ok(result?),
        }
    }

    fn publish(&self, update: MintingProofUpdate) {
        let _ = self.subscribers.send(Arc::new(update));
    }
}

fn group_proofs(proofs: &[MintingProof]) -> NewMintingProofs {
    let mut grouped = NewMintingProofs::new();

    for proof in proofs {
        grouped
            .entry(proof.minting_proof_data.flame_block_hash)
            .or_default()
            .push(proof.clone());
    }

    grouped
}

fn get_minting_proofs_from_transactions(
    transactions: &[Transaction],
    block_tip: BtcBlockTip,
    network_id: u8,
) -> Vec<MintingProof> {
    transactions
        .iter()
        .flat_map(|transaction| {
            transaction.output.iter().filter_map(|output| {
                MintingProofData::from_tx_out(output)
                    .filter(|minting_proof_data| minting_proof_data.network_id == network_id)
                    .map(|minting_proof_data| MintingProof {
                        minting_proof_data,
                        burned_amount: output.value,
                        bitcoin_block_tip: block_tip,
                    })
            })
        })
        .collect::<Vec<_>>()
}

#[cfg(test)]
#[path = "unit-tests/worker_tests.rs"]
mod tests;
