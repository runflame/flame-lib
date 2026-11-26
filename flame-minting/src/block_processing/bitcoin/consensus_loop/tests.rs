//! The loop over Bitcoin in memory: no bitcoind and no network.

use super::*;
use crate::ConsensusStatus;
use crate::block_processing::{
    BlockProcessor, ConsensusManager,
    journal::consensus::InMemoryConsensusJournal,
    tests::support::{ArrivalHandlerProcessor, wait_until},
};
use btc_integration::BtcBlockRecords;
use btc_integration::watcher::WatchError;
use btc_integration::watcher::impls::{MemoryBtc, RecentBlocks};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Bitcoin consensus that only remembers what it was asked to do.
struct Processor {
    /// The blocks consensus has applied, the lowest first.
    chain: StdMutex<Vec<BlockId>>,
    /// The cursor after every apply and every rollback.
    cursors: StdMutex<Vec<BlockId>>,
    /// The next apply fails.
    fail: AtomicBool,
}

impl Processor {
    fn cursor(&self) -> BlockId {
        *self.chain.lock().unwrap().last().unwrap()
    }
}

impl BlockProcessor for Processor {
    type Error = &'static str;
    async fn apply(&self, block: BtcBlockRecords) -> Result<BlockId, Self::Error> {
        if self.fail.swap(false, Ordering::Relaxed) {
            return Err("apply failed");
        }
        self.chain.lock().unwrap().push(block.id);
        self.cursors.lock().unwrap().push(block.id);
        Ok(block.id)
    }
    async fn resume(&self) -> Result<BlockId, Self::Error> {
        Ok(self.cursor())
    }
    async fn rollback(&self, removed: BlockId) -> Result<BlockId, Self::Error> {
        assert_eq!(self.chain.lock().unwrap().pop(), Some(removed));
        let previous = self.cursor();
        self.cursors.lock().unwrap().push(previous);
        Ok(previous)
    }
}

struct ArrivalHandler;

impl crate::FlameBlockArrivalHandler for ArrivalHandler {
    type Error = std::convert::Infallible;
    async fn on_flame_block_arrival(&self, _: flamechain::BlockTip) -> Result<(), Self::Error> {
        Ok(())
    }
}

type TestManager =
    ConsensusManager<Processor, ArrivalHandlerProcessor<ArrivalHandler>, InMemoryConsensusJournal>;
type TestLoop =
    ConsensusLoop<Processor, ArrivalHandlerProcessor<ArrivalHandler>, InMemoryConsensusJournal>;

/// Consensus that has applied `chain`.
fn manager(chain: &[BlockId], fail: bool) -> TestManager {
    let processor = Processor {
        chain: StdMutex::new(chain.to_vec()),
        cursors: StdMutex::new(vec![]),
        fail: AtomicBool::new(fail),
    };
    ConsensusManager::new(
        processor,
        ArrivalHandlerProcessor::new(ArrivalHandler),
        InMemoryConsensusJournal::new(*chain.last().unwrap()),
    )
}

/// The loop of consensus that has applied `chain`: its first block is the
/// lowest one consensus knows.
fn consensus(chain: &[BlockId], fail: bool) -> TestLoop {
    ConsensusLoop::new(manager(chain, fail), chain[0])
}

fn cursors(consensus: &TestLoop) -> Vec<BlockId> {
    let processor = consensus.manager().bitcoin_processor();
    processor.cursors.lock().unwrap().clone()
}

/// The loop ended by itself and stopped consensus.
async fn stopped(consensus: &TestLoop) {
    wait_until(|| async { consensus.manager().status() == ConsensusStatus::Stopped }).await;
}

#[tokio::test]
async fn reorg_rolls_back_old_blocks_before_applying_the_new_branch() {
    let btc = MemoryBtc::new();
    btc.mine_n(3);
    let old = btc.best_chain();
    let consensus = consensus(&old, false);
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();

    btc.reorg(2, 2);

    let new = btc.best_chain();
    wait_until(|| async { cursors(&consensus).len() == 4 }).await;
    consensus.shutdown().await.unwrap();
    assert_eq!(cursors(&consensus), vec![old[2], old[1], new[2], new[3]]);
}

#[tokio::test]
async fn the_loop_follows_bitcoin_from_the_block_consensus_recovered() {
    let btc = MemoryBtc::new();
    btc.mine_n(3);
    let chain = btc.best_chain();
    // Consensus stopped at block 2, and Bitcoin is at block 3.
    let consensus = consensus(&chain[..=2], false);
    let mut best = consensus.subscribe();
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();

    // The first block the minter hears of is the best block of Bitcoin, not
    // the block consensus stopped at.
    let heard = *best.wait_for(Option::is_some).await.unwrap();
    assert_eq!(heard, Some(chain[3]));

    wait_until(|| async { !cursors(&consensus).is_empty() }).await;
    consensus.shutdown().await.unwrap();
    assert_eq!(cursors(&consensus), vec![chain[3]]);
}

#[tokio::test]
async fn a_loop_started_again_forgets_the_best_block_of_the_last_run() {
    let btc = MemoryBtc::new();
    let consensus = consensus(&[btc.best()], false);
    let best = consensus.subscribe();
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();
    let old = btc.mine();
    wait_until(|| async { !cursors(&consensus).is_empty() }).await;
    consensus.shutdown().await.unwrap();
    btc.mine();

    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();

    // Bitcoin went on while the loop was down.
    assert_ne!(*best.borrow(), Some(old));
    consensus.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_cancelled_loop_does_not_submit_more_events() {
    let btc = MemoryBtc::new();
    let genesis = btc.best();
    btc.mine();
    let manager = manager(&[genesis], false);
    manager.startup().await.unwrap();
    let watcher =
        BtcRecordsWatcher::resume(btc.source(), RecentBlocks::default(), BtcView::new(genesis));
    let best = watch::channel(None).0;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    TestLoop::run(&manager, watcher, &best, cancellation)
        .await
        .unwrap();

    manager.shutdown().await.unwrap();
    assert_eq!(*best.borrow(), None);
    assert!(
        manager
            .bitcoin_processor()
            .cursors
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn consensus_that_failed_goes_on_from_the_block_it_has_applied() {
    let btc = MemoryBtc::new();
    let genesis = btc.best();
    // The first apply fails.
    let consensus = consensus(&[genesis], true);
    let first = btc.mine();
    let second = btc.mine();
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();

    // Nobody starts consensus again: the loop does. Both blocks were handed
    // over, the first one failed and the second one was dropped with the
    // queue: both come again.
    wait_until(|| async { cursors(&consensus).len() == 2 }).await;
    assert_eq!(consensus.manager().status(), ConsensusStatus::Running);
    consensus.shutdown().await.unwrap();
    assert_eq!(cursors(&consensus), vec![first, second]);
}

#[tokio::test]
async fn a_source_that_did_not_answer_is_asked_again() {
    let btc = MemoryBtc::new();
    let genesis = btc.best();
    let consensus = consensus(&[genesis], false);
    let block = btc.mine();
    btc.fail_call(0);
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();

    wait_until(|| async { !cursors(&consensus).is_empty() }).await;
    consensus.shutdown().await.unwrap();
    assert_eq!(cursors(&consensus), vec![block]);
}

#[tokio::test]
async fn the_loop_ends_when_bitcoin_replaces_the_lowest_block_of_consensus() {
    let btc = MemoryBtc::new();
    let lowest = btc.mine();
    let consensus = consensus(&[lowest], false);
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();

    btc.reorg(1, 2);

    stopped(&consensus).await;
    assert!(matches!(
        consensus.shutdown().await,
        Err(ConsensusLoopError::Watch(BtcRecordsWatchError::Watch(
            WatchError::GenesisBlockReplaced { .. }
        )))
    ));
    assert!(cursors(&consensus).is_empty());
}
