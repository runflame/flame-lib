use std::{num::NonZeroUsize, sync::Arc};

use super::{ProtocolIndexer, StartupError};
use crate::{
    btc::bitcoin_facade::BitcoinFacade,
    protocol::{HistoryChange, HistoryError, indexer::test_support::*},
};

fn indexer(rpc: &Arc<FakeRpc>) -> Arc<ProtocolIndexer<FakeRpc>> {
    Arc::new(ProtocolIndexer::new(Arc::new(BitcoinFacade::new(
        Arc::clone(rpc),
    ))))
}
fn limit(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}

#[tokio::test]
async fn startup_retains_tip_without_subscribers_and_restart_keeps_existing_receivers() {
    let genesis = block_tip(0, 0);
    let rpc = FakeRpc::new(linear_chain(0), genesis);
    let indexer = indexer(&rpc);
    assert!(matches!(
        indexer.get_history(genesis, limit(1)).await,
        Err(HistoryError::NotRunning)
    ));
    indexer.startup().await.unwrap();
    let mut signals = indexer.subscribe();
    assert_eq!(*signals.borrow_and_update(), Some(genesis));
    assert!(matches!(
        indexer.startup().await,
        Err(StartupError::AlreadyRunning)
    ));
    indexer.shutdown().await.unwrap();
    assert_eq!(recv_tip(&mut signals).await, None);
    indexer.shutdown().await.unwrap();
    assert!(matches!(
        indexer.get_history(genesis, limit(1)).await,
        Err(HistoryError::NotRunning)
    ));
    indexer.startup().await.unwrap();
    assert_eq!(recv_tip(&mut signals).await, Some(genesis));
    assert!(
        matches!(indexer.get_history(genesis, limit(1)).await.unwrap().change,
        HistoryChange::Extension { new_blocks } if new_blocks.is_empty())
    );
    indexer.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_subscribers_coalesce_notifications_and_fetch_independent_complete_histories() {
    let rpc = FakeRpc::new(linear_chain(25), block_tip(0, 0));
    let indexer = indexer(&rpc);
    let mut slow = indexer.subscribe();
    let mut fast = indexer.subscribe();
    indexer.startup().await.unwrap();
    assert_eq!(recv_tip(&mut fast).await, Some(block_tip(0, 0)));
    slow.borrow_and_update();
    for h in 1..=25 {
        rpc.set_best_tip(block_tip(h, h)).await;
        rpc.announce(block_tip(h, h));
        assert_eq!(recv_tip(&mut fast).await, Some(block_tip(h, h)));
    }
    assert_eq!(recv_tip(&mut slow).await, Some(block_tip(25, 25)));
    assert!(!slow.has_changed().unwrap());
    let slow_update = indexer
        .get_history(block_tip(0, 0), limit(100))
        .await
        .unwrap();
    let fast_update = indexer
        .get_history(block_tip(24, 24), limit(100))
        .await
        .unwrap();
    assert!(
        matches!(slow_update.change, HistoryChange::Extension { new_blocks } if new_blocks.len() == 25)
    );
    assert!(
        matches!(fast_update.change, HistoryChange::Extension { new_blocks } if new_blocks.len() == 1)
    );
    indexer.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_height_reorg_round_trip_and_shorter_tip_leave_notifications_pending() {
    let a = block_tip(1, 1);
    let b = block_tip(101, 1);
    let rpc = FakeRpc::new(linear_chain(1).block(b, Some(block_hash(0)), vec![]), a);
    let indexer = indexer(&rpc);
    indexer.startup().await.unwrap();
    let mut slow = indexer.subscribe();
    let mut observer = indexer.subscribe();
    for tip in [b, a] {
        rpc.set_best_tip(tip).await;
        rpc.announce(tip);
        assert_eq!(recv_tip(&mut observer).await, Some(tip));
    }
    assert!(slow.has_changed().unwrap());
    assert_eq!(recv_tip(&mut slow).await, Some(a));
    let genesis = block_tip(0, 0);
    rpc.set_best_tip(genesis).await;
    rpc.announce(genesis);
    assert_eq!(recv_tip(&mut slow).await, Some(genesis));
    indexer.shutdown().await.unwrap();
}

#[tokio::test]
async fn notification_during_fetch_stays_pending_and_shutdown_cancels_inflight_history() {
    let rpc = FakeRpc::new(linear_chain(2), block_tip(1, 1));
    let indexer = indexer(&rpc);
    indexer.startup().await.unwrap();
    let mut signals = indexer.subscribe();
    let mut observer = indexer.subscribe();
    signals.borrow_and_update();
    let gate = LoadGate::new();
    rpc.gates
        .lock()
        .unwrap()
        .insert(block_hash(1), Arc::clone(&gate));
    let task = {
        let indexer = Arc::clone(&indexer);
        tokio::spawn(async move { indexer.get_history(block_tip(0, 0), limit(1)).await })
    };
    gate.wait_for(1).await;
    rpc.set_best_tip(block_tip(2, 2)).await;
    rpc.announce(block_tip(2, 2));
    assert_eq!(recv_tip(&mut observer).await, Some(block_tip(2, 2)));
    gate.release.add_permits(1);
    let update = task.await.unwrap().unwrap();
    assert_eq!(update.next_cursor, block_tip(1, 1));
    assert_eq!(recv_tip(&mut signals).await, Some(block_tip(2, 2)));

    rpc.gates
        .lock()
        .unwrap()
        .insert(block_hash(2), Arc::clone(&gate));
    let task = {
        let indexer = Arc::clone(&indexer);
        tokio::spawn(async move { indexer.get_history(update.next_cursor, limit(1)).await })
    };
    gate.wait_for(1).await;
    indexer.shutdown().await.unwrap();
    assert!(matches!(task.await.unwrap(), Err(HistoryError::NotRunning)));
    // Restart must not revive a request from the cancelled generation.
    rpc.gates.lock().unwrap().clear();
    indexer.startup().await.unwrap();
    assert_eq!(
        indexer
            .get_history(block_tip(1, 1), limit(1))
            .await
            .unwrap()
            .next_cursor,
        block_tip(2, 2)
    );
    indexer.shutdown().await.unwrap();
}
