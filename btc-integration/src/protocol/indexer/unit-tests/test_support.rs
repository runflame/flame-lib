use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use corepc_client::{
    bitcoin::{
        Amount, BlockHash, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
        absolute, transaction,
    },
    client_sync::{Error, Result},
};
use ed25519_dalek::SigningKey;
use flamechain::BlockHash as FlameBlockHash;
use flamevm::Predicate;
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, watch};

use crate::{
    btc::rpc::{BtcBlockHeaderInfo, BtcBlockTip, BtcTransactionWithPrevouts, RpcApi},
    protocol::{AcquisitionData, MinterP2wsh, MintingVoteData, minter_witness_script},
};

pub(super) struct TestChain {
    headers: BTreeMap<BlockHash, BtcBlockHeaderInfo>,
    transactions: BTreeMap<BlockHash, Vec<BtcTransactionWithPrevouts>>,
}

impl TestChain {
    pub(super) fn new() -> Self {
        Self {
            headers: BTreeMap::new(),
            transactions: BTreeMap::new(),
        }
    }

    pub(super) fn block(
        mut self,
        tip: BtcBlockTip,
        previous_block_hash: Option<BlockHash>,
        transactions: Vec<BtcTransactionWithPrevouts>,
    ) -> Self {
        self.headers.insert(
            tip.hash,
            BtcBlockHeaderInfo {
                tip,
                previous_block_hash,
            },
        );
        self.transactions.insert(tip.hash, transactions);
        self
    }
}

pub(super) struct FakeRpc {
    best_tip: RwLock<BtcBlockTip>,
    headers: BTreeMap<BlockHash, BtcBlockHeaderInfo>,
    transactions: BTreeMap<BlockHash, Vec<BtcTransactionWithPrevouts>>,
    announcements: mpsc::UnboundedSender<BtcBlockTip>,
    announcement_receiver: Mutex<mpsc::UnboundedReceiver<BtcBlockTip>>,
    header_calls: StdMutex<Vec<BlockHash>>,
    pub(super) block_calls: StdMutex<Vec<BlockHash>>,
    pub(super) gates: StdMutex<BTreeMap<BlockHash, Arc<LoadGate>>>,
    pub(super) failures: StdMutex<BTreeMap<BlockHash, Error>>,
    active: AtomicUsize,
    pub(super) max_active: AtomicUsize,
}

impl FakeRpc {
    pub(super) fn new(chain: TestChain, best_tip: BtcBlockTip) -> Arc<Self> {
        let (announcements, announcement_receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            best_tip: RwLock::new(best_tip),
            headers: chain.headers,
            transactions: chain.transactions,
            announcements,
            announcement_receiver: Mutex::new(announcement_receiver),
            header_calls: StdMutex::new(Vec::new()),
            block_calls: StdMutex::new(Vec::new()),
            gates: StdMutex::new(BTreeMap::new()),
            failures: StdMutex::new(BTreeMap::new()),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        })
    }

    pub(super) async fn set_best_tip(&self, tip: BtcBlockTip) {
        *self.best_tip.write().await = tip;
    }

    pub(super) fn announce(&self, tip: BtcBlockTip) {
        self.announcements
            .send(tip)
            .expect("test indexer is listening for block announcements");
    }

    pub(super) fn clear_header_calls(&self) {
        self.header_calls.lock().unwrap().clear();
    }

    pub(super) fn header_calls(&self) -> Vec<BlockHash> {
        self.header_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl RpcApi for FakeRpc {
    async fn list_unspent(
        &self,
    ) -> corepc_client::client_sync::Result<Vec<crate::btc::rpc::BtcUnspentOutput>> {
        unreachable!("wallet RPC not used by this test")
    }
    async fn fund_raw_transaction(
        &self,
        _: &Transaction,
        _: &crate::btc::rpc::BtcFundOptions,
    ) -> corepc_client::client_sync::Result<Transaction> {
        unreachable!("wallet RPC not used by this test")
    }
    async fn sign_raw_transaction_with_wallet(
        &self,
        _: &Transaction,
    ) -> corepc_client::client_sync::Result<crate::btc::rpc::BtcSignedTransaction> {
        unreachable!("wallet RPC not used by this test")
    }

    async fn best_block_tip(&self) -> Result<BtcBlockTip> {
        Ok(*self.best_tip.read().await)
    }

    async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo> {
        self.header_calls.lock().unwrap().push(block_hash);
        self.headers
            .get(&block_hash)
            .copied()
            .ok_or_else(|| rpc_error(-5, "Block not found"))
    }

    async fn transactions_in_block(&self, block_hash: BlockHash) -> Result<Vec<Transaction>> {
        Ok(self
            .transactions
            .get(&block_hash)
            .into_iter()
            .flatten()
            .map(|entry| entry.transaction.clone())
            .collect())
    }

    async fn transactions_with_prevouts_in_block(
        &self,
        block_hash: BlockHash,
    ) -> Result<Vec<BtcTransactionWithPrevouts>> {
        self.block_calls.lock().unwrap().push(block_hash);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _active = ActiveLoad(&self.active);
        let gate = self.gates.lock().unwrap().get(&block_hash).cloned();
        if let Some(gate) = gate {
            gate.entered.add_permits(1);
            gate.release.acquire().await.unwrap().forget();
        }
        if let Some(error) = self.failures.lock().unwrap().remove(&block_hash) {
            return Err(error);
        }
        self.transactions
            .get(&block_hash)
            .cloned()
            .ok_or_else(|| rpc_error(-1, "Block not available (pruned data)"))
    }

    async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash> {
        let mut tip = *self.best_tip.read().await;
        if height > tip.height {
            return Err(rpc_error(-8, "Block height out of range"));
        }
        while tip.height > height {
            let previous = self.headers[&tip.hash].previous_block_hash.unwrap();
            tip = self.headers[&previous].tip;
        }
        Ok(tip.hash)
    }

    async fn wait_for_new_block(
        &self,
        prev_block: BlockHash,
        _timeout_ms: u64,
    ) -> Result<BtcBlockTip> {
        let tip = *self.best_tip.read().await;
        if tip.hash != prev_block {
            return Ok(tip);
        }
        self.announcement_receiver
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| Error::Returned("test announcement channel closed".to_owned()))
    }

    async fn send_raw_transaction(
        &self,
        _transaction: &Transaction,
        _max_burn_amount: Option<Amount>,
    ) -> Result<Txid> {
        unreachable!("not used by protocol indexer tests")
    }
}

pub(super) fn block_hash(value: u64) -> BlockHash {
    BlockHash::from_str(&format!("{value:064x}")).expect("valid block hash")
}

pub(super) fn block_tip(value: u64, height: u64) -> BtcBlockTip {
    BtcBlockTip {
        hash: block_hash(value),
        height,
    }
}

pub(super) fn acquisition_transaction(satoshis: u64) -> BtcTransactionWithPrevouts {
    let data = AcquisitionData::new(
        [0x11; 32].into(),
        Predicate::opaque(Predicate::unspendable_key()),
        SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    );
    BtcTransactionWithPrevouts {
        transaction: Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![TxOut {
                value: Amount::from_sat(satoshis),
                script_pubkey: data.to_script(),
            }],
        },
        prevouts: Vec::new(),
    }
}

pub(super) fn vote_transaction(authenticated: bool) -> BtcTransactionWithPrevouts {
    let predicate = Predicate::opaque(Predicate::unspendable_key());
    let witness_script = minter_witness_script::build_with_authorization(
        &predicate,
        corepc_client::bitcoin::Script::from_bytes(&[0x51]),
    )
    .into_bytes();
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[&witness_script]),
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: MintingVoteData::V1 {
                flame_block_height: 42,
                flame_block_hash: FlameBlockHash::from([0x42; 32]),
            }
            .to_script(),
        }],
    };
    let script_pubkey = if authenticated {
        let minter_p2wsh = MinterP2wsh::from_witness_script(&witness_script);
        let mut bytes = vec![0x00, 0x20];
        bytes.extend_from_slice(minter_p2wsh.as_bytes());
        ScriptBuf::from_bytes(bytes)
    } else {
        ScriptBuf::new()
    };
    BtcTransactionWithPrevouts {
        transaction,
        prevouts: vec![Some(TxOut {
            value: Amount::from_sat(1),
            script_pubkey,
        })],
    }
}

pub(super) async fn recv_tip(
    receiver: &mut watch::Receiver<Option<BtcBlockTip>>,
) -> Option<BtcBlockTip> {
    tokio::time::timeout(Duration::from_secs(3), receiver.changed())
        .await
        .expect("tip notification timeout")
        .expect("tip sender remains alive");
    *receiver.borrow_and_update()
}

pub(super) fn rpc_error(code: i32, message: &str) -> Error {
    Error::JsonRpc(jsonrpc::Error::Rpc(jsonrpc::error::RpcError {
        code,
        message: message.into(),
        data: None,
    }))
}

pub(super) struct LoadGate {
    pub entered: Semaphore,
    pub release: Semaphore,
}

impl LoadGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }

    pub async fn wait_for(&self, count: u32) {
        tokio::time::timeout(Duration::from_secs(3), self.entered.acquire_many(count))
            .await
            .expect("load gate timeout")
            .unwrap()
            .forget();
    }
}

struct ActiveLoad<'a>(&'a AtomicUsize);
impl Drop for ActiveLoad<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn linear_chain(height: u64) -> TestChain {
    let mut chain = TestChain::new().block(block_tip(0, 0), None, vec![]);
    for h in 1..=height {
        chain = chain.block(block_tip(h, h), Some(block_hash(h - 1)), vec![]);
    }
    chain
}
