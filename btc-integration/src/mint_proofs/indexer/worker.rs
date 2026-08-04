use corepc_client::bitcoin::Transaction;
use corepc_client::client_sync::{Error, Result as RpcResult};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;

use crate::mint_proofs::indexer::indexer::NewMintingProofs;
use crate::{BlockTip, MintingProof, MintingProofData, MintingProofStorage, RpcApi};

const BOOTSTRAP_INDEX_BLOCK: u64 = 20;

pub(super) struct IndexerWorker<R, S> {
    rpc_api: Arc<R>,
    storage: Arc<Mutex<S>>,
    subscribers: broadcast::Sender<Arc<NewMintingProofs>>,
    cancellation_token: CancellationToken,
}

impl<R, S> IndexerWorker<R, S>
where
    R: RpcApi,
    S: MintingProofStorage,
{
    pub(super) fn new(
        rpc_api: Arc<R>,
        storage: Arc<Mutex<S>>,
        subscribers: broadcast::Sender<Arc<NewMintingProofs>>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            rpc_api,
            storage,
            subscribers,
            cancellation_token,
        }
    }

    pub(super) async fn run(self, initial_tip: BlockTip) -> RpcResult<()>
    where
        R: 'static,
        S: Send + 'static,
    {
        let mut previous_tip = initial_tip;

        loop {
            let announced_tip = tokio::select! {
                _ = self.cancellation_token.cancelled() => return Ok(()),
                result = self.rpc_api.wait_for_next_block(previous_tip) => result?,
            };

            if announced_tip.height > previous_tip.height {
                for height in (previous_tip.height + 1)..announced_tip.height {
                    let hash = tokio::select! {
                        _ = self.cancellation_token.cancelled() => return Ok(()),
                        result = self.rpc_api.block_hash_at_height(height) => result?,
                    };
                    let block_tip = BlockTip { hash, height };

                    self.index_block(block_tip).await?
                }

                self.index_block(announced_tip).await?
            } else if announced_tip.hash != previous_tip.hash {
                // TODO: handle reorgs
                panic!("reorg")
            }

            previous_tip = announced_tip;
        }
    }

    pub(super) async fn bootstrap(&self) -> RpcResult<BlockTip> {
        let initial_tip = self.rpc_api.block_tip().await?;
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
            let block_tip = BlockTip { hash, height };

            self.index_block(block_tip).await?;
        }
        self.index_block(initial_tip).await?;

        Ok(initial_tip)
    }

    async fn index_block(&self, block_tip: BlockTip) -> RpcResult<()> {
        let transactions = tokio::select! {
            _ = self.cancellation_token.cancelled() => return Ok(()),
            result = self.rpc_api.transactions_in_block(block_tip.hash) => result?,
        };
        let proofs = get_minting_proofs_from_transactions(&transactions, block_tip);
        let mut new_proofs = NewMintingProofs::new();

        {
            let mut storage = self.storage.lock().await;

            for proof in proofs {
                let flame_block_hash = proof.minting_proof_data.flame_block_hash;
                storage.insert(flame_block_hash, proof.clone());
                new_proofs.entry(flame_block_hash).or_default().push(proof);
            }
        }

        if new_proofs.is_empty() {
            return Ok(());
        }

        let _ = self.subscribers.send(Arc::new(new_proofs));

        Ok(())
    }
}

fn get_minting_proofs_from_transactions(
    transactions: &[Transaction],
    block_tip: BlockTip,
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
