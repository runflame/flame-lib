use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use bitcoin::block::{Header, Version};
use bitcoin::hashes::Hash;
use bitcoin::{
    Amount, Block, BlockHash, CompactTarget, OutPoint, Script, ScriptBuf, Sequence, Transaction,
    TxIn, TxMerkleNode, TxOut, Txid, absolute, transaction,
};

use crate::watcher::index::BlockIndex;
use crate::watcher::source::{ChainSource, SourceError};
use crate::watcher::types::{BlockId, Prevouts};

/// Blocks are never deleted: after a reorg the old branch stays available by
/// hash, as in Bitcoin Core.
struct World {
    blocks: HashMap<BlockHash, Mined>,
    outputs: HashMap<OutPoint, TxOut>,
    chain: Vec<BlockId>,
    /// How many blocks were mined. A block gets this number as its nonce,
    /// which makes every header unique and tells which block is older.
    mined: u32,
    wakers: Vec<Waker>,
    calls: u32,
    fail_at: Option<u32>,
    unspent: HashMap<OutPoint, TxOut>,
    mempool: Vec<Transaction>,
}

struct Mined {
    height: u64,
    block: Block,
    prevouts: Prevouts,
    spent: Vec<(OutPoint, TxOut)>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejected {
    #[error("the transaction has no inputs")]
    NoInputs,
    #[error("the output {outpoint} does not exist or is spent")]
    MissingInput { outpoint: OutPoint },
    #[error("two inputs spend the output {outpoint}")]
    DuplicateInput { outpoint: OutPoint },
    #[error("the output {outpoint} is spent by a transaction that waits for a block")]
    SpentInMempool { outpoint: OutPoint },
    #[error("the transaction spends {inputs} and pays {outputs}")]
    Overspend { inputs: Amount, outputs: Amount },
    #[error("a script or a signature does not hold: {0}")]
    Script(String),
}

fn accept(
    spendable: &mut HashMap<OutPoint, TxOut>,
    transaction: &Transaction,
) -> Result<(), Rejected> {
    if transaction.input.is_empty() {
        return Err(Rejected::NoInputs);
    }
    let mut spent = HashMap::new();
    for input in &transaction.input {
        let outpoint = input.previous_output;
        let Some(output) = spendable.get(&outpoint) else {
            return Err(Rejected::MissingInput { outpoint });
        };
        if spent.insert(outpoint, output.clone()).is_some() {
            return Err(Rejected::DuplicateInput { outpoint });
        }
    }
    let inputs: Amount = spent.values().map(|output| output.value).sum();
    let outputs: Amount = transaction.output.iter().map(|output| output.value).sum();
    if outputs > inputs {
        return Err(Rejected::Overspend { inputs, outputs });
    }
    transaction
        .verify(|outpoint| spent.get(outpoint).cloned())
        .map_err(|error| Rejected::Script(error.to_string()))?;

    for outpoint in spent.keys() {
        spendable.remove(outpoint);
    }
    spendable.extend(new_outputs(transaction));
    Ok(())
}

fn new_outputs(transaction: &Transaction) -> impl Iterator<Item = (OutPoint, TxOut)> {
    let txid = transaction.compute_txid();
    let outputs = transaction.output.iter().enumerate();
    outputs
        .filter(|(_, output)| !output.script_pubkey.is_op_return())
        .map(move |(vout, output)| (OutPoint::new(txid, vout as u32), output.clone()))
}

impl World {
    fn best(&self) -> BlockId {
        *self.chain.last().expect("the chain has the genesis block")
    }

    fn add(&mut self, parent: Option<BlockId>, txdata: Vec<Transaction>) -> BlockId {
        let (height, prev_blockhash, time) = match parent {
            Some(parent) => {
                let time = self.blocks[&parent.hash].block.header.time;
                (parent.height + 1, parent.hash, time + 600)
            }
            None => (0, BlockHash::all_zeros(), 1_700_000_000),
        };
        self.mined += 1;
        let prevouts = self.spend(&txdata);
        let mut block = Block {
            header: Header {
                version: Version::TWO,
                prev_blockhash,
                merkle_root: TxMerkleNode::all_zeros(),
                time,
                // The difficulty of regtest.
                bits: CompactTarget::from_consensus(0x207f_ffff),
                nonce: self.mined,
            },
            txdata,
        };
        // A block without transactions has no merkle root: it stays zero.
        if let Some(merkle_root) = block.compute_merkle_root() {
            block.header.merkle_root = merkle_root;
        }
        let hash = block.block_hash();
        let mined = Mined {
            height,
            block,
            prevouts,
            spent: Vec::new(),
        };
        self.blocks.insert(hash, mined);
        BlockId { height, hash }
    }

    fn spend(&mut self, txdata: &[Transaction]) -> Prevouts {
        let mut prevouts = Vec::with_capacity(txdata.len());
        for transaction in txdata {
            let spent = transaction
                .input
                .iter()
                .map(|input| self.outputs.get(&input.previous_output).cloned())
                .collect();
            prevouts.push(spent);
            let txid = transaction.compute_txid();
            for (vout, output) in transaction.output.iter().enumerate() {
                let outpoint = OutPoint::new(txid, vout as u32);
                self.outputs.insert(outpoint, output.clone());
            }
        }
        Prevouts::new(prevouts)
    }

    fn set_best(&mut self, best: BlockId) {
        assert!(
            self.blocks
                .get(&best.hash)
                .is_some_and(|mined| mined.height == best.height),
            "unknown block {best}"
        );
        let mut branch = Vec::new();
        let mut block = best;
        while self.chain.get(block.height as usize) != Some(&block) {
            branch.push(block);
            block = block.parent(&self.blocks[&block.hash].block.header);
        }
        // The blocks above it leave the chain, the best one first. Their
        // transactions wait for a block again, ahead of the ones that waited
        // already: those may spend their outputs.
        let mut waiting = Vec::new();
        for removed in self.chain.split_off(block.height as usize + 1).iter().rev() {
            waiting.splice(0..0, self.disconnect(*removed));
        }
        for added in branch.into_iter().rev() {
            self.connect(added);
            self.chain.push(added);
        }
        // What the new chain made impossible does not wait any more: a
        // transaction that is in it now, or one that spends what it spent.
        waiting.append(&mut self.mempool);
        let mut spendable = self.unspent.clone();
        waiting.retain(|transaction| accept(&mut spendable, transaction).is_ok());
        self.mempool = waiting;
        self.wake();
    }

    fn connect(&mut self, id: BlockId) {
        let mined = self.blocks.get_mut(&id.hash).expect("a known block");
        mined.spent.clear();
        for transaction in &mined.block.txdata {
            for input in &transaction.input {
                let outpoint = input.previous_output;
                if let Some(output) = self.unspent.remove(&outpoint) {
                    mined.spent.push((outpoint, output));
                }
            }
            self.unspent.extend(new_outputs(transaction));
        }
    }

    fn disconnect(&mut self, id: BlockId) -> Vec<Transaction> {
        let mined = self.blocks.get_mut(&id.hash).expect("a known block");
        for transaction in &mined.block.txdata {
            for (outpoint, _) in new_outputs(transaction) {
                self.unspent.remove(&outpoint);
            }
        }
        self.unspent.extend(mined.spent.drain(..));
        mined.block.txdata.clone()
    }

    fn spendable(&self) -> HashMap<OutPoint, TxOut> {
        let mut spendable = self.unspent.clone();
        for transaction in &self.mempool {
            accept(&mut spendable, transaction).expect("a waiting transaction holds");
        }
        spendable
    }

    fn mine(&mut self, transactions: Vec<Transaction>) -> BlockId {
        let block = self.add(Some(self.best()), transactions);
        self.set_best(block);
        block
    }

    fn wake(&mut self) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }

    fn take_failure(&mut self) -> Result<(), SourceError> {
        self.calls += 1;
        match self.fail_at {
            Some(0) => {
                self.fail_at = None;
                Err(SourceError("the source is unreachable".into()))
            }
            Some(calls) => {
                self.fail_at = Some(calls - 1);
                Ok(())
            }
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
pub struct MemoryBtc {
    world: Arc<Mutex<World>>,
}

impl Default for MemoryBtc {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBtc {
    pub fn new() -> Self {
        let mut world = World {
            blocks: HashMap::new(),
            outputs: HashMap::new(),
            chain: Vec::new(),
            mined: 0,
            wakers: Vec::new(),
            calls: 0,
            fail_at: None,
            unspent: HashMap::new(),
            mempool: Vec::new(),
        };
        let genesis = world.add(None, Vec::new());
        world.chain.push(genesis);
        MemoryBtc {
            world: Arc::new(Mutex::new(world)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, World> {
        self.world.lock().expect("the world lock is not poisoned")
    }

    pub fn best(&self) -> BlockId {
        self.lock().best()
    }

    pub fn best_chain(&self) -> Vec<BlockId> {
        self.lock().chain.clone()
    }

    pub fn block_at(&self, height: u64) -> Option<BlockId> {
        self.lock().chain.get(height as usize).copied()
    }

    pub fn submit(&self, transaction: Transaction) -> Result<Txid, Rejected> {
        let mut world = self.lock();
        let mut spendable = world.spendable();
        accept(&mut spendable, &transaction).map_err(|rejected| match rejected {
            Rejected::MissingInput { outpoint } if world.unspent.contains_key(&outpoint) => {
                Rejected::SpentInMempool { outpoint }
            }
            other => other,
        })?;
        let txid = transaction.compute_txid();
        world.mempool.push(transaction);
        Ok(txid)
    }

    pub fn mempool(&self) -> Vec<Txid> {
        let world = self.lock();
        world
            .mempool
            .iter()
            .map(Transaction::compute_txid)
            .collect()
    }

    pub(crate) fn unspent(&self, script_pubkey: &Script) -> Vec<(OutPoint, TxOut)> {
        let world = self.lock();
        let spendable = world.spendable();
        let confirmed = world.unspent.iter();
        let mut unspent: Vec<_> = confirmed
            .filter(|(outpoint, output)| {
                output.script_pubkey == *script_pubkey && spendable.contains_key(outpoint)
            })
            .map(|(outpoint, output)| (*outpoint, output.clone()))
            .collect();
        unspent.sort_by_key(|(outpoint, _)| *outpoint);
        unspent
    }

    pub fn mine(&self) -> BlockId {
        let mut world = self.lock();
        let waiting = world.mempool.clone();
        world.mine(waiting)
    }

    /// A new block on top of the best one, with these waiting transactions
    /// only. The others go on waiting.
    ///
    /// Panics if one of them does not wait, or spends an output of a waiting
    /// transaction that is left out.
    pub fn mine_only(&self, txids: &[Txid]) -> BlockId {
        let mut world = self.lock();
        let waiting = world.mempool.iter();
        let chosen: Vec<Transaction> = waiting
            .filter(|transaction| txids.contains(&transaction.compute_txid()))
            .cloned()
            .collect();
        assert_eq!(chosen.len(), txids.len(), "not every transaction waits");
        let mut spendable = world.unspent.clone();
        for transaction in &chosen {
            if let Err(rejected) = accept(&mut spendable, transaction) {
                let txid = transaction.compute_txid();
                panic!("{txid} cannot be mined without the others: {rejected}");
            }
        }
        world.mine(chosen)
    }

    pub fn mine_with(&self, transactions: Vec<Transaction>) -> BlockId {
        self.lock().mine(transactions)
    }

    pub fn mine_coins(&self, script_pubkey: ScriptBuf, amount: Amount) -> OutPoint {
        let mut world = self.lock();
        let payment = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            // It spends nothing. The sequence number tells two payments of
            // the same amount to the same script apart.
            input: vec![TxIn {
                sequence: Sequence(world.mined),
                ..TxIn::default()
            }],
            output: vec![TxOut {
                value: amount,
                script_pubkey,
            }],
        };
        let outpoint = OutPoint::new(payment.compute_txid(), 0);
        world.mine(vec![payment]);
        outpoint
    }

    pub fn mine_n(&self, count: u64) -> BlockId {
        (0..count).fold(self.best(), |_, _| self.mine())
    }

    /// A block on top of any known block. As in Bitcoin, the branch becomes the
    /// best one only when it is higher than the current best.
    pub fn mine_on(&self, parent: BlockId) -> BlockId {
        let mut world = self.lock();
        let block = world.add(Some(parent), Vec::new());
        if block.height > world.best().height {
            world.set_best(block);
        }
        block
    }

    pub fn set_best(&self, block: BlockId) {
        self.lock().set_best(block);
    }

    pub fn reorg(&self, depth: u64, new_blocks: u64) -> BlockId {
        let mut world = self.lock();
        let fork = world.best().height - depth;
        let mut block = world.chain[fork as usize];
        for _ in 0..new_blocks {
            block = world.add(Some(block), Vec::new());
        }
        world.set_best(block);
        block
    }

    /// Makes one upcoming source call fail, as if bitcoind were unreachable.
    /// `call` counts from the next call, which is 0.
    pub fn fail_call(&self, call: u32) {
        self.lock().fail_at = Some(call);
    }

    pub fn calls(&self) -> u32 {
        self.lock().calls
    }

    pub fn source(&self) -> MemorySource {
        MemorySource {
            world: self.world.clone(),
            seen: None,
        }
    }

    pub fn lagging_source(&self) -> (MemorySource, Lag) {
        let seen = Arc::new(Mutex::new(Seen {
            best: self.lock().chain[0],
            mined: 0,
        }));
        let lag = Lag {
            world: self.world.clone(),
            seen: seen.clone(),
        };
        lag.catch_up();
        let source = MemorySource {
            world: self.world.clone(),
            seen: Some(seen),
        };
        (source, lag)
    }
}

struct Seen {
    best: BlockId,
    mined: u32,
}

pub struct Lag {
    world: Arc<Mutex<World>>,
    seen: Arc<Mutex<Seen>>,
}

impl Lag {
    pub fn best(&self) -> BlockId {
        self.seen.lock().expect("not poisoned").best
    }

    pub fn deliver(&self, block: BlockId) {
        let mut world = self.world.lock().expect("not poisoned");
        let mut seen = self.seen.lock().expect("not poisoned");
        seen.best = block;
        seen.mined = seen.mined.max(world.blocks[&block.hash].block.header.nonce);
        drop(seen);
        world.wake();
    }

    pub fn catch_up(&self) -> BlockId {
        let best = self.world.lock().expect("not poisoned").best();
        self.deliver(best);
        best
    }
}

pub struct MemorySource {
    world: Arc<Mutex<World>>,
    seen: Option<Arc<Mutex<Seen>>>,
}

impl MemorySource {
    fn lock(&self) -> MutexGuard<'_, World> {
        self.world.lock().expect("the world lock is not poisoned")
    }

    fn visible_best(&self, world: &World) -> BlockId {
        match &self.seen {
            None => world.best(),
            Some(seen) => seen.lock().expect("not poisoned").best,
        }
    }

    fn find(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        let mut world = self.lock();
        world.take_failure()?;
        let newest_known = match &self.seen {
            None => u32::MAX,
            Some(seen) => seen.lock().expect("not poisoned").mined,
        };
        Ok(world
            .blocks
            .get(&hash)
            .filter(|mined| mined.block.header.nonce <= newest_known)
            .map(|mined| (mined.block.clone(), mined.prevouts.clone())))
    }
}

impl BlockIndex for MemorySource {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        Ok(self.find(id.hash)?.map(|(block, _)| block.header))
    }
}

impl ChainSource for MemorySource {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        self.find(hash)
    }

    fn wait_for_new_block(
        &self,
        known: BlockHash,
    ) -> impl Future<Output = Result<BlockId, SourceError>> + Send {
        WaitForNewBlock {
            source: self,
            known,
            started: false,
        }
    }

    async fn block_at(&self, height: u64) -> Result<Option<BlockId>, SourceError> {
        let mut world = self.lock();
        world.take_failure()?;
        // Down from the best block this source sees: for a lagging node that
        // is not the best chain of the world.
        let mut block = self.visible_best(&world);
        while block.height > height {
            block = block.parent(&world.blocks[&block.hash].block.header);
        }
        Ok((block.height == height).then_some(block))
    }
}

struct WaitForNewBlock<'a> {
    source: &'a MemorySource,
    known: BlockHash,
    started: bool,
}

impl Future for WaitForNewBlock<'_> {
    type Output = Result<BlockId, SourceError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let source = self.source;
        let mut world = source.lock();
        if !self.started {
            self.started = true;
            world.take_failure()?;
        }
        let best = source.visible_best(&world);
        if best.hash != self.known {
            return Poll::Ready(Ok(best));
        }
        if !world.wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            world.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}
