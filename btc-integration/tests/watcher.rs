//! How to use the watcher. Every test is one scenario.
//!
//! There is no executor here. With the source and the storage in memory a call
//! to `next()` is either ready at once or waits for the source, and one poll
//! (`now_or_never`) tells which. The only exception is the test with a slow
//! source, which runs under `block_on`.

use std::collections::HashSet;
use std::future::poll_fn;
use std::pin::pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;

use bitcoin::block::Header;
use bitcoin::{
    Amount, Block, BlockHash, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, absolute, transaction,
};
use btc_integration::watcher::impls::{MemoryBtc, MemorySource, MemoryStorage, RecentBlocks};
use btc_integration::watcher::{
    BlockId, BlockIndex, BtcEvent, BtcStorage, BtcView, BtcWatcher, ChainSource, Prevouts,
    SourceError, StorageError, WatchError,
};
use futures::FutureExt;
use futures::executor::block_on;

/// Bitcoin of six blocks (heights 0…5) and a watcher whose genesis block
/// is at height 2.
fn setup() -> (MemoryBtc, BtcWatcher<MemorySource, MemoryStorage>) {
    let btc = MemoryBtc::new();
    btc.mine_n(5);
    let genesis_id = btc.block_at(2).unwrap();
    let watcher = BtcWatcher::new(btc.source(), MemoryStorage::new(), genesis_id);
    (btc, watcher)
}

/// An event that is ready now.
fn event<S: ChainSource, B: BtcStorage>(watcher: &mut BtcWatcher<S, B>) -> BtcEvent {
    watcher
        .next()
        .now_or_never()
        .expect("an event should be ready, but the watcher waits")
        .expect("the source and the storage answered")
}

/// An error that is ready now.
fn error<S: ChainSource, B: BtcStorage>(watcher: &mut BtcWatcher<S, B>) -> WatchError {
    watcher
        .next()
        .now_or_never()
        .expect("an error should be ready, but the watcher waits")
        .expect_err("the call should fail")
}

/// The watcher waits for the source: there is nothing to report.
fn waits<S: ChainSource, B: BtcStorage>(watcher: &mut BtcWatcher<S, B>) -> bool {
    watcher.next().now_or_never().is_none()
}

fn heights(event: &BtcEvent) -> Vec<u64> {
    event.blocks().iter().map(|block| block.height).collect()
}

/// A transaction with one input, which spends `spends`, and one output of
/// `sat` satoshi. Nothing checks a transaction, so the scripts are empty.
fn spend(spends: OutPoint, sat: u64) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: spends,
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(sat),
            script_pubkey: ScriptBuf::new(),
        }],
    }
}

#[test]
fn blocks_that_appeared_together_arrive_as_one_event() {
    let (btc, mut watcher) = setup();

    // The first call reports the chain above the genesis block.
    let first = event(&mut watcher);
    assert!(matches!(first, BtcEvent::NewBlocks { .. }));
    assert_eq!(heights(&first), [3, 4, 5]);
    assert_eq!(watcher.view().best(), btc.best());

    // Nothing more to report.
    assert!(waits(&mut watcher));

    btc.mine();
    assert_eq!(heights(&event(&mut watcher)), [6]);

    // Three blocks mined while the consumer was busy arrive as one event.
    btc.mine_n(3);
    assert_eq!(heights(&event(&mut watcher)), [7, 8, 9]);
}

#[test]
fn reorgs_of_depth_1_3_and_15() {
    for depth in [1u64, 3, 15] {
        let btc = MemoryBtc::new();
        let genesis_id = btc.best();
        btc.mine_n(20);
        let mut watcher = BtcWatcher::new(btc.source(), MemoryStorage::new(), genesis_id);
        event(&mut watcher);
        // The blocks that will drop out, newest first, and the block
        // below them.
        let old: Vec<BlockId> = (0..depth).map(|i| btc.block_at(20 - i).unwrap()).collect();
        let common = btc.block_at(20 - depth).unwrap();

        let new_best = btc.reorg(depth, depth + 1);

        let BtcEvent::Reorg {
            fork,
            removed,
            blocks,
        } = event(&mut watcher)
        else {
            panic!("expected a reorg");
        };
        assert_eq!(fork, common);
        assert_eq!(removed, old);
        assert_eq!(blocks.len() as u64, depth + 1);
        assert_eq!(blocks[0].height, 21 - depth);
        assert_eq!(watcher.view().best(), new_best);
    }
}

#[test]
fn rollback_to_an_ancestor_has_no_new_blocks() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let fork = btc.block_at(3).unwrap();
    let removed = vec![btc.block_at(5).unwrap(), btc.block_at(4).unwrap()];

    btc.set_best(fork);

    assert_eq!(
        event(&mut watcher),
        BtcEvent::Reorg {
            fork,
            removed,
            blocks: vec![]
        }
    );
    assert_eq!(watcher.view().best(), fork);
}

#[test]
fn rollback_to_the_genesis_block_is_not_its_replacement() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let genesis_id = watcher.view().genesis_id();

    btc.set_best(genesis_id);

    assert_eq!(event(&mut watcher).removed().len(), 3);
    assert_eq!(watcher.view().best(), genesis_id);
}

#[test]
fn a_best_block_that_left_and_came_back_is_not_an_event() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let best = btc.best();

    // A block of the same height on a sibling branch was the best one for a
    // moment.
    let rival = btc.mine_on(btc.block_at(4).unwrap());
    btc.set_best(rival);
    btc.set_best(best);

    assert!(waits(&mut watcher));
}

#[test]
fn a_source_error_is_retryable_and_leaves_the_view_untouched() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let before = *watcher.view();
    btc.mine_n(3);

    // Source calls: 0 waits for the best block, 1…3 fetch headers. The second
    // header fails, in the middle of the walk down.
    btc.fail_call(2);
    let failure = error(&mut watcher);
    assert!(matches!(failure, WatchError::Source(_)));
    assert!(!failure.is_fatal());
    assert_eq!(*watcher.view(), before);

    // A retry reports everything.
    assert_eq!(heights(&event(&mut watcher)), [6, 7, 8]);
}

#[test]
fn a_failed_download_is_retried_without_a_new_search() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let before = *watcher.view();
    btc.mine_n(3);

    // Source calls: 0 waits for the best block, 1…3 fetch headers, 4…6
    // download blocks. The second download fails.
    btc.fail_call(5);
    assert!(matches!(error(&mut watcher), WatchError::Source(_)));
    assert_eq!(*watcher.view(), before);

    // The retry keeps the path it has found and the block it has stored: it
    // only downloads the two blocks that are missing.
    let calls = btc.calls();
    assert_eq!(heights(&event(&mut watcher)), [6, 7, 8]);
    assert_eq!(btc.calls() - calls, 2);
}

#[test]
fn a_waiting_call_can_be_resumed_or_dropped() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);

    // The same call lives to see the block.
    {
        let mut call = pin!(watcher.next());
        assert!(call.as_mut().now_or_never().is_none());
        btc.mine();
        let got = call.as_mut().now_or_never().unwrap().unwrap();
        assert_eq!(heights(&got), [6]);
    }

    // The call is dropped (this is what cancellation in `select!` looks like)
    // and the block appears later: the next call reports it.
    assert!(waits(&mut watcher));
    btc.mine();
    assert_eq!(heights(&event(&mut watcher)), [7]);
}

#[test]
fn replacing_the_genesis_block_is_fatal() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let before = *watcher.view();
    let genesis_id = before.genesis_id();

    // A reorg from height 1: the block at the height of the genesis
    // block (2) is now another one.
    let depth = btc.best().height - 1;
    btc.reorg(depth, depth + 1);

    // The error repeats on every call, the view is untouched.
    for _ in 0..2 {
        let failure = error(&mut watcher);
        assert!(failure.is_fatal());
        assert!(matches!(
            failure,
            WatchError::GenesisBlockReplaced { genesis_id: s, found }
                if s == genesis_id && found.height == genesis_id.height && found != genesis_id
        ));
    }
    assert_eq!(*watcher.view(), before);
}

#[test]
fn a_source_behind_the_genesis_block_is_not_fatal() {
    let btc = MemoryBtc::new();
    btc.mine();
    // The Bitcoin node of this Flame node is still syncing: it sees blocks 0
    // and 1.
    let (source, lag) = btc.lagging_source();
    btc.mine_n(4);
    let genesis_id = btc.block_at(3).unwrap();
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), genesis_id);

    let failure = error(&mut watcher);
    assert!(matches!(failure, WatchError::SourceBehindGenesis { .. }));
    assert!(!failure.is_fatal());

    lag.catch_up();
    assert_eq!(heights(&event(&mut watcher)), [4, 5]);
}

#[test]
fn long_branches_arrive_as_several_events() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(5);
    let mut watcher =
        BtcWatcher::new(btc.source(), MemoryStorage::new(), genesis_id).max_blocks_per_event(2);

    assert_eq!(heights(&event(&mut watcher)), [1, 2]);
    assert_eq!(heights(&event(&mut watcher)), [3, 4]);
    assert_eq!(heights(&event(&mut watcher)), [5]);
    assert!(waits(&mut watcher));

    // A reorg is cut the same way: the first piece carries the removed blocks,
    // the rest is growth.
    btc.reorg(2, 3);
    let first = event(&mut watcher);
    assert_eq!(first.removed().len(), 2);
    assert_eq!(heights(&first), [4, 5]);
    let rest = event(&mut watcher);
    assert!(matches!(rest, BtcEvent::NewBlocks { .. }));
    assert_eq!(heights(&rest), [6]);
}

#[test]
fn the_storage_holds_the_chain_of_the_view_and_nothing_else() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let storage = watcher.storage().clone();
    assert_eq!(storage.len(), 3);

    let old = [btc.block_at(5).unwrap(), btc.block_at(4).unwrap()];
    btc.reorg(2, 3);

    // The new blocks are in the storage by the time of the event.
    let got = event(&mut watcher);
    assert!(got.blocks().iter().all(|block| storage.contains(*block)));

    // The removed blocks leave on the next call.
    assert!(waits(&mut watcher));
    assert!(old.iter().all(|block| !storage.contains(*block)));
    // What is left is the best chain above the genesis block (height 2).
    let chain = btc.best_chain();
    assert!(chain[3..].iter().all(|block| storage.contains(*block)));
    assert_eq!(storage.len(), chain[3..].len());
}

#[test]
fn a_consumer_reads_blocks_from_the_storage() {
    let (btc, mut watcher) = setup();
    // All a consumer needs is the event and a handle to the storage.
    let storage = watcher.storage().clone();
    let got = event(&mut watcher);

    // The block an event names is there in full.
    let id = got.blocks()[0];
    let block = storage.block(id).now_or_never().unwrap().unwrap().unwrap();
    assert_eq!(block.block_hash(), id.hash);

    // A block at any height of the chain is found from the best block.
    let best = watcher.view().best();
    let at = |height| storage.ancestor(best, height).now_or_never().unwrap();
    assert_eq!(at(4), Ok(btc.block_at(4)));
    assert_eq!(at(2), Ok(Some(watcher.view().genesis_id())));
    // There is nothing above the best block, and the storage holds nothing
    // below the genesis block.
    assert_eq!(at(6), Ok(None));
    assert_eq!(at(1), Ok(None));
}

#[test]
fn a_block_comes_with_the_outputs_its_inputs_spend() {
    let btc = MemoryBtc::new();
    let mut watcher = BtcWatcher::new(btc.source(), MemoryStorage::new(), btc.best());
    let storage = watcher.storage().clone();

    // Block 1 creates an output. Block 2 spends it, and then spends the
    // output of that transaction too.
    let funding = spend(OutPoint::null(), 5_000);
    let first = spend(OutPoint::new(funding.compute_txid(), 0), 4_000);
    let second = spend(OutPoint::new(first.compute_txid(), 0), 3_000);
    let created = btc.mine_with(vec![funding.clone()]);
    let spent = btc.mine_with(vec![first.clone(), second.clone()]);
    assert_eq!(heights(&event(&mut watcher)), [1, 2]);

    // The block has its transactions, and a merkle root over them.
    let block = storage.block(spent).now_or_never().unwrap();
    let block = block.unwrap().expect("the block of an event is stored");
    assert_eq!(block.txdata, [first.clone(), second]);
    assert!(block.check_merkle_root());

    // Every input comes with the output it spends.
    let prevouts = |id| storage.prevouts(id).now_or_never().unwrap().unwrap();
    assert_eq!(
        prevouts(spent).unwrap().as_slice(),
        [
            vec![Some(funding.output[0].clone())],
            vec![Some(first.output[0].clone())]
        ]
    );
    // An input that spends nothing, like the input of a coinbase, has none.
    assert_eq!(prevouts(created).unwrap().as_slice(), [vec![None]]);
    // The prevouts of a block that is not stored are not stored either.
    assert_eq!(prevouts(btc.block_at(0).unwrap()), None);
}

/// A source that returns blocks without the outputs their inputs spend.
struct LosesPrevouts {
    inner: MemorySource,
}

impl BlockIndex for LosesPrevouts {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        self.inner.header(id).await
    }
}

impl ChainSource for LosesPrevouts {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        let block = self.inner.block(hash).await?;
        Ok(block.map(|(block, _)| (block, Prevouts::default())))
    }

    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        self.inner.wait_for_new_block(known).await
    }
}

#[test]
fn prevouts_that_do_not_match_the_block_are_a_source_error() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    let source = LosesPrevouts {
        inner: btc.source(),
    };
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), genesis_id);

    // The block has a transaction with an input, and the answer has no entry
    // for it: the block is not stored, and the consumer does not hear of it.
    btc.mine_with(vec![spend(OutPoint::null(), 5_000)]);
    assert!(matches!(error(&mut watcher), WatchError::Source(_)));
    assert!(watcher.storage().is_empty());
    assert_eq!(watcher.view().best(), genesis_id);
}

/// A view saved with one Bitcoin node and continued with another, which is on
/// another branch and has never seen the blocks of the first.
#[test]
fn a_view_the_source_does_not_know_is_fatal() {
    let first = MemoryBtc::new();
    let genesis_id = first.best();
    first.mine_n(3);
    let mut watcher = BtcWatcher::new(first.source(), MemoryStorage::new(), genesis_id);
    event(&mut watcher);
    let (view, storage) = (*watcher.view(), watcher.storage().clone());

    let other = MemoryBtc::new();
    other.mine();
    other.reorg(1, 3);
    assert_eq!(other.block_at(0), Some(genesis_id));
    assert_ne!(other.best(), view.best());

    // The storage did not survive either. The blocks to remove cannot be
    // found: nobody has the header of the best block of the view.
    let mut lost = BtcWatcher::resume(other.source(), MemoryStorage::new(), view);
    for _ in 0..2 {
        let failure = error(&mut lost);
        assert!(failure.is_fatal());
        assert_eq!(failure, WatchError::ViewBlockUnknown { block: view.best() });
    }
    assert_eq!(*lost.view(), view);

    // With the storage of the first run the headers of the view are at hand,
    // and the watcher gets to the chain of the other node.
    let mut kept = BtcWatcher::resume(other.source(), storage, view);
    let got = event(&mut kept);
    assert_eq!(got.removed().len(), 3);
    assert_eq!(heights(&got), [1, 2, 3]);
    assert_eq!(kept.view().best(), other.best());
}

#[test]
fn a_restart_continues_from_the_saved_view() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    // The node saved its view and stopped. What it has to save is two block
    // ids: the genesis block and the best block.
    let (genesis_id, best) = (watcher.view().genesis_id(), watcher.view().best());
    let storage = watcher.storage().clone();
    drop(watcher);

    // While it was down, the chain moved to another branch.
    btc.reorg(1, 3);
    let saved = BtcView::restore(genesis_id, best);

    // The storage survived the restart. The source is asked for the best
    // block, three headers and three blocks. The header of the removed block
    // comes from the storage.
    let before = btc.calls();
    let mut watcher = BtcWatcher::resume(btc.source(), storage, saved);
    let got = event(&mut watcher);
    assert_eq!(got.removed().len(), 1);
    assert_eq!(heights(&got), [5, 6, 7]);
    assert_eq!(btc.calls() - before, 7);

    // The storage did not survive. The event is the same, and it costs one
    // more request: the header of the removed block comes from the source.
    let before = btc.calls();
    let mut watcher = BtcWatcher::resume(btc.source(), MemoryStorage::new(), saved);
    let got = event(&mut watcher);
    assert_eq!(got.removed().len(), 1);
    assert_eq!(heights(&got), [5, 6, 7]);
    assert_eq!(btc.calls() - before, 8);
}

#[test]
fn blocks_already_in_the_storage_are_not_requested_again() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let storage = watcher.storage().clone();
    drop(watcher);

    // The consumer lost its view and starts over from the genesis block,
    // but the storage is intact: the whole chain comes from it.
    let genesis_id = btc.block_at(2).unwrap();
    let before = btc.calls();
    let mut watcher = BtcWatcher::new(btc.source(), storage, genesis_id);
    assert_eq!(heights(&event(&mut watcher)), [3, 4, 5]);
    // The only source call asks for the best block.
    assert_eq!(btc.calls() - before, 1);
}

/// A watcher started from the best block of the source, ready now.
fn from_best(
    btc: &MemoryBtc,
    genesis_id: BlockId,
) -> Result<BtcWatcher<MemorySource, MemoryStorage>, WatchError> {
    BtcWatcher::from_best(btc.source(), MemoryStorage::new(), genesis_id)
        .now_or_never()
        .expect("the source answers at once")
}

#[test]
fn a_watcher_from_the_best_block_skips_the_chain_below_it() {
    let btc = MemoryBtc::new();
    btc.mine_n(50);
    let genesis_id = btc.block_at(2).unwrap();

    // Two source calls, however far the best block is from the genesis
    // block: the best block, and the block at the height of the genesis
    // block.
    let before = btc.calls();
    let mut watcher = from_best(&btc, genesis_id).unwrap();
    assert_eq!(btc.calls() - before, 2);
    assert_eq!(*watcher.view(), BtcView::restore(genesis_id, btc.best()));

    // Nothing to report and nothing downloaded.
    assert!(waits(&mut watcher));
    assert!(watcher.storage().is_empty());

    // From here it reports changes as any watcher does. The blocks a reorg
    // removes were never in the storage.
    btc.mine();
    assert_eq!(heights(&event(&mut watcher)), [51]);
    btc.reorg(3, 4);
    let got = event(&mut watcher);
    assert_eq!(got.removed().len(), 3);
    assert_eq!(heights(&got), [49, 50, 51, 52]);
    assert!(waits(&mut watcher));
    assert_eq!(watcher.view().best(), btc.best());
    assert_eq!(watcher.storage().len(), 4);
}

#[test]
fn a_watcher_from_the_best_block_checks_the_genesis_block() {
    let btc = MemoryBtc::new();
    btc.mine_n(5);
    let genesis_id = btc.block_at(2).unwrap();

    // The genesis block is the best block: one call is enough.
    let before = btc.calls();
    let watcher = from_best(&btc, btc.best()).unwrap();
    assert_eq!(btc.calls() - before, 1);
    assert_eq!(*watcher.view(), BtcView::new(btc.best()));

    // The Bitcoin node is still syncing and has not reached the genesis
    // block.
    let (source, _lag) = btc.lagging_source();
    let ahead = btc.mine_n(2);
    let failure = BtcWatcher::from_best(source, MemoryStorage::new(), ahead)
        .now_or_never()
        .unwrap()
        .err()
        .unwrap();
    assert!(matches!(failure, WatchError::SourceBehindGenesis { .. }));
    assert!(!failure.is_fatal());

    // The source is unreachable: nothing is fatal about that.
    btc.fail_call(1);
    let failure = from_best(&btc, genesis_id).err().unwrap();
    assert!(matches!(failure, WatchError::Source(_)));

    // The best chain does not pass through the genesis block.
    let depth = btc.best().height - 1;
    btc.reorg(depth, depth + 1);
    let failure = from_best(&btc, genesis_id).err().unwrap();
    assert!(failure.is_fatal());
    assert!(matches!(
        failure,
        WatchError::GenesisBlockReplaced { genesis_id: s, found }
            if s == genesis_id && found.height == genesis_id.height && found != genesis_id
    ));
}

#[test]
fn a_consumer_view_follows_the_source_through_anything() {
    // A random number generator with a fixed seed: every run is the same.
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut random = |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };

    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    let mut watcher =
        BtcWatcher::new(btc.source(), MemoryStorage::new(), genesis_id).max_blocks_per_event(3);
    let storage = watcher.storage().clone();
    // The view of the consumer: only events move it.
    let mut consumer = BtcView::new(genesis_id);
    let (mut grew, mut reorgs, mut rollbacks) = (0, 0, 0);

    for _ in 0..500 {
        let height = btc.best().height;
        match random(4) {
            0 | 1 => {
                btc.mine_n(1 + random(3));
            }
            2 if height > 0 => {
                let depth = 1 + random(height.min(6));
                btc.reorg(depth, random(depth + 3));
            }
            3 if height > 0 => btc.set_best(btc.block_at(random(height)).unwrap()),
            _ => {}
        }

        while let Some(event) = watcher.next().now_or_never() {
            let event = event.unwrap();
            match &event {
                BtcEvent::NewBlocks { .. } => grew += 1,
                BtcEvent::Reorg { blocks, .. } if blocks.is_empty() => rollbacks += 1,
                BtcEvent::Reorg { .. } => reorgs += 1,
            }
            consumer.apply(&event).unwrap();
        }

        // The consumer, the watcher and the source agree on the best block, and the
        // storage holds exactly the chain below it.
        assert_eq!(consumer, *watcher.view());
        assert_eq!(consumer.best(), btc.best());
        let chain = btc.best_chain();
        assert!(chain[1..].iter().all(|block| storage.contains(*block)));
        assert_eq!(storage.len(), chain[1..].len());
    }

    // The run did go through every kind of event.
    assert!(grew > 100 && reorgs > 50 && rollbacks > 20);
}

/// A source whose wait for a new block times out once, like a long poll over
/// RPC. Also an example of wrapping a source: failures and delays are made the
/// same way.
struct TimesOutOnce {
    inner: MemorySource,
    timed_out: AtomicBool,
}

impl BlockIndex for TimesOutOnce {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        self.inner.header(id).await
    }
}

impl ChainSource for TimesOutOnce {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        self.inner.block(hash).await
    }

    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        if !self.timed_out.swap(true, Ordering::Relaxed) {
            // The wait is over, the best block is the same.
            return Ok(BlockId {
                height: 0,
                hash: known,
            });
        }
        self.inner.wait_for_new_block(known).await
    }
}

#[test]
fn a_wait_that_timed_out_is_not_an_event() {
    let btc = MemoryBtc::new();
    let source = TimesOutOnce {
        inner: btc.source(),
        timed_out: AtomicBool::new(false),
    };
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), btc.best());

    assert!(waits(&mut watcher));
    assert!(watcher.source().timed_out.load(Ordering::Relaxed));

    btc.mine();
    assert_eq!(heights(&event(&mut watcher)), [1]);
}

/// A source that answers with the same header or the same block, whatever it
/// is asked for.
struct Confused {
    inner: MemorySource,
    header: Option<BlockId>,
    block: Option<BlockHash>,
}

impl BlockIndex for Confused {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        self.inner.header(self.header.unwrap_or(id)).await
    }
}

impl ChainSource for Confused {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        self.inner.block(self.block.unwrap_or(hash)).await
    }

    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        self.inner.wait_for_new_block(known).await
    }
}

#[test]
fn an_answer_that_was_not_asked_for_is_a_source_error() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    let first = btc.mine();
    btc.mine();

    // The watcher asks for the header of block 2 and gets the header of block 1.
    let source = Confused {
        inner: btc.source(),
        header: Some(first),
        block: None,
    };
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), genesis_id);
    assert!(matches!(error(&mut watcher), WatchError::Source(_)));
    assert_eq!(watcher.view().best(), genesis_id);

    // The headers are right, but every download brings block 1. Block 1 itself
    // is stored, block 2 is refused, and the consumer hears of neither.
    let source = Confused {
        inner: btc.source(),
        header: None,
        block: Some(first.hash),
    };
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), genesis_id);
    assert!(matches!(error(&mut watcher), WatchError::Source(_)));
    assert_eq!(watcher.view().best(), genesis_id);
    assert_eq!(watcher.storage().len(), 1);
}

/// A source that has dropped some blocks, the way a pruned bitcoind drops
/// blocks that left the best chain.
struct Forgetful {
    inner: MemorySource,
    forgotten: Mutex<HashSet<BlockHash>>,
}

impl BlockIndex for Forgetful {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        self.inner.header(id).await
    }
}

impl ChainSource for Forgetful {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        if self.forgotten.lock().unwrap().contains(&hash) {
            return Ok(None);
        }
        self.inner.block(hash).await
    }

    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        self.inner.wait_for_new_block(known).await
    }
}

#[test]
fn blocks_that_went_stale_while_waiting_do_not_block_the_watcher() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(3);
    let source = Forgetful {
        inner: btc.source(),
        forgotten: Mutex::new(HashSet::new()),
    };
    let mut watcher =
        BtcWatcher::new(source, MemoryStorage::new(), genesis_id).max_blocks_per_event(1);

    // Block 1 is reported; blocks 2 and 3 are found and wait for their turn.
    assert_eq!(heights(&event(&mut watcher)), [1]);

    // While they waited a reorg replaced them, and the source dropped them.
    let stale = [btc.block_at(2).unwrap(), btc.block_at(3).unwrap()];
    let new_best = btc.reorg(2, 3);
    watcher
        .source()
        .forgotten
        .lock()
        .unwrap()
        .extend(stale.map(|block| block.hash));
    assert!(matches!(error(&mut watcher), WatchError::Source(_)));

    // The watcher forgot the stale path and found a new one from the best
    // block of the source.
    while let Some(event) = watcher.next().now_or_never() {
        assert!(matches!(event.unwrap(), BtcEvent::NewBlocks { .. }));
    }
    assert_eq!(watcher.view().best(), new_best);
}

/// A source where Bitcoin grows by one block during the first download.
struct Grows {
    inner: MemorySource,
    btc: MemoryBtc,
    grew: AtomicBool,
}

impl BlockIndex for Grows {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        self.inner.header(id).await
    }
}

impl ChainSource for Grows {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        if !self.grew.swap(true, Ordering::Relaxed) {
            self.btc.mine();
        }
        self.inner.block(hash).await
    }

    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        self.inner.wait_for_new_block(known).await
    }
}

#[test]
fn a_block_that_appears_during_a_download_comes_in_the_next_event() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(2);
    let source = Grows {
        inner: btc.source(),
        btc: btc.clone(),
        grew: AtomicBool::new(false),
    };
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), genesis_id);

    // Block 3 is mined while block 1 is downloaded. The event carries the
    // blocks that were found before that, and the view stops at block 2.
    assert_eq!(heights(&event(&mut watcher)), [1, 2]);
    assert_eq!(watcher.view().best(), btc.block_at(2).unwrap());

    assert_eq!(heights(&event(&mut watcher)), [3]);
    assert!(waits(&mut watcher));
}

/// A storage whose next write or next removal fails, like a database that is
/// down for a moment.
struct FailsOnce {
    inner: MemoryStorage,
    put_fails: AtomicBool,
    remove_fails: AtomicBool,
}

impl BlockIndex for FailsOnce {
    type Error = StorageError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, StorageError> {
        self.inner.header(id).await
    }
}

impl BtcStorage for FailsOnce {
    async fn block(&self, id: BlockId) -> Result<Option<Block>, StorageError> {
        self.inner.block(id).await
    }

    async fn prevouts(&self, id: BlockId) -> Result<Option<Prevouts>, StorageError> {
        self.inner.prevouts(id).await
    }

    async fn put(&self, id: BlockId, block: Block, prevouts: Prevouts) -> Result<(), StorageError> {
        if self.put_fails.swap(false, Ordering::Relaxed) {
            return Err(StorageError("the database is down".into()));
        }
        self.inner.put(id, block, prevouts).await
    }

    async fn remove_many(&self, ids: &[BlockId]) -> Result<(), StorageError> {
        if self.remove_fails.swap(false, Ordering::Relaxed) {
            return Err(StorageError("the database is down".into()));
        }
        self.inner.remove_many(ids).await
    }
}

#[test]
fn a_storage_error_is_retryable_and_leaves_the_view_untouched() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(2);
    let storage = FailsOnce {
        inner: MemoryStorage::new(),
        put_fails: AtomicBool::new(true),
        remove_fails: AtomicBool::new(false),
    };
    let mut watcher = BtcWatcher::new(btc.source(), storage, genesis_id);

    let failure = error(&mut watcher);
    assert!(matches!(failure, WatchError::Storage(_)));
    assert!(!failure.is_fatal());
    assert_eq!(watcher.view().best(), genesis_id);

    // The consumer has not seen the blocks that could not be stored. A retry
    // reports them.
    assert_eq!(heights(&event(&mut watcher)), [1, 2]);
    assert_eq!(watcher.storage().inner.len(), 2);
}

#[test]
fn a_failed_removal_is_retried_and_loses_no_block() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(3);
    let storage = FailsOnce {
        inner: MemoryStorage::new(),
        put_fails: AtomicBool::new(false),
        remove_fails: AtomicBool::new(false),
    };
    let mut watcher = BtcWatcher::new(btc.source(), storage, genesis_id);
    event(&mut watcher);

    // A reorg removes blocks 2 and 3 and brings one new block.
    btc.reorg(2, 1);
    assert_eq!(event(&mut watcher).removed().len(), 2);

    // The next call asks the storage to drop the two removed blocks, and the
    // storage fails.
    watcher
        .storage()
        .remove_fails
        .store(true, Ordering::Relaxed);
    assert!(matches!(error(&mut watcher), WatchError::Storage(_)));

    // The retry drops both: the storage holds block 1 and the new block 2.
    assert!(waits(&mut watcher));
    assert_eq!(watcher.storage().inner.len(), 2);
}

/// A source whose `block` call takes two polls, like a request over a network,
/// and counts how many such calls are in flight.
struct Slow {
    inner: MemorySource,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
}

impl Slow {
    fn new(inner: MemorySource) -> Self {
        Slow {
            inner,
            in_flight: AtomicUsize::new(0),
            most_in_flight: AtomicUsize::new(0),
        }
    }
}

impl BlockIndex for Slow {
    type Error = SourceError;

    async fn header(&self, id: BlockId) -> Result<Option<Header>, SourceError> {
        self.inner.header(id).await
    }
}

impl ChainSource for Slow {
    async fn block(&self, hash: BlockHash) -> Result<Option<(Block, Prevouts)>, SourceError> {
        let in_flight = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.most_in_flight.fetch_max(in_flight, Ordering::Relaxed);
        // Give way once: the other downloads start before this one ends.
        let mut gave_way = false;
        poll_fn(|context| {
            if gave_way {
                return Poll::Ready(());
            }
            gave_way = true;
            context.waker().wake_by_ref();
            Poll::Pending
        })
        .await;
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.inner.block(hash).await
    }

    async fn wait_for_new_block(&self, known: BlockHash) -> Result<BlockId, SourceError> {
        self.inner.wait_for_new_block(known).await
    }
}

#[test]
fn the_blocks_of_an_event_are_downloaded_several_at_a_time() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(7);

    let mut watcher = BtcWatcher::new(Slow::new(btc.source()), MemoryStorage::new(), genesis_id)
        .parallel_downloads(3);
    let event = block_on(watcher.next()).unwrap();
    // The event still names the blocks in the order of the chain.
    assert_eq!(heights(&event), [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(watcher.storage().len(), 7);
    assert_eq!(watcher.source().most_in_flight.load(Ordering::Relaxed), 3);

    // With a limit of one the watcher asks for one block after another.
    let mut watcher = BtcWatcher::new(Slow::new(btc.source()), MemoryStorage::new(), genesis_id)
        .parallel_downloads(1);
    block_on(watcher.next()).unwrap();
    assert_eq!(watcher.source().most_in_flight.load(Ordering::Relaxed), 1);
}

#[test]
fn blocks_of_a_dropped_path_leave_the_storage() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(3);
    let old = [1, 2].map(|height| btc.block_at(height).unwrap());
    let source = Forgetful {
        inner: btc.source(),
        forgotten: Mutex::new(HashSet::from([btc.block_at(3).unwrap().hash])),
    };
    let mut watcher = BtcWatcher::new(source, MemoryStorage::new(), genesis_id);
    let storage = watcher.storage().clone();

    // Blocks 1 and 2 are stored, block 3 is gone: the path is dropped, and no
    // event names its blocks.
    assert!(matches!(error(&mut watcher), WatchError::Source(_)));
    assert!(old.iter().all(|block| storage.contains(*block)));

    // Another branch took over. The next call first removes what the dropped
    // path left behind.
    btc.reorg(3, 3);
    assert_eq!(heights(&event(&mut watcher)), [1, 2, 3]);
    assert!(old.iter().all(|block| !storage.contains(*block)));
    assert_eq!(storage.len(), 3);
}

#[test]
fn recent_blocks_keeps_only_the_latest_blocks() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(10);
    let mut watcher = BtcWatcher::new(btc.source(), RecentBlocks::new(3), genesis_id);
    let storage = watcher.storage().clone();

    // An event names no more blocks than the storage keeps, so the consumer
    // finds every block of the event there.
    for expected in [vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9], vec![10]] {
        let got = event(&mut watcher);
        assert_eq!(heights(&got), expected);
        assert!(got.blocks().iter().all(|block| storage.contains(*block)));
    }

    // What is left is the top of the chain. An older block is not here: it
    // stays in Bitcoin.
    let chain = btc.best_chain();
    assert_eq!(storage.len(), 3);
    assert!(chain[8..].iter().all(|block| storage.contains(*block)));
    let older = storage.block(chain[7]).now_or_never().unwrap();
    assert_eq!(older, Ok(None));

    // A reorg deeper than the storage: the watcher takes the headers it has
    // dropped from the source.
    let new_best = btc.reorg(5, 6);
    let first = event(&mut watcher);
    assert_eq!(first.removed().len(), 5);
    assert_eq!(heights(&first), [6, 7, 8]);
    assert!(first.blocks().iter().all(|block| storage.contains(*block)));
    assert_eq!(heights(&event(&mut watcher)), [9, 10, 11]);
    assert_eq!(watcher.view().best(), new_best);

    assert!(waits(&mut watcher));
    let chain = btc.best_chain();
    assert_eq!(storage.len(), 3);
    assert!(chain[9..].iter().all(|block| storage.contains(*block)));
}

#[test]
fn a_rewound_watcher_reports_the_blocks_above_again() {
    let (btc, mut watcher) = setup();
    assert_eq!(heights(&event(&mut watcher)), [3, 4, 5]);
    let [third, fourth, fifth] = [3, 4, 5].map(|height| btc.block_at(height).unwrap());

    // The consumer applied block 3 and failed on block 4: it goes back.
    let before = btc.calls();
    watcher.rewind_to(third);
    assert_eq!(watcher.view().best(), third);

    let blocks = vec![fourth, fifth];
    assert_eq!(event(&mut watcher), BtcEvent::NewBlocks { blocks });
    assert_eq!(watcher.view().best(), fifth);
    // The blocks were still in the storage: the only source call asks for
    // the best block.
    assert_eq!(btc.calls() - before, 1);
    assert!(waits(&mut watcher));
}

#[test]
fn a_watcher_rewound_to_a_removed_block_reports_the_reorg_from_it() {
    let (btc, mut watcher) = setup();
    event(&mut watcher);
    let [third, fourth, fifth] = [3, 4, 5].map(|height| btc.block_at(height).unwrap());

    // A reorg removes blocks 4 and 5 and brings three new ones.
    let new_best = btc.reorg(2, 3);
    let reorg = event(&mut watcher);
    assert_eq!(reorg.removed(), [fifth, fourth]);

    // The consumer did not apply the reorg at all: the same event comes
    // again, though the removed blocks have left the storage.
    watcher.rewind_to(fifth);
    assert_eq!(event(&mut watcher), reorg);

    // The consumer removed block 5 and failed on block 4: the rest of the
    // reorg comes.
    watcher.rewind_to(fourth);
    let rest = BtcEvent::Reorg {
        fork: third,
        removed: vec![fourth],
        blocks: reorg.blocks().to_vec(),
    };
    assert_eq!(event(&mut watcher), rest);
    assert_eq!(watcher.view().best(), new_best);
}

#[test]
fn a_bounded_storage_has_every_block_of_an_event_after_a_rewind() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.best();
    btc.mine_n(6);
    let mut watcher = BtcWatcher::new(btc.source(), RecentBlocks::new(3), genesis_id);
    let storage = watcher.storage().clone();
    event(&mut watcher);
    event(&mut watcher);
    // The storage holds blocks 4, 5 and 6.

    // The view goes back to block 3, and a reorg replaces blocks 5 and 6.
    // The next event names block 4, which is in the storage, and the two new
    // blocks. Storing them pushes out the blocks held the longest, and the
    // first of those is block 4: the watcher requests it again.
    watcher.rewind_to(btc.block_at(3).unwrap());
    btc.reorg(2, 2);
    let got = event(&mut watcher);
    assert_eq!(heights(&got), [4, 5, 6]);
    assert!(got.blocks().iter().all(|block| storage.contains(*block)));
}
