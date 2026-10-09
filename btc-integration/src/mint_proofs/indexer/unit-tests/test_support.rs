use std::{
    collections::{BTreeMap, HashMap, HashSet},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use corepc_client::{
    bitcoin::{Amount, BlockHash, Transaction, TxOut, Txid, absolute, transaction},
    client_sync::{Error, Result},
};
use ed25519_dalek::SigningKey;
use flamechain::{BlockHash as FlameBlockHash, FlameNetwork};
use flamevm::Predicate;
use tokio::sync::{Mutex, RwLock, Semaphore, broadcast, mpsc, watch};

use super::indexer::MintingProofUpdate;
use crate::{
    MintingProof,
    mint_proofs::MintingProofData,
    rpc::{BtcBlockHeaderInfo, BtcBlockTip, RpcApi},
};

pub struct TestChain {
    headers: BTreeMap<BlockHash, BtcBlockHeaderInfo>,
    transactions: BTreeMap<BlockHash, Vec<Transaction>>,
}

impl TestChain {
    pub fn new() -> Self {
        Self {
            headers: BTreeMap::new(),
            transactions: BTreeMap::new(),
        }
    }

    pub fn linear(first_height: u64, last_height: u64) -> Self {
        (first_height..=last_height).fold(Self::new(), |chain, height| {
            chain.block(
                block_tip(height, height),
                height.checked_sub(1).map(block_hash),
                &[],
            )
        })
    }

    pub fn block(
        mut self,
        tip: BtcBlockTip,
        previous_block_hash: Option<BlockHash>,
        proofs: &[MintingProof],
    ) -> Self {
        self.headers.insert(
            tip.hash,
            BtcBlockHeaderInfo {
                tip,
                previous_block_hash,
            },
        );
        if !proofs.is_empty() {
            self.transactions
                .insert(tip.hash, vec![transaction(proofs)]);
        }
        self
    }

    pub fn proofs(mut self, tip: BtcBlockTip, proofs: &[MintingProof]) -> Self {
        self.transactions
            .insert(tip.hash, vec![transaction(proofs)]);
        self
    }
}

struct TransactionGate {
    requested: watch::Sender<bool>,
    permit: Semaphore,
}

#[derive(Clone)]
pub struct TransactionBlocker {
    gate: Arc<TransactionGate>,
}

impl TransactionBlocker {
    pub async fn wait_until_requested(&self) {
        let mut requested = self.gate.requested.subscribe();
        while !*requested.borrow() {
            requested
                .changed()
                .await
                .expect("transaction request sender remains alive");
        }
    }

    pub fn release(&self) {
        self.gate.permit.add_permits(1);
    }
}

pub struct FakeRpc {
    best_tip: RwLock<BtcBlockTip>,
    headers: BTreeMap<BlockHash, BtcBlockHeaderInfo>,
    transactions: BTreeMap<BlockHash, Vec<Transaction>>,
    announcements: mpsc::UnboundedSender<BtcBlockTip>,
    announcement_receiver: Mutex<mpsc::UnboundedReceiver<BtcBlockTip>>,
    failing_blocks: RwLock<HashSet<BlockHash>>,
    transaction_calls: Mutex<HashMap<BlockHash, usize>>,
    transaction_gates: RwLock<HashMap<BlockHash, Arc<TransactionGate>>>,
}

impl FakeRpc {
    pub fn new(chain: TestChain, best_tip: BtcBlockTip) -> Arc<Self> {
        let (announcements, announcement_receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            best_tip: RwLock::new(best_tip),
            headers: chain.headers,
            transactions: chain.transactions,
            announcements,
            announcement_receiver: Mutex::new(announcement_receiver),
            failing_blocks: RwLock::new(HashSet::new()),
            transaction_calls: Mutex::new(HashMap::new()),
            transaction_gates: RwLock::new(HashMap::new()),
        })
    }

    pub async fn set_best_tip(&self, tip: BtcBlockTip) {
        *self.best_tip.write().await = tip;
    }

    pub fn announce(&self, tip: BtcBlockTip) {
        self.announcements
            .send(tip)
            .expect("test indexer is listening for block announcements");
    }

    pub async fn fail_transactions_for(&self, block_hash: BlockHash) {
        self.failing_blocks.write().await.insert(block_hash);
    }

    pub async fn allow_transactions_for(&self, block_hash: BlockHash) {
        self.failing_blocks.write().await.remove(&block_hash);
    }

    pub async fn wait_for_transaction_calls(&self, block_hash: BlockHash, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let calls = self
                    .transaction_calls
                    .lock()
                    .await
                    .get(&block_hash)
                    .copied()
                    .unwrap_or_default();
                if calls >= expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("transaction call timeout");
    }

    pub async fn block_transactions_for(&self, block_hash: BlockHash) -> TransactionBlocker {
        let (requested, _) = watch::channel(false);
        let gate = Arc::new(TransactionGate {
            requested,
            permit: Semaphore::new(0),
        });
        self.transaction_gates
            .write()
            .await
            .insert(block_hash, Arc::clone(&gate));
        TransactionBlocker { gate }
    }
}

#[async_trait]
impl RpcApi for FakeRpc {
    async fn best_block_tip(&self) -> Result<BtcBlockTip> {
        Ok(*self.best_tip.read().await)
    }

    async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo> {
        self.headers
            .get(&block_hash)
            .copied()
            .ok_or_else(|| Error::Returned(format!("test block header {block_hash} was not found")))
    }

    async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>> {
        *self
            .transaction_calls
            .lock()
            .await
            .entry(block_hash)
            .or_default() += 1;
        let gate = self
            .transaction_gates
            .read()
            .await
            .get(&block_hash)
            .cloned();
        if let Some(gate) = gate {
            gate.requested.send_replace(true);
            gate.permit
                .acquire()
                .await
                .expect("test transaction gate remains open")
                .forget();
        }
        if self.failing_blocks.read().await.contains(&block_hash) {
            return Err(Error::Returned(format!(
                "failed to fetch test block {block_hash}"
            )));
        }
        Ok(self
            .transactions
            .get(&block_hash)
            .cloned()
            .unwrap_or_default())
    }

    async fn transactions_at_height(&self, _height: u64) -> Result<(BlockHash, Vec<Transaction>)> {
        unreachable!("not used by indexer tests")
    }

    async fn block_hash_at_height(&self, _height: u64) -> Result<BlockHash> {
        unreachable!("not used by indexer tests")
    }

    async fn wait_for_next_block(&self, _prev_block: BtcBlockTip) -> Result<BtcBlockTip> {
        self.announcement_receiver
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| Error::Returned("test block announcement channel closed".to_owned()))
    }

    async fn publish_transaction(&self, _signed_transaction: &Transaction) -> Result<Txid> {
        unreachable!("not used by indexer tests")
    }

    async fn fund_and_sign_transaction(&self, _transaction: &Transaction) -> Result<Transaction> {
        unreachable!("not used by indexer tests")
    }

    async fn publish_mint_transaction(
        &self,
        _transaction: &Transaction,
        _max_burn_amount: Amount,
    ) -> Result<Txid> {
        unreachable!("not used by indexer tests")
    }
}

pub fn block_hash(value: u64) -> BlockHash {
    BlockHash::from_str(&format!("{value:064x}")).expect("valid block hash")
}

pub fn block_tip(hash_value: u64, height: u64) -> BtcBlockTip {
    BtcBlockTip {
        hash: block_hash(hash_value),
        height,
    }
}

pub fn proof(flame_block_hash: [u8; 32], burned_sats: u64, tip: BtcBlockTip) -> MintingProof {
    MintingProof {
        minting_proof_data: MintingProofData {
            network: FlameNetwork::Regtest,
            flame_block_hash: FlameBlockHash::from(flame_block_hash),
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

pub async fn recv_update(
    receiver: &mut broadcast::Receiver<Arc<MintingProofUpdate>>,
) -> Arc<MintingProofUpdate> {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .expect("proof update timeout")
        .expect("proof update sender remains alive")
}
