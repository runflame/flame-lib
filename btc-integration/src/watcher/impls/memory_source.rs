//! Bitcoin in memory: the source for tests and for a devnet without bitcoind.
//!
//! The test decides when a block appears, on which branch, with which
//! transactions, and which node has already seen it. Blocks are real
//! [`bitcoin::Block`]s with real hashes and merkle roots.
//!
//! A transaction gets into a block in one of two ways:
//! - [`MemoryBtc::submit`] checks it the way a node does and lets it wait for
//!   a block: its inputs are unspent, its scripts and signatures hold, it does
//!   not pay more than it spends. [`MemoryBtc::mine`] takes every waiting
//!   transaction, [`MemoryBtc::mine_only`] the ones the test names, so the
//!   test decides how long a transaction waits.
//! - [`MemoryBtc::mine_with`] puts transactions into a block as they are.
//!   Here the test is the miner, and nothing is checked.
//!
//! Not checked at all: proof of work, fees, lock times, the maturity of
//! outputs.

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

/// Every block that ever appeared, and the best chain among them.
///
/// Blocks are never deleted: after a reorg the old branch stays available by
/// hash, as in Bitcoin Core.
struct World {
    blocks: HashMap<BlockHash, Mined>,
    /// Every output of every transaction that was ever mined, on any branch.
    /// The prevouts of a new block come from here.
    outputs: HashMap<OutPoint, TxOut>,
    /// The best chain, by height: `chain[0]` is the genesis block, the last
    /// one is the best block.
    chain: Vec<BlockId>,
    /// How many blocks were mined. A block gets this number as its nonce,
    /// which makes every header unique and tells which block is older.
    mined: u32,
    /// Those waiting for the best block to change.
    wakers: Vec<Waker>,
    /// How many times the sources were asked.
    calls: u32,
    /// Which upcoming source call fails; 0 is the next one.
    fail_at: Option<u32>,
    /// The unspent outputs of the best chain.
    unspent: HashMap<OutPoint, TxOut>,
    /// The transactions that wait for a block, in the order they came. One
    /// may spend an output of an earlier one.
    mempool: Vec<Transaction>,
}

/// A block as the world keeps it.
struct Mined {
    height: u64,
    block: Block,
    prevouts: Prevouts,
    /// The outputs this block took out of the unspent ones when it joined
    /// the best chain. They come back when it leaves.
    spent: Vec<(OutPoint, TxOut)>,
}

/// Why [`MemoryBtc::submit`] did not take a transaction.
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

/// Checks a transaction against the outputs it may spend and, if it holds,
/// spends them and adds its own.
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

/// The outputs of a transaction that can be spent.
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

    /// Creates a block on top of `parent` with these transactions. Does not
    /// change the best chain.
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

    /// The prevouts of the transactions of a new block, in block order. An
    /// input that names an output no mined transaction has gets `None`. A
    /// transaction can spend an output of an earlier one in the same block.
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

    /// Makes `best` the best block: the best chain now ends at it.
    fn set_best(&mut self, best: BlockId) {
        assert!(
            self.blocks
                .get(&best.hash)
                .is_some_and(|mined| mined.height == best.height),
            "unknown block {best}"
        );
        // The blocks of the new branch, down to the block it shares with the
        // chain we have.
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

    /// The block joins the best chain: its inputs are spent, its outputs can
    /// be spent.
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

    /// The block leaves the best chain: its outputs are gone, the outputs it
    /// spent are back. Returns its transactions.
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

    /// The outputs a new transaction may spend: the unspent outputs of the
    /// best chain, after the transactions that wait.
    fn spendable(&self) -> HashMap<OutPoint, TxOut> {
        let mut spendable = self.unspent.clone();
        for transaction in &self.mempool {
            accept(&mut spendable, transaction).expect("a waiting transaction holds");
        }
        spendable
    }

    /// A new block on top of the best one, with these transactions.
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

    /// Counts a source call and decides whether it has to fail.
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

/// A handle to Bitcoin in memory. Cheap to clone: all clones share one chain.
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
    /// A chain of one block at height 0.
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

    /// The best block.
    pub fn best(&self) -> BlockId {
        self.lock().best()
    }

    /// The best chain by height: the genesis block first, the best block last.
    pub fn best_chain(&self) -> Vec<BlockId> {
        self.lock().chain.clone()
    }

    /// The block of the best chain at this height.
    pub fn block_at(&self, height: u64) -> Option<BlockId> {
        self.lock().chain.get(height as usize).copied()
    }

    /// Checks a transaction the way a node does and lets it wait for a
    /// block.
    ///
    /// Its inputs have to be unspent outputs of the best chain or of the
    /// transactions that wait.
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

    /// The transactions that wait for a block, in the order they came.
    pub fn mempool(&self) -> Vec<Txid> {
        let world = self.lock();
        world
            .mempool
            .iter()
            .map(Transaction::compute_txid)
            .collect()
    }

    /// The outputs of the best chain that pay to `script_pubkey` and that
    /// nothing has spent, a waiting transaction included. In the order of
    /// their outpoints.
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

    /// A new block on top of the best one, with every transaction that
    /// waits.
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

    /// A new block on top of the best one, with these transactions as they
    /// are: nothing checks them. The transactions that wait go on waiting,
    /// unless the block makes them impossible.
    ///
    /// The source returns the block with its prevouts: for every input, the
    /// output it names, if a transaction mined before has it.
    pub fn mine_with(&self, transactions: Vec<Transaction>) -> BlockId {
        self.lock().mine(transactions)
    }

    /// A new block with one transaction that pays `amount` to `script_pubkey`
    /// out of nothing, the way a block reward does. Returns the new output.
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

    /// `count` blocks in a row; returns the last one.
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

    /// Makes any known block the best one: a branch of the same height, or an
    /// ancestor (this is what `invalidateblock` does).
    pub fn set_best(&self, block: BlockId) {
        self.lock().set_best(block);
    }

    /// A reorg in one line: steps `depth` blocks back from the best block,
    /// builds a branch of `new_blocks` blocks there and makes it the best.
    /// Returns the new best block.
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

    /// How many times the sources of this chain were asked: for a header, a
    /// block, the best block or the block at a height. The difference tells a
    /// test whether the watcher went to the source.
    pub fn calls(&self) -> u32 {
        self.lock().calls
    }

    /// A source that sees every block at once.
    pub fn source(&self) -> MemorySource {
        MemorySource {
            world: self.world.clone(),
            seen: None,
        }
    }

    /// The source of a lagging node: it sees only what was delivered to it
    /// through [`Lag`]. The node starts in sync.
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

/// What has reached a lagging node.
struct Seen {
    /// The best block the node sees.
    best: BlockId,
    /// The node knows every block mined up to this one, by the count in
    /// `World::mined`.
    mined: u32,
}

/// Controls what a lagging node sees. This is how a test with several nodes
/// says which node has received which Bitcoin block.
pub struct Lag {
    world: Arc<Mutex<World>>,
    seen: Arc<Mutex<Seen>>,
}

impl Lag {
    /// The best block the node sees.
    pub fn best(&self) -> BlockId {
        self.seen.lock().expect("not poisoned").best
    }

    /// The node receives `block` and every block mined before it; `block`
    /// becomes the best block it sees.
    pub fn deliver(&self, block: BlockId) {
        let mut world = self.world.lock().expect("not poisoned");
        let mut seen = self.seen.lock().expect("not poisoned");
        seen.best = block;
        seen.mined = seen.mined.max(world.blocks[&block.hash].block.header.nonce);
        drop(seen);
        world.wake();
    }

    /// The node receives everything there is. Returns the best block.
    pub fn catch_up(&self) -> BlockId {
        let best = self.world.lock().expect("not poisoned").best();
        self.deliver(best);
        best
    }
}

/// [`ChainSource`] over [`MemoryBtc`].
pub struct MemorySource {
    world: Arc<Mutex<World>>,
    /// `None`: the source sees everything; otherwise only what was delivered.
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

    /// A block the source knows, with its prevouts, after counting the call.
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

/// Ready as soon as the visible best block is not `known`.
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
