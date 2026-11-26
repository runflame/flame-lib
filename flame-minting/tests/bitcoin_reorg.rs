#[path = "common/bitcoin_reorg.rs"]
mod support;

use support::{Fixture, bitcoin_block};

#[tokio::test]
async fn reorg_restores_chain_selection_and_allows_further_processing() {
    let fixture = Fixture::new(2);
    let reorganized = fixture.node().await;
    let clean = fixture.node().await;
    let initial = clean.snapshot().await;
    let old = [
        bitcoin_block(2, 1, 1, &fixture.old_flame[..1]),
        bitcoin_block(3, 1, 2, &fixture.old_flame[1..]),
    ];
    for block in &old {
        reorganized.apply(block).await;
    }
    assert_eq!(
        reorganized.snapshot().await.flame_tip,
        fixture.old_flame[1].header.block_tip()
    );

    for block in old.iter().rev() {
        reorganized.rollback(block).await;
    }
    assert_eq!(reorganized.snapshot().await, initial);

    let replacement = bitcoin_block(2, 2, 1, &fixture.new_flame);
    reorganized.apply(&replacement).await;
    clean.apply(&replacement).await;
    let snapshot = reorganized.snapshot().await;
    assert_eq!(snapshot, clean.snapshot().await);
    assert_eq!(snapshot.btc_tip, replacement.id);
    assert_eq!(snapshot.flame_tip, fixture.new_flame[1].header.block_tip());

    let continuation = bitcoin_block(3, 2, 2, &fixture.old_flame);
    reorganized.apply(&continuation).await;
    clean.apply(&continuation).await;
    let snapshot = reorganized.snapshot().await;
    assert_eq!(snapshot, clean.snapshot().await);
    assert_eq!(snapshot.btc_tip, continuation.id);
    assert_eq!(snapshot.flame_tip, fixture.old_flame[1].header.block_tip());
}

#[tokio::test]
async fn reorg_replaces_old_branch_rewards_with_new_branch_rewards() {
    let fixture = Fixture::new(150);
    let reorganized = fixture.node().await;
    let clean = fixture.node().await;
    let old = bitcoin_block(
        2,
        1,
        1,
        &[fixture.old_flame[0].clone(), fixture.old_flame[149].clone()],
    );
    reorganized.apply(&old).await;
    let old_rewards = reorganized.rewards(200).await;
    assert_eq!(old_rewards.len(), 1);
    assert!(old_rewards[0].1 > 0);

    reorganized.rollback(&old).await;
    assert!(reorganized.rewards(200).await.is_empty());

    let replacement = bitcoin_block(
        2,
        2,
        2,
        &[fixture.new_flame[0].clone(), fixture.new_flame[149].clone()],
    );
    reorganized.apply(&replacement).await;
    clean.apply(&replacement).await;
    let new_rewards = reorganized.rewards(200).await;
    assert_eq!(new_rewards, clean.rewards(200).await);
    assert_eq!(new_rewards.len(), 1);
    assert!(new_rewards[0].1 > 0);
    assert_ne!(new_rewards[0].0, old_rewards[0].0);
}
