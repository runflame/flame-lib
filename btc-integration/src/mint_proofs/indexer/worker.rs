use corepc_client::bitcoin::{BlockHash, Transaction};
use corepc_client::client_sync::{Error, Result as RpcResult};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::MintingProof;
use crate::mint_proofs::MintingProofData;
use crate::mint_proofs::indexer::bitcoin_chain_update_planner::{
    BitcoinChainUpdatePlanner, ChainUpdatePlan,
};
use crate::mint_proofs::indexer::indexer::{MintingProofUpdate, NewMintingProofs};
use crate::mint_proofs::minting_proof_storage::{MintingProofStorage, MintingProofsByBitcoinBlock};
use crate::rpc::{BtcBlockTip, RpcApi};

const BOOTSTRAP_INDEX_BLOCK: u64 = 20;

pub(super) struct IndexerWorker<R, S> {
    rpc_api: Arc<R>,
    storage: Arc<S>,
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
        subscribers: broadcast::Sender<Arc<MintingProofUpdate>>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            rpc_api,
            storage,
            subscribers,
            cancellation_token,
        }
    }

    pub(super) async fn run(self, initial_tip: BtcBlockTip) -> RpcResult<()>
    where
        R: 'static,
        S: 'static,
    {
        let mut applied_tip = initial_tip;
        let mut update_planner =
            BitcoinChainUpdatePlanner::new(initial_tip, Arc::clone(&self.rpc_api));

        loop {
            let announced_tip = tokio::select! {
                _ = self.cancellation_token.cancelled() => return Ok(()),
                result = self.rpc_api.wait_for_next_block(applied_tip) => result?,
            };

            applied_tip = self
                .handle_new_tip(announced_tip, &mut update_planner)
                .await?;
        }
    }

    async fn handle_new_tip(
        &self,
        new_tip: BtcBlockTip,
        update_planner: &mut BitcoinChainUpdatePlanner<R>,
    ) -> RpcResult<BtcBlockTip> {
        let mut tip = new_tip;
        loop {
            let update_plan = update_planner.plan_update(tip).await?;
            match update_plan {
                ChainUpdatePlan::Extension { .. } => {
                    self.apply_update_plan(update_plan).await?;
                    update_planner.mark_applied(tip);

                    return Ok(tip);
                }
                ChainUpdatePlan::Reorg { .. } => {
                    let best_tip = self.rpc_api.best_block_tip().await?;
                    if best_tip != tip {
                        // If we receive reorg during reorg handling, restart the process with the
                        // new best tip.
                        tip = best_tip;
                        continue;
                    } else {
                        self.apply_update_plan(update_plan).await?;
                        update_planner.mark_applied(tip);
                        return Ok(best_tip);
                    }
                }
            }
        }
    }

    pub(super) async fn bootstrap(&self) -> RpcResult<BtcBlockTip> {
        let initial_tip = self.rpc_api.best_block_tip().await?;
        let first_height = initial_tip.height.saturating_sub(BOOTSTRAP_INDEX_BLOCK - 1);

        for height in first_height..initial_tip.height {
            let hash = tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    return Err(Error::Returned(
                        "mint-proof indexer bootstrap was cancelled".to_owned(),
                    ));
                },
                result = self.rpc_api.block_hash_at_height(height) => result?,
            };
            let block_tip = BtcBlockTip { hash, height };

            let new_proofs = self.index_block(block_tip).await?;
            if !new_proofs.is_empty() {
                self.publish(MintingProofUpdate::NewBlocks(new_proofs));
            }
        }
        let new_proofs = self.index_block(initial_tip).await?;
        if !new_proofs.is_empty() {
            self.publish(MintingProofUpdate::NewBlocks(new_proofs));
        }

        Ok(initial_tip)
    }

    async fn index_blocks(&self, blocks: &[BtcBlockTip]) -> RpcResult<NewMintingProofs> {
        let mut all_new_proofs = NewMintingProofs::new();

        for block in blocks {
            merge_proofs(&mut all_new_proofs, self.index_block(*block).await?);
        }

        Ok(all_new_proofs)
    }

    async fn apply_update_plan(&self, update_plan: ChainUpdatePlan) -> RpcResult<()> {
        match update_plan {
            ChainUpdatePlan::Extension { new_blocks } => {
                let new_proofs = self.index_blocks(&new_blocks).await?;
                if !new_proofs.is_empty() {
                    self.publish(MintingProofUpdate::NewBlocks(new_proofs));
                }
            }
            ChainUpdatePlan::Reorg {
                discarded_blocks,
                new_blocks,
            } => {
                let discarded_hashes = discarded_blocks
                    .iter()
                    .map(|block| block.hash)
                    .collect::<Vec<_>>();
                let deleted_proofs = self.delete_proofs(&discarded_hashes).await;
                let new_proofs = self.index_blocks(&new_blocks).await?;

                self.publish(MintingProofUpdate::Reorg {
                    deleted_proofs,
                    new_proofs,
                });
            }
        }

        Ok(())
    }

    async fn index_block(&self, block_tip: BtcBlockTip) -> RpcResult<NewMintingProofs> {
        let transactions = tokio::select! {
            _ = self.cancellation_token.cancelled() => return Ok(NewMintingProofs::new()),
            result = self.rpc_api.transactions_in_block(block_tip.hash) => result?,
        };
        let proofs = get_minting_proofs_from_transactions(&transactions, block_tip);
        let mut new_proofs = NewMintingProofs::new();

        for proof in proofs {
            let flame_block_hash = proof.minting_proof_data.flame_block_hash;
            self.storage.insert(flame_block_hash, proof.clone()).await;
            new_proofs.entry(flame_block_hash).or_default().push(proof);
        }

        Ok(new_proofs)
    }

    async fn delete_proofs(&self, block_hashes: &[BlockHash]) -> MintingProofsByBitcoinBlock {
        self.storage.remove_by_bitcoin_blocks(block_hashes).await
    }

    fn publish(&self, update: MintingProofUpdate) {
        let _ = self.subscribers.send(Arc::new(update));
    }
}

fn merge_proofs(target: &mut NewMintingProofs, proofs: NewMintingProofs) {
    for (flame_block_hash, proofs) in proofs {
        target.entry(flame_block_hash).or_default().extend(proofs);
    }
}

fn get_minting_proofs_from_transactions(
    transactions: &[Transaction],
    block_tip: BtcBlockTip,
) -> Vec<MintingProof> {
    transactions
        .iter()
        .flat_map(|transaction| {
            transaction.output.iter().filter_map(|output| {
                MintingProofData::from_tx_out(output).map(|minting_proof_data| MintingProof {
                    minting_proof_data,
                    burned_amount: output.value,
                    bitcoin_block_tip: block_tip,
                })
            })
        })
        .collect::<Vec<_>>()
}
