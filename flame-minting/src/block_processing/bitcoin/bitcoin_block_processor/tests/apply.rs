use super::*;

#[tokio::test]
async fn apply_completes_the_whole_block_and_rollback_restores_all_consensus_records() {
    let fixture = Fixture::new().await;
    let p = &fixture.processor;
    assert_eq!(p.apply(fixture.block()).await.unwrap(), btc_tip(101));
    fixture.assert_applied().await;
    assert_ne!(fixture.snapshot().await, p.consensus_storage.undo);
    assert_eq!(
        p.canonical_storage.state.lock().unwrap().commits,
        fixture.states[1..].iter().map(tip).collect::<Vec<_>>()
    );
    fixture.no_replanning().await;
    assert_eq!(p.apply(fixture.block()).await.unwrap(), btc_tip(101));
    assert_eq!(p.engine.calls.load(Ordering::SeqCst), 1);
    fixture.allow_planning();
    assert_eq!(p.rollback(btc_tip(101)).await.unwrap(), btc_tip(100));
    fixture.assert_restored().await;
    assert_eq!(p.rollback(btc_tip(101)).await.unwrap(), btc_tip(100));
    assert_eq!(p.canonical_storage.state.lock().unwrap().commits.len(), 8);
}

#[tokio::test]
async fn undo_read_failure_does_not_begin_an_operation() {
    let fixture = Fixture::new().await;
    let p = &fixture.processor;
    p.consensus_storage
        .reject_reads
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        p.apply(fixture.block()).await,
        Err(BlockProcessingError::Consensus("undo read rejected"))
    ));
    assert!(p.journal.pending_entry().await.unwrap().is_none());
    assert!(p.consensus_storage.writes.lock().unwrap().is_empty());
    assert_eq!(p.resume().await.unwrap(), btc_tip(100));
}

#[tokio::test]
async fn rejects_invalid_block_order_before_calculation() {
    let fixture = Fixture::new().await;
    let p = &fixture.processor;
    let mut block = fixture.block();
    block.id = btc_tip(102);
    assert!(matches!(
        p.apply(block).await,
        Err(BlockProcessingError::InvalidSuccessor { .. })
    ));
    assert_eq!(p.engine.calls.load(Ordering::SeqCst), 0);
    p.apply(fixture.block()).await.unwrap();
    assert!(matches!(
        p.rollback(btc_tip(100)).await,
        Err(BlockProcessingError::MissingEntry(_))
    ));
    fixture.assert_applied().await;
}
