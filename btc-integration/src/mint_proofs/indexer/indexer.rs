use corepc_client::client_sync::{Error, Result as RpcResult};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::MintingProof;
use crate::mint_proofs::indexer::worker::IndexerWorker;
use crate::mint_proofs::minting_proof_storage::MintingProofStorage;
use crate::rpc::RpcApi;

pub type NewMintingProofs = BTreeMap<[u8; 32], Vec<MintingProof>>;

struct IndexerWorkerHandle {
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<RpcResult<()>>,
}

pub struct MintProofIndexer<R, S> {
    rpc: Arc<R>,
    storage: Arc<Mutex<S>>,
    subscribers: broadcast::Sender<Arc<NewMintingProofs>>,
    worker: Mutex<Option<IndexerWorkerHandle>>,
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
        let mut worker_slot = self.worker.lock().await;
        if worker_slot.is_some() {
            // TODO: better error
            return Err(Error::Returned(
                "mint-proof indexer is already running".to_owned(),
            ));
        }

        let cancellation_token = CancellationToken::new();
        let indexer_worker = IndexerWorker::new(
            Arc::clone(&self.rpc),
            Arc::clone(&self.storage),
            self.subscribers.clone(),
            cancellation_token.clone(),
        );

        let initial_tip = indexer_worker.bootstrap().await?;

        let join_handle = tokio::spawn(indexer_worker.run(initial_tip));
        *worker_slot = Some(IndexerWorkerHandle {
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
}

impl<R, S> Drop for MintProofIndexer<R, S> {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut().take() {
            worker.cancellation_token.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
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

    use super::*;
    use crate::mint_proofs::MintingProofData;
    use crate::mint_proofs::minting_proof_storage::InMemoryMintingProofStorage;
    use crate::mint_proofs::minting_proof_storage::MintingProof;
    use crate::rpc::BtcBlockTip;

    struct TestRpc {
        initial_tip: BtcBlockTip,
        next_tip: BtcBlockTip,
        wait_calls: AtomicUsize,
        transaction_calls: AtomicUsize,
        transactions_by_block: BTreeMap<BlockHash, Vec<Transaction>>,
    }

    #[async_trait]
    impl RpcApi for TestRpc {
        async fn block_tip(&self) -> Result<BtcBlockTip> {
            Ok(self.initial_tip)
        }

        async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>> {
            self.transaction_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .transactions_by_block
                .get(&block_hash)
                .cloned()
                .unwrap_or_default())
        }

        async fn transactions_at_height(
            &self,
            _height: u64,
        ) -> Result<(BlockHash, Vec<Transaction>)> {
            unreachable!("the test announces only the directly-adjacent block")
        }

        async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash> {
            Ok(block_hash(height))
        }

        async fn wait_for_next_block(&self, _prev_block: BtcBlockTip) -> Result<BtcBlockTip> {
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

    fn block_tip(hash_suffix: u8, height: u64) -> BtcBlockTip {
        BtcBlockTip {
            hash: block_hash(u64::from(hash_suffix)),
            height,
        }
    }

    fn block_hash(value: u64) -> BlockHash {
        BlockHash::from_str(&format!("{value:064x}")).expect("valid block hash")
    }

    fn proof(flame_block_hash: [u8; 32], burned_sats: u64, tip: BtcBlockTip) -> MintingProof {
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
            transaction_calls: AtomicUsize::new(0),
            transactions_by_block: BTreeMap::from([(next_tip.hash, transactions)]),
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

    #[tokio::test]
    async fn startup_bootstraps_current_and_previous_nineteen_blocks() {
        let initial_tip = block_tip(100, 25);
        let oldest_bootstrap_tip = BtcBlockTip {
            hash: block_hash(6),
            height: 6,
        };
        let excluded_tip = BtcBlockTip {
            hash: block_hash(5),
            height: 5,
        };
        let oldest = proof([0x11; 32], 1_000, oldest_bootstrap_tip);
        let current = proof([0x11; 32], 2_000, initial_tip);
        let excluded = proof([0x22; 32], 3_000, excluded_tip);
        let rpc = Arc::new(TestRpc {
            initial_tip,
            next_tip: initial_tip,
            wait_calls: AtomicUsize::new(0),
            transaction_calls: AtomicUsize::new(0),
            transactions_by_block: BTreeMap::from([
                (excluded_tip.hash, vec![transaction(&[excluded])]),
                (
                    oldest_bootstrap_tip.hash,
                    vec![transaction(std::slice::from_ref(&oldest))],
                ),
                (
                    initial_tip.hash,
                    vec![transaction(std::slice::from_ref(&current))],
                ),
            ]),
        });
        let indexer = MintProofIndexer::new(Arc::clone(&rpc), InMemoryMintingProofStorage::new());

        indexer.startup().await.expect("start indexer");

        assert_eq!(rpc.transaction_calls.load(Ordering::SeqCst), 20);
        assert_eq!(indexer.get_proofs([0x11; 32]).await, vec![oldest, current]);
        assert!(indexer.get_proofs([0x22; 32]).await.is_empty());

        indexer.shutdown().await.expect("shut down indexer");
    }
}
