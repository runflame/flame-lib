use std::sync::{Arc, atomic::Ordering};

use futures_util::FutureExt;

use super::*;
use crate::protocol::indexer::test_support::*;

fn source(rpc: &Arc<FakeRpc>) -> Arc<IndexedBlockSource<FakeRpc>> {
    Arc::new(IndexedBlockSource::new(Arc::new(BitcoinFacade::new(
        Arc::clone(rpc),
    ))))
}

#[tokio::test]
async fn parses_acquisitions_and_only_authenticated_votes() {
    let tip = block_tip(1, 1);
    let rpc = FakeRpc::new(
        linear_chain(0).block(
            tip,
            Some(block_hash(0)),
            vec![
                acquisition_transaction(1000),
                vote_transaction(true),
                vote_transaction(false),
            ],
        ),
        tip,
    );
    let block = source(&rpc).load(tip).await.unwrap();
    assert_eq!(block.btc_block_tip, tip);
    assert_eq!(block.acquisitions.len(), 1);
    assert_eq!(block.acquisitions[0].amount().to_sat(), 1000);
    assert_eq!(block.votes.len(), 1);
}

#[tokio::test]
async fn missing_or_misaligned_prevouts_fail_the_whole_load() {
    let mut missing = vote_transaction(true);
    missing.prevouts[0] = None;
    let mut malformed = vote_transaction(true);
    malformed.prevouts.clear();
    let rpc = FakeRpc::new(
        linear_chain(0)
            .block(
                block_tip(1, 1),
                Some(block_hash(0)),
                vec![acquisition_transaction(1)],
            )
            .block(block_tip(2, 2), Some(block_hash(1)), vec![missing])
            .block(block_tip(3, 3), Some(block_hash(2)), vec![malformed]),
        block_tip(3, 3),
    );
    let source = source(&rpc);
    assert!(matches!(
        source.load_many(&[block_tip(1, 1), block_tip(2, 2)]).await,
        Err(HistoryError::HistoryUnavailable(_))
    ));
    assert!(matches!(
        source.load(block_tip(3, 3)).await,
        Err(HistoryError::VoteProcessing(_))
    ));
}

#[tokio::test]
async fn cached_blocks_do_not_wait_for_busy_rpc_slots() {
    let rpc = FakeRpc::new(linear_chain(10), block_tip(10, 10));
    let source = source(&rpc);
    source.observe_tip(block_tip(10, 10)).await.unwrap();
    let cached = source.load(block_tip(10, 10)).await.unwrap();
    let gate = LoadGate::new();
    for h in 1..=4 {
        rpc.gates
            .lock()
            .unwrap()
            .insert(block_hash(h), Arc::clone(&gate));
    }
    let task = {
        let source = Arc::clone(&source);
        tokio::spawn(async move {
            source
                .load_many(&(1..=4).map(|h| block_tip(h, h)).collect::<Vec<_>>())
                .await
        })
    };
    gate.wait_for(4).await;
    let result = source
        .load(block_tip(10, 10))
        .now_or_never()
        .expect("cache hits must be ready even when all RPC slots are occupied")
        .unwrap();
    assert_eq!(result, cached);
    gate.release.add_permits(4);
    assert_eq!(task.await.unwrap().unwrap().len(), 4);
}

#[tokio::test]
async fn all_requests_share_four_slots_and_cancellation_releases_them() {
    let rpc = FakeRpc::new(linear_chain(8), block_tip(8, 8));
    let source = source(&rpc);
    let gate = LoadGate::new();
    for h in 1..=8 {
        rpc.gates
            .lock()
            .unwrap()
            .insert(block_hash(h), Arc::clone(&gate));
    }
    let first = {
        let source = Arc::clone(&source);
        tokio::spawn(async move {
            source
                .load_many(&(1..=8).map(|h| block_tip(h, h)).collect::<Vec<_>>())
                .await
        })
    };
    gate.wait_for(4).await;
    let second = {
        let source = Arc::clone(&source);
        tokio::spawn(async move {
            source
                .load_many(&(5..=8).map(|h| block_tip(h, h)).collect::<Vec<_>>())
                .await
        })
    };
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    gate.wait_for(4).await;
    assert_eq!(rpc.max_active.load(Ordering::SeqCst), 4);
    gate.release.add_permits(4);
    assert_eq!(
        second.await.unwrap().unwrap().last().unwrap().btc_block_tip,
        block_tip(8, 8)
    );
}
