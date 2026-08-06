//! Mint Proof Indexer is a background task that performs 2 key functions:
//! - Indexing minting proofs
//! - Indexing reorgs
//!
//! The service does not have any persistent state, as only last 10 bitcoin blocks are usually needed
//! to be indexed. It is the task of indexer user to store the last bitcoin tip and pass it to the
//! indexer on startup. This ensures that only one btc block is used in the node, and there is no
//! conflict between indexer btc block tip and indexer user btc block tip.

use corepc_client::client_sync::{Error, Result as RpcResult};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::MintingProof;
use crate::mint_proofs::indexer::worker::IndexerWorker;
use crate::mint_proofs::minting_proof_storage::{MintingProofStorage, MintingProofsByBitcoinBlock};
use crate::rpc::RpcApi;

pub type NewMintingProofs = BTreeMap<[u8; 32], Vec<MintingProof>>;

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
    join_handle: JoinHandle<RpcResult<()>>,
}

pub struct MintProofIndexer<R, S> {
    rpc: Arc<R>,
    storage: Arc<S>,
    subscribers: broadcast::Sender<Arc<MintingProofUpdate>>,
    worker: Mutex<Option<IndexerWorkerHandle>>,
}

impl<R, S> MintProofIndexer<R, S>
where
    R: RpcApi,
    S: MintingProofStorage,
{
    pub fn new(rpc: Arc<R>, storage: Arc<S>) -> Self {
        let (subscribers, _) = broadcast::channel(16);

        Self {
            rpc,
            storage,
            subscribers,
            worker: Mutex::new(None),
        }
    }

    pub async fn get_proofs(&self, flame_block_hash: [u8; 32]) -> Vec<MintingProof> {
        self.storage.get(flame_block_hash).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<MintingProofUpdate>> {
        self.subscribers.subscribe()
    }

    pub async fn startup(&self) -> RpcResult<()>
    where
        R: 'static,
        S: 'static,
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
    use ed25519_dalek::SigningKey;
    use flamevm::Predicate;

    use super::*;
    use crate::mint_proofs::MintingProofData;
    use crate::mint_proofs::minting_proof_storage::InMemoryMintingProofStorage;
    use crate::mint_proofs::minting_proof_storage::MintingProof;
    use crate::rpc::{BtcBlockHeaderInfo, BtcBlockTip};

    struct TestRpc {
        initial_tip: BtcBlockTip,
        next_tip: BtcBlockTip,
        best_tip: BtcBlockTip,
        block_tip_calls: AtomicUsize,
        wait_calls: AtomicUsize,
        transaction_calls: AtomicUsize,
        transactions_by_block: BTreeMap<BlockHash, Vec<Transaction>>,
        headers: BTreeMap<BlockHash, BtcBlockHeaderInfo>,
    }

    #[async_trait]
    impl RpcApi for TestRpc {
        async fn best_block_tip(&self) -> Result<BtcBlockTip> {
            if self.block_tip_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(self.initial_tip)
            } else {
                Ok(self.best_tip)
            }
        }

        async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo> {
            self.headers.get(&block_hash).copied().ok_or_else(|| {
                Error::Returned(format!("test block header {block_hash} was not found"))
            })
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

    fn header(tip: BtcBlockTip, previous_block_hash: Option<BlockHash>) -> BtcBlockHeaderInfo {
        BtcBlockHeaderInfo {
            tip,
            previous_block_hash,
        }
    }

    fn proof(flame_block_hash: [u8; 32], burned_sats: u64, tip: BtcBlockTip) -> MintingProof {
        MintingProof {
            minting_proof_data: MintingProofData {
                network_id: 7,
                flame_block_hash,
                flame_reward_address: Predicate::opaque(Predicate::unspendable_key()),
                validator_pubkey: Some(SigningKey::from_bytes(&[7; 32]).verifying_key()),
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
            best_tip: initial_tip,
            block_tip_calls: AtomicUsize::new(0),
            wait_calls: AtomicUsize::new(0),
            transaction_calls: AtomicUsize::new(0),
            transactions_by_block: BTreeMap::from([(next_tip.hash, transactions)]),
            headers: BTreeMap::from([
                (initial_tip.hash, header(initial_tip, Some(block_hash(0)))),
                (next_tip.hash, header(next_tip, Some(initial_tip.hash))),
            ]),
        });
        let indexer = MintProofIndexer::new(rpc, Arc::new(InMemoryMintingProofStorage::new()));
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
            notification.as_ref(),
            &MintingProofUpdate::NewBlocks({
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
            best_tip: initial_tip,
            block_tip_calls: AtomicUsize::new(0),
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
            headers: BTreeMap::from([(
                initial_tip.hash,
                header(initial_tip, Some(block_hash(99))),
            )]),
        });
        let indexer = MintProofIndexer::new(
            Arc::clone(&rpc),
            Arc::new(InMemoryMintingProofStorage::new()),
        );

        indexer.startup().await.expect("start indexer");

        assert_eq!(rpc.transaction_calls.load(Ordering::SeqCst), 20);
        assert_eq!(indexer.get_proofs([0x11; 32]).await, vec![oldest, current]);
        assert!(indexer.get_proofs([0x22; 32]).await.is_empty());

        indexer.shutdown().await.expect("shut down indexer");
    }

    #[tokio::test]
    async fn reorg_deletes_discarded_proofs_and_publishes_replacement_proofs() {
        let common_ancestor = block_tip(0, 0);
        let old_tip = block_tip(11, 1);
        let new_block = block_tip(21, 1);
        let new_tip = block_tip(22, 2);
        let discarded = proof([0x11; 32], 1_000, old_tip);
        let replacement = proof([0x11; 32], 2_000, new_block);
        let additional = proof([0x22; 32], 3_000, new_tip);
        let rpc = Arc::new(TestRpc {
            initial_tip: old_tip,
            next_tip: new_tip,
            best_tip: new_tip,
            block_tip_calls: AtomicUsize::new(0),
            wait_calls: AtomicUsize::new(0),
            transaction_calls: AtomicUsize::new(0),
            transactions_by_block: BTreeMap::from([
                (
                    old_tip.hash,
                    vec![transaction(std::slice::from_ref(&discarded))],
                ),
                (
                    new_block.hash,
                    vec![transaction(std::slice::from_ref(&replacement))],
                ),
                (
                    new_tip.hash,
                    vec![transaction(std::slice::from_ref(&additional))],
                ),
            ]),
            headers: BTreeMap::from([
                (common_ancestor.hash, header(common_ancestor, None)),
                (old_tip.hash, header(old_tip, Some(common_ancestor.hash))),
                (
                    new_block.hash,
                    header(new_block, Some(common_ancestor.hash)),
                ),
                (new_tip.hash, header(new_tip, Some(new_block.hash))),
            ]),
        });
        let indexer = MintProofIndexer::new(rpc, Arc::new(InMemoryMintingProofStorage::new()));
        let mut notifications = indexer.subscribe();

        indexer.startup().await.expect("start indexer");
        let bootstrap_update = notifications.recv().await.expect("bootstrap update");
        assert_eq!(
            bootstrap_update.as_ref(),
            &MintingProofUpdate::NewBlocks(BTreeMap::from([
                ([0x11; 32], vec![discarded.clone()],)
            ]))
        );

        let reorg_update = tokio::time::timeout(Duration::from_secs(1), notifications.recv())
            .await
            .expect("reorg notification timeout")
            .expect("reorg notification");
        assert_eq!(
            reorg_update.as_ref(),
            &MintingProofUpdate::Reorg {
                deleted_proofs: BTreeMap::from([(old_tip.hash, vec![discarded])]),
                new_proofs: BTreeMap::from([
                    ([0x11; 32], vec![replacement.clone()]),
                    ([0x22; 32], vec![additional.clone()]),
                ]),
            }
        );
        assert_eq!(indexer.get_proofs([0x11; 32]).await, vec![replacement]);
        assert_eq!(indexer.get_proofs([0x22; 32]).await, vec![additional]);

        indexer.shutdown().await.expect("shut down indexer");
    }

    #[tokio::test]
    async fn rechecks_the_best_tip_when_another_reorg_occurs_during_handling() {
        let common_ancestor = block_tip(0, 0);
        let old_tip = block_tip(11, 1);
        let announced_block = block_tip(21, 1);
        let announced_tip = block_tip(22, 2);
        let best_block = block_tip(31, 1);
        let best_middle = block_tip(32, 2);
        let best_tip = block_tip(33, 3);
        let old_proof = proof([0x11; 32], 1_000, old_tip);
        let superseded_proof = proof([0x11; 32], 2_000, announced_block);
        let winning_proof = proof([0x11; 32], 3_000, best_block);
        let additional_proof = proof([0x22; 32], 4_000, best_tip);
        let rpc = Arc::new(TestRpc {
            initial_tip: old_tip,
            next_tip: announced_tip,
            best_tip,
            block_tip_calls: AtomicUsize::new(0),
            wait_calls: AtomicUsize::new(0),
            transaction_calls: AtomicUsize::new(0),
            transactions_by_block: BTreeMap::from([
                (
                    old_tip.hash,
                    vec![transaction(std::slice::from_ref(&old_proof))],
                ),
                (
                    announced_block.hash,
                    vec![transaction(std::slice::from_ref(&superseded_proof))],
                ),
                (
                    best_block.hash,
                    vec![transaction(std::slice::from_ref(&winning_proof))],
                ),
                (
                    best_tip.hash,
                    vec![transaction(std::slice::from_ref(&additional_proof))],
                ),
            ]),
            headers: BTreeMap::from([
                (common_ancestor.hash, header(common_ancestor, None)),
                (old_tip.hash, header(old_tip, Some(common_ancestor.hash))),
                (
                    announced_block.hash,
                    header(announced_block, Some(common_ancestor.hash)),
                ),
                (
                    announced_tip.hash,
                    header(announced_tip, Some(announced_block.hash)),
                ),
                (
                    best_block.hash,
                    header(best_block, Some(common_ancestor.hash)),
                ),
                (best_middle.hash, header(best_middle, Some(best_block.hash))),
                (best_tip.hash, header(best_tip, Some(best_middle.hash))),
            ]),
        });
        let indexer = MintProofIndexer::new(
            Arc::clone(&rpc),
            Arc::new(InMemoryMintingProofStorage::new()),
        );
        let mut notifications = indexer.subscribe();

        indexer.startup().await.expect("start indexer");
        notifications.recv().await.expect("bootstrap update");

        let reorg_update = tokio::time::timeout(Duration::from_secs(1), notifications.recv())
            .await
            .expect("reorg notification timeout")
            .expect("reorg notification");
        assert_eq!(
            reorg_update.as_ref(),
            &MintingProofUpdate::Reorg {
                deleted_proofs: BTreeMap::from([(old_tip.hash, vec![old_proof])]),
                new_proofs: BTreeMap::from([
                    ([0x11; 32], vec![winning_proof.clone()]),
                    ([0x22; 32], vec![additional_proof.clone()]),
                ]),
            }
        );
        assert_eq!(indexer.get_proofs([0x11; 32]).await, vec![winning_proof]);
        assert_eq!(indexer.get_proofs([0x22; 32]).await, vec![additional_proof]);
        assert!(
            !indexer
                .get_proofs([0x11; 32])
                .await
                .contains(&superseded_proof)
        );
        assert_eq!(rpc.block_tip_calls.load(Ordering::SeqCst), 3);

        indexer.shutdown().await.expect("shut down indexer");
    }
}
