use corepc_client::bitcoin::Transaction;
use corepc_client::client_sync::{Error, Result as RpcResult};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{BlockTip, MintingProof, MintingProofData, MintingProofStorage, RpcApi};

pub type NewMintingProofs = BTreeMap<[u8; 32], Vec<MintingProof>>;

struct IndexerWorker {
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<RpcResult<()>>,
}

pub struct MintProofIndexer<R, S> {
    rpc: Arc<R>,
    storage: Arc<Mutex<S>>,
    subscribers: broadcast::Sender<Arc<NewMintingProofs>>,
    worker: Mutex<Option<IndexerWorker>>,
}

impl<R, S> MintProofIndexer<R, S>
where
    R: RpcApi,
    S: MintingProofStorage,
{
    pub fn new(rpc: Arc<R>, storage: S) -> Self {
        let (subscribers, _) = broadcast::channel(16);

        Self {
            rpc,
            storage: Arc::new(Mutex::new(storage)),
            subscribers,
            worker: Mutex::new(None),
        }
    }

    pub async fn get_proofs(&self, flame_block_hash: [u8; 32]) -> Vec<MintingProof> {
        self.storage.lock().await.get(flame_block_hash)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<NewMintingProofs>> {
        self.subscribers.subscribe()
    }

    pub async fn startup(&self) -> RpcResult<()>
    where
        R: 'static,
        S: Send + 'static,
    {
        let mut worker = self.worker.lock().await;
        if worker.is_some() {
            // TODO: better error
            return Err(Error::Returned(
                "mint-proof indexer is already running".to_owned(),
            ));
        }

        let rpc = Arc::clone(&self.rpc);
        let storage = Arc::clone(&self.storage);
        let subscribers = self.subscribers.clone();
        let cancellation_token = CancellationToken::new();
        let worker_cancellation_token = cancellation_token.clone();

        let join_handle = tokio::spawn(async move {
            Self::run(rpc, storage, subscribers, worker_cancellation_token).await
        });
        *worker = Some(IndexerWorker {
            cancellation_token,
            join_handle,
        });

        Ok(())
    }

    pub async fn shutdown(&self) -> RpcResult<()> {
        let mut worker_slot = self.worker.lock().await;
        let Some(worker) = worker_slot.take() else {
            return Ok(());
        };

        worker.cancellation_token.cancel();
        let result = worker
            .join_handle
            .await
            .map_err(|error| Error::Returned(format!("mint-proof indexer task failed: {error}")))?;
        drop(worker_slot);

        result
    }

    async fn run(
        rpc: Arc<R>,
        storage: Arc<Mutex<S>>,
        subscribers: broadcast::Sender<Arc<NewMintingProofs>>,
        cancellation_token: CancellationToken,
    ) -> RpcResult<()>
    where
        R: 'static,
        S: Send + 'static,
    {
        let initial_tip = tokio::select! {
            _ = cancellation_token.cancelled() => return Ok(()),
            result = rpc.block_tip() => result?,
        };

        let mut previous_tip = initial_tip;

        loop {
            let announced_tip = tokio::select! {
                _ = cancellation_token.cancelled() => return Ok(()),
                result = rpc.wait_for_next_block(previous_tip) => result?,
            };

            if announced_tip.height > previous_tip.height {
                for height in (previous_tip.height + 1)..announced_tip.height {
                    let hash = tokio::select! {
                        _ = cancellation_token.cancelled() => return Ok(()),
                        result = rpc.block_hash_at_height(height) => result?,
                    };
                    let block_tip = BlockTip { hash, height };

                    Self::index_block(&rpc, &storage, &subscribers, block_tip, &cancellation_token)
                        .await?
                }

                Self::index_block(
                    &rpc,
                    &storage,
                    &subscribers,
                    announced_tip,
                    &cancellation_token,
                )
                .await?
            } else if announced_tip.hash != previous_tip.hash {
                // TODO: handle reorgs
                panic!("reorg")
            }

            previous_tip = announced_tip;
        }
    }

    async fn index_block(
        rpc: &Arc<R>,
        storage: &Arc<Mutex<S>>,
        subscribers: &broadcast::Sender<Arc<NewMintingProofs>>,
        block_tip: BlockTip,
        cancellation_token: &CancellationToken,
    ) -> RpcResult<()> {
        let transactions = tokio::select! {
            _ = cancellation_token.cancelled() => return Ok(()),
            result = rpc.transactions_in_block(block_tip.hash) => result?,
        };
        let proofs = get_minting_proofs_from_transactions(&transactions, block_tip);
        let mut new_proofs = NewMintingProofs::new();

        {
            let mut storage = storage.lock().await;

            for proof in proofs {
                let flame_block_hash = proof.minting_proof_data.flame_block_hash;
                storage.insert(flame_block_hash, proof.clone());
                new_proofs.entry(flame_block_hash).or_default().push(proof);
            }
        }

        if new_proofs.is_empty() {
            return Ok(());
        }

        let _ = subscribers.send(Arc::new(new_proofs));

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

#[cfg(test)]
mod tests {
    use std::{
        future,
        str::FromStr,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use async_trait::async_trait;
    use corepc_client::{
        bitcoin::{Amount, BlockHash, Transaction, TxOut, Txid, absolute, transaction},
        client_sync::Result,
    };

    use crate::{
        InMemoryMintingProofStorage, MintingProofData, RpcApi,
        mint_proofs::minting_proof_storage::MintingProof,
    };

    use super::*;

    struct TestRpc {
        initial_tip: BlockTip,
        next_tip: BlockTip,
        wait_calls: AtomicUsize,
        transactions: Vec<Transaction>,
    }

    #[async_trait]
    impl RpcApi for TestRpc {
        async fn block_tip(&self) -> Result<BlockTip> {
            Ok(self.initial_tip)
        }

        async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>> {
            assert_eq!(block_hash, self.next_tip.hash);
            Ok(self.transactions.clone())
        }

        async fn transactions_at_height(
            &self,
            _height: u64,
        ) -> Result<(BlockHash, Vec<Transaction>)> {
            unreachable!("the test announces only the directly-adjacent block")
        }

        async fn block_hash_at_height(&self, _height: u64) -> Result<BlockHash> {
            unreachable!("the test announces only the directly-adjacent block")
        }

        async fn wait_for_next_block(&self, _prev_block: BlockTip) -> Result<BlockTip> {
            if self.wait_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(self.next_tip)
            } else {
                future::pending().await
            }
        }

        async fn publish_transaction(&self, _signed_transaction: &Transaction) -> Result<Txid> {
            unreachable!("publishing is unrelated to indexing")
        }

        async fn fund_and_sign_transaction(
            &self,
            _transaction: &Transaction,
        ) -> Result<Transaction> {
            unreachable!("publishing is unrelated to indexing")
        }

        async fn publish_mint_transaction(
            &self,
            _transaction: &Transaction,
            _max_burn_amount: Amount,
        ) -> Result<Txid> {
            unreachable!("publishing is unrelated to indexing")
        }
    }

    fn block_tip(hash_suffix: u8, height: u64) -> BlockTip {
        BlockTip {
            hash: BlockHash::from_str(&format!("{:064x}", hash_suffix)).expect("valid block hash"),
            height,
        }
    }

    fn proof(flame_block_hash: [u8; 32], burned_sats: u64, tip: BlockTip) -> MintingProof {
        MintingProof {
            minting_proof_data: MintingProofData {
                network_id: 7,
                flame_block_hash,
                want_participate_in_consensus: true,
            },
            burned_amount: Amount::from_sat(burned_sats),
            bitcoin_block_tip: tip,
        }
    }

    fn transaction(proofs: &[MintingProof]) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: proofs
                .iter()
                .map(|proof| TxOut {
                    value: proof.burned_amount,
                    script_pubkey: proof.minting_proof_data.to_script(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn startup_stores_and_publishes_new_proofs_grouped_by_flame_hash() {
        let initial_tip = block_tip(1, 100);
        let next_tip = block_tip(2, 101);
        let first = proof([0x11; 32], 1_000, next_tip);
        let second = proof([0x11; 32], 2_000, next_tip);
        let other = proof([0x22; 32], 3_000, next_tip);
        let transactions = vec![transaction(&[first.clone(), second.clone(), other.clone()])];
        let rpc = Arc::new(TestRpc {
            initial_tip,
            next_tip,
            wait_calls: AtomicUsize::new(0),
            transactions,
        });
        let indexer = MintProofIndexer::new(rpc, InMemoryMintingProofStorage::new());
        let mut received_notifications = indexer.subscribe();

        indexer.startup().await.unwrap();
        let duplicate_start_error = indexer
            .startup()
            .await
            .expect_err("reject duplicate startup");
        assert!(
            duplicate_start_error
                .to_string()
                .contains("already running")
        );

        let notification =
            tokio::time::timeout(Duration::from_secs(1), received_notifications.recv())
                .await
                .expect("proof notification timeout")
                .expect("proof notification");
        tokio::time::timeout(Duration::from_secs(1), indexer.shutdown())
            .await
            .expect("indexer shutdown timeout")
            .expect("shut down indexer");
        indexer.shutdown().await.expect("shutdown is idempotent");
        indexer.startup().await.expect("restart indexer");
        indexer
            .shutdown()
            .await
            .expect("shut down restarted indexer");

        assert_eq!(
            indexer.get_proofs([0x11; 32]).await,
            vec![first.clone(), second.clone()]
        );
        assert_eq!(indexer.get_proofs([0x22; 32]).await, vec![other.clone()]);
        assert_eq!(
            notification,
            Arc::new({
                let mut expected = NewMintingProofs::new();
                expected.insert([0x11; 32], vec![first, second]);
                expected.insert([0x22; 32], vec![other]);
                expected
            })
        );
    }
}
