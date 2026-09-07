use std::{num::NonZeroUsize, sync::Arc};

use super::*;
use crate::{btc::bitcoin_facade::BitcoinFacade, protocol::indexer::test_support::*};
use corepc_client::client_sync::Error as RpcError;

fn reader(rpc: &Arc<FakeRpc>) -> Arc<HistoryReader<FakeRpc>> {
    let bitcoin = Arc::new(BitcoinFacade::new(Arc::clone(rpc)));
    let source = Arc::new(IndexedBlockSource::new(Arc::clone(&bitcoin)));
    Arc::new(HistoryReader::new(bitcoin, source))
}
fn limit(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
fn blocks(update: &HistoryUpdate) -> &[IndexedBlock] {
    match &update.change {
        HistoryChange::Extension { new_blocks } | HistoryChange::Reorg { new_blocks, .. } => {
            new_blocks
        }
    }
}

#[tokio::test]
async fn paginates_old_history_and_empty_blocks_without_loading_beyond_the_page() {
    let tip = block_tip(25, 25);
    let rpc = FakeRpc::new(linear_chain(25), tip);
    let history = reader(&rpc);
    history.blocks.observe_tip(tip).await.unwrap();
    let mut cursor = block_tip(0, 0);
    let mut heights = vec![];
    loop {
        let update = history.get_history(cursor, limit(3)).await.unwrap();
        assert_eq!(update.target_tip, tip);
        assert!(blocks(&update).len() <= 3);
        heights.extend(blocks(&update).iter().map(|b| b.btc_block_tip.height));
        cursor = update.next_cursor;
        if cursor == update.target_tip {
            break;
        }
        assert_eq!(rpc.block_calls.lock().unwrap().len(), heights.len());
    }
    assert_eq!(heights, (1..=25).collect::<Vec<_>>());
    let at_tip = history.get_history(tip, limit(3)).await.unwrap();
    assert_eq!(at_tip.next_cursor, tip);
    assert!(
        matches!(at_tip.change, HistoryChange::Extension { new_blocks } if new_blocks.is_empty())
    );

    let calls = rpc.block_calls.lock().unwrap().len();
    let cached = history
        .get_history(block_tip(15, 15), limit(10))
        .await
        .unwrap();
    assert_eq!(rpc.block_calls.lock().unwrap().len(), calls);
    assert_eq!(
        cached,
        reader(&rpc)
            .get_history(block_tip(15, 15), limit(10))
            .await
            .unwrap()
    );
    history
        .get_history(block_tip(0, 0), limit(2))
        .await
        .unwrap();
    let calls = rpc.block_calls.lock().unwrap().len();
    history
        .get_history(block_tip(15, 15), limit(10))
        .await
        .unwrap();
    assert_eq!(rpc.block_calls.lock().unwrap().len(), calls);
    // The block immediately outside the ten-block window still requires RPC.
    history
        .get_history(block_tip(14, 14), limit(1))
        .await
        .unwrap();
    assert_eq!(rpc.block_calls.lock().unwrap().len(), calls + 1);
}

#[tokio::test]
async fn deep_reorg_returns_all_removed_tips_in_rollback_order_and_pages_only_new_blocks() {
    let mut chain = linear_chain(25);
    for h in 1..=22 {
        chain = chain.block(
            block_tip(100 + h, h),
            Some(if h == 1 {
                block_hash(0)
            } else {
                block_hash(99 + h)
            }),
            vec![],
        );
    }
    let target = block_tip(122, 22);
    let rpc = FakeRpc::new(chain, target);
    let history = reader(&rpc);
    history.blocks.observe_tip(target).await.unwrap();
    rpc.clear_header_calls();
    // The caller applied only 20 blocks of the old branch, not its tip at 25.
    let update = history
        .get_history(block_tip(20, 20), limit(2))
        .await
        .unwrap();
    match &update.change {
        HistoryChange::Reorg {
            removed_block_tips,
            new_blocks,
        } => {
            assert_eq!(
                *removed_block_tips,
                (1..=20).rev().map(|h| block_tip(h, h)).collect::<Vec<_>>()
            );
            assert_eq!(
                new_blocks
                    .iter()
                    .map(|b| b.btc_block_tip)
                    .collect::<Vec<_>>(),
                vec![block_tip(101, 1), block_tip(102, 2)]
            );
        }
        other => panic!("expected reorg: {other:?}"),
    }
    assert_eq!(update.next_cursor, block_tip(102, 2));
    assert!(rpc.header_calls().contains(&block_hash(1)));
    let next = history
        .get_history(update.next_cursor, limit(2))
        .await
        .unwrap();
    assert!(matches!(next.change, HistoryChange::Extension { .. }));
    assert_eq!(next.next_cursor, block_tip(104, 4));

    // Another reorg between pages must be relative to the new caller cursor.
    rpc.set_best_tip(block_tip(25, 25)).await;
    let back = history
        .get_history(next.next_cursor, limit(1))
        .await
        .unwrap();
    assert!(
        matches!(back.change, HistoryChange::Reorg { removed_block_tips, .. }
        if removed_block_tips == (1..=4).rev().map(|h| block_tip(100 + h, h)).collect::<Vec<_>>())
    );
    assert_eq!(back.next_cursor, block_tip(1, 1));
}

#[tokio::test]
async fn rollback_to_ancestor_has_no_new_blocks() {
    let rpc = FakeRpc::new(linear_chain(12), block_tip(3, 3));
    let update = reader(&rpc)
        .get_history(block_tip(12, 12), limit(1))
        .await
        .unwrap();
    assert_eq!(update.next_cursor, block_tip(3, 3));
    assert_eq!(update.target_tip, update.next_cursor);
    assert!(
        matches!(update.change, HistoryChange::Reorg { new_blocks, removed_block_tips }
        if new_blocks.is_empty() && removed_block_tips == (4..=12).rev().map(|h| block_tip(h, h)).collect::<Vec<_>>())
    );
}

#[tokio::test]
async fn invalid_cursors_limits_and_unavailable_history_are_distinct_from_rpc_errors() {
    let rpc = FakeRpc::new(linear_chain(2), block_tip(2, 2));
    let history = reader(&rpc);
    assert!(matches!(
        history.get_history(block_tip(0, 0), limit(1001)).await,
        Err(HistoryError::InvalidLimit)
    ));
    assert!(matches!(
        history.get_history(block_tip(99, 1), limit(1)).await,
        Err(HistoryError::UnknownCursor(_))
    ));
    assert!(matches!(
        history.get_history(block_tip(1, 9), limit(1)).await,
        Err(HistoryError::InvalidCursor(_))
    ));

    rpc.failures.lock().unwrap().insert(
        block_hash(1),
        rpc_error(-1, "Block not available (pruned data)"),
    );
    assert!(matches!(
        history.get_history(block_tip(0, 0), limit(2)).await,
        Err(HistoryError::HistoryUnavailable(_))
    ));
    rpc.failures.lock().unwrap().insert(
        block_hash(1),
        RpcError::Returned("temporary failure".into()),
    );
    assert!(matches!(
        history.get_history(block_tip(0, 0), limit(2)).await,
        Err(HistoryError::Rpc(_))
    ));
    assert_eq!(
        history
            .get_history(block_tip(0, 0), limit(2))
            .await
            .unwrap()
            .next_cursor,
        block_tip(2, 2)
    );
}

#[tokio::test]
async fn reorg_during_load_rejects_snapshot_and_does_not_repopulate_old_cache_window() {
    let old = block_tip(1, 1);
    let new = block_tip(101, 1);
    let rpc = FakeRpc::new(linear_chain(1).block(new, Some(block_hash(0)), vec![]), old);
    let history = reader(&rpc);
    history.blocks.observe_tip(old).await.unwrap();
    let gate = LoadGate::new();
    rpc.gates
        .lock()
        .unwrap()
        .insert(old.hash, Arc::clone(&gate));
    let task = {
        let history = Arc::clone(&history);
        tokio::spawn(async move { history.get_history(block_tip(0, 0), limit(10)).await })
    };
    gate.wait_for(1).await;
    rpc.set_best_tip(new).await;
    history.blocks.observe_tip(new).await.unwrap();
    gate.release.add_permits(1);
    assert!(matches!(
        task.await.unwrap(),
        Err(HistoryError::ChainChanged)
    ));
    assert_eq!(
        history
            .get_history(old, limit(1))
            .await
            .unwrap()
            .next_cursor,
        new
    );
    rpc.gates.lock().unwrap().remove(&old.hash);
    history.blocks.load(old).await.unwrap();
    assert_eq!(
        rpc.block_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|hash| **hash == old.hash)
            .count(),
        2
    );
}

#[tokio::test]
async fn growth_during_load_keeps_the_original_snapshot_valid() {
    let rpc = FakeRpc::new(linear_chain(2), block_tip(1, 1));
    let history = reader(&rpc);
    let gate = LoadGate::new();
    rpc.gates
        .lock()
        .unwrap()
        .insert(block_hash(1), Arc::clone(&gate));
    let task = tokio::spawn(async move { history.get_history(block_tip(0, 0), limit(10)).await });
    gate.wait_for(1).await;
    rpc.set_best_tip(block_tip(2, 2)).await;
    gate.release.add_permits(1);
    let update = task.await.unwrap().unwrap();
    assert_eq!(update.target_tip, block_tip(1, 1));
    assert_eq!(update.next_cursor, update.target_tip);
}

#[tokio::test]
async fn shorter_chain_during_load_returns_chain_changed() {
    let rpc = FakeRpc::new(linear_chain(1), block_tip(1, 1));
    let history = reader(&rpc);
    let gate = LoadGate::new();
    rpc.gates
        .lock()
        .unwrap()
        .insert(block_hash(1), Arc::clone(&gate));
    let task = tokio::spawn(async move { history.get_history(block_tip(0, 0), limit(10)).await });
    gate.wait_for(1).await;
    rpc.set_best_tip(block_tip(0, 0)).await;
    gate.release.add_permits(1);
    assert!(matches!(
        task.await.unwrap(),
        Err(HistoryError::ChainChanged)
    ));
}
