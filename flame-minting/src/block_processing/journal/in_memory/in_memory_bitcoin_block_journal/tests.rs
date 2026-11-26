use bitcoin::hashes::Hash;
use flame_chain_service::ChainPath;
use flamechain::{BlockHash as FlameBlockHash, BlockTip};
use flamevm::Predicate;

use crate::rewards::{PendingReward, RewardIndex, RewardsOutcome, RewardsUndo};

use super::*;

const CONSENSUS_STAGES: [ApplyStage; 6] = [
    ApplyStage::RemoveEndorsements,
    ApplyStage::StoreEndorsements,
    ApplyStage::StoreAcquisitions,
    ApplyStage::StoreBlockWeights,
    ApplyStage::StoreDoubleSigns,
    ApplyStage::StorePendingEndorsements,
];

const ROLLBACK_STAGES: [RollbackStage; 11] = [
    RollbackStage::RemoveEndorsements,
    RollbackStage::RestoreAcquisitions,
    RollbackStage::StoreDoubleSigns,
    RollbackStage::StoreEndorsements,
    RollbackStage::StorePendingEndorsements,
    RollbackStage::StoreBlockWeights,
    RollbackStage::PrepareFlameTransition,
    RollbackStage::ApplyFlameTransition,
    RollbackStage::PrepareRewards,
    RollbackStage::ApplyRewards,
    RollbackStage::StoreCursor,
];

fn tip(height: u64, hash: u8) -> BlockId {
    BlockId {
        hash: BlockHash::from_byte_array([hash; 32]),
        height,
    }
}

fn outcome(btc_tip: BlockId) -> MintingOutcome {
    MintingOutcome {
        consensus: crate::consensus::ConsensusOutcome::default(),
        next_btc_cursor: btc_tip,
    }
}

fn flame_tip(hash: u8) -> BlockTip {
    BlockTip {
        hash: FlameBlockHash::new([hash; 32]),
        height: 0.into(),
    }
}

fn undo(hash: u8) -> ConsensusUndo {
    ConsensusUndo {
        weighted_blocks: BTreeMap::from([(flame_tip(hash), None)]),
        ..ConsensusUndo::default()
    }
}

fn transition(hash: u8) -> FlameTransition {
    FlameTransition {
        previous_tip: flame_tip(hash),
        next_tip: flame_tip(hash),
        path: ChainPath::default(),
        attached_core_headers: Vec::new(),
    }
}

fn reward(amount: u64) -> PendingReward {
    PendingReward {
        flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
        access_predicate: Predicate::opaque(Predicate::unspendable_key()),
        amount,
    }
}

fn changes(amount: u64) -> RewardsChanges {
    RewardsChanges {
        outcomes: vec![RewardsOutcome {
            target_block_height: 200.into(),
            rewards: vec![reward(amount)],
        }],
        undo: RewardsUndo {
            rewards: BTreeMap::from([(
                (200.into(), RewardIndex::from(&reward(amount))),
                Some(reward(amount / 2)),
            )]),
        },
    }
}

async fn advance_to_flame(journal: &InMemoryBitcoinBlockJournal, btc_tip: BlockId) {
    for stage in CONSENSUS_STAGES {
        journal.complete_apply_stage(btc_tip, stage).await.unwrap();
        journal.complete_apply_stage(btc_tip, stage).await.unwrap();
    }
}

async fn advance_to_rewards(journal: &InMemoryBitcoinBlockJournal, btc_tip: BlockId) {
    advance_to_flame(journal, btc_tip).await;
    journal
        .store_flame_transition(btc_tip, &transition(1))
        .await
        .unwrap();
    journal
        .complete_apply_stage(btc_tip, ApplyStage::PrepareFlameTransition)
        .await
        .unwrap();
    journal
        .complete_apply_stage(btc_tip, ApplyStage::ApplyFlameTransition)
        .await
        .unwrap();
}

async fn finish_apply(journal: &InMemoryBitcoinBlockJournal, btc_tip: BlockId) {
    advance_to_rewards(journal, btc_tip).await;
    journal
        .store_rewards_changes(btc_tip, &changes(10))
        .await
        .unwrap();
    for stage in [
        ApplyStage::PrepareRewards,
        ApplyStage::ApplyRewards,
        ApplyStage::StoreCursor,
    ] {
        journal.complete_apply_stage(btc_tip, stage).await.unwrap();
    }
}

async fn finish_rollback(journal: &InMemoryBitcoinBlockJournal, btc_tip: BlockId) {
    journal.begin_rollback(btc_tip).await.unwrap();
    for stage in ROLLBACK_STAGES {
        journal
            .complete_rollback_stage(btc_tip, stage)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn pending_and_applied_entries_keep_owned_outcome_and_undo_snapshots() {
    let initial = tip(100, 1);
    let btc_tip = tip(101, 2);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    assert!(journal.get_entry(btc_tip).await.unwrap().is_none());
    assert!(journal.get_pending_entry().await.unwrap().is_none());
    let mut original = outcome(btc_tip);
    let mut original_undo = undo(1);
    journal
        .begin_apply(initial, &original, &original_undo)
        .await
        .unwrap();
    original.next_btc_cursor = tip(101, 3);
    original_undo.weighted_blocks.clear();
    let mut snapshot = journal.get_pending_entry().await.unwrap().unwrap();
    snapshot.consensus_undo.weighted_blocks.clear();
    snapshot.minting_outcome.next_btc_cursor = original.next_btc_cursor;
    snapshot.status = JournalStatus::Applied;
    assert_eq!(
        journal
            .begin_apply(initial, &original, &original_undo)
            .await,
        Err(Error::OperationInProgress(btc_tip))
    );
    let stored = journal.get_entry(btc_tip).await.unwrap().unwrap();
    assert_eq!(stored.minting_outcome, outcome(btc_tip));
    assert_eq!(stored.consensus_undo, undo(1));
    assert_eq!(
        stored.status,
        JournalStatus::Applying(ApplyStage::RemoveEndorsements)
    );
    finish_apply(&journal, btc_tip).await;
    assert!(journal.get_pending_entry().await.unwrap().is_none());
    let stored = journal.get_entry(btc_tip).await.unwrap().unwrap();
    assert_eq!(stored.status, JournalStatus::Applied);
    assert_eq!(stored.consensus_undo, undo(1));
    assert_eq!(stored.minting_outcome, outcome(btc_tip));
    assert_eq!(
        stored
            .rewards_changes
            .unwrap()
            .undo
            .rewards
            .values()
            .next()
            .unwrap()
            .as_ref()
            .unwrap()
            .amount,
        5
    );
}

#[tokio::test]
async fn preparation_requires_data_and_rejects_overwriting_saved_plans() {
    let initial = tip(100, 1);
    let btc_tip = tip(101, 2);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    journal
        .begin_apply(initial, &outcome(btc_tip), &undo(1))
        .await
        .unwrap();
    assert!(matches!(
        journal
            .store_flame_transition(btc_tip, &transition(1))
            .await,
        Err(Error::UnexpectedStatus { .. })
    ));
    assert!(matches!(
        journal.store_rewards_changes(btc_tip, &changes(10)).await,
        Err(Error::UnexpectedStatus { .. })
    ));
    advance_to_flame(&journal, btc_tip).await;
    assert_eq!(
        journal
            .complete_apply_stage(btc_tip, ApplyStage::PrepareFlameTransition)
            .await,
        Err(Error::MissingFlameTransition)
    );
    let mut flame = transition(1);
    journal
        .store_flame_transition(btc_tip, &flame)
        .await
        .unwrap();
    flame.next_tip = flame_tip(9);
    assert_eq!(
        journal.store_flame_transition(btc_tip, &flame).await,
        Err(Error::FlameTransitionAlreadyPrepared)
    );
    assert_eq!(
        journal
            .get_pending_entry()
            .await
            .unwrap()
            .unwrap()
            .flame_transition,
        Some(transition(1))
    );
    journal
        .complete_apply_stage(btc_tip, ApplyStage::PrepareFlameTransition)
        .await
        .unwrap();
    journal
        .complete_apply_stage(btc_tip, ApplyStage::ApplyFlameTransition)
        .await
        .unwrap();
    assert_eq!(
        journal
            .complete_apply_stage(btc_tip, ApplyStage::PrepareRewards)
            .await,
        Err(Error::MissingRewardsChanges)
    );
    let mut rewards = changes(10);
    journal
        .store_rewards_changes(btc_tip, &rewards)
        .await
        .unwrap();
    rewards.outcomes[0].rewards[0].amount = 99;
    rewards.undo.rewards.clear();
    assert_eq!(
        journal.store_rewards_changes(btc_tip, &rewards).await,
        Err(Error::RewardsChangesAlreadyPrepared)
    );
    let mut snapshot = journal
        .get_pending_entry()
        .await
        .unwrap()
        .unwrap()
        .rewards_changes
        .unwrap();
    snapshot.outcomes[0].rewards[0].amount = 99;
    snapshot.undo.rewards.clear();
    let saved = journal
        .get_pending_entry()
        .await
        .unwrap()
        .unwrap()
        .rewards_changes
        .unwrap();
    assert_eq!(saved.outcomes[0].rewards[0].amount, 10);
    assert_eq!(
        saved
            .undo
            .rewards
            .values()
            .next()
            .unwrap()
            .as_ref()
            .unwrap()
            .amount,
        5
    );
}

#[tokio::test]
async fn incomplete_operations_cannot_finish_or_start_another_operation() {
    let initial = tip(100, 1);
    let btc_tip = tip(101, 2);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    journal
        .begin_apply(initial, &outcome(btc_tip), &undo(1))
        .await
        .unwrap();
    assert!(matches!(
        journal
            .complete_apply_stage(btc_tip, ApplyStage::StoreCursor)
            .await,
        Err(Error::UnexpectedStatus { .. })
    ));
    assert_eq!(
        journal.begin_rollback(btc_tip).await,
        Err(Error::OperationInProgress(btc_tip))
    );
    finish_apply(&journal, btc_tip).await;
    journal
        .complete_apply_stage(btc_tip, ApplyStage::StoreCursor)
        .await
        .unwrap();
    journal.begin_rollback(btc_tip).await.unwrap();
    assert!(matches!(
        journal
            .complete_rollback_stage(btc_tip, RollbackStage::StoreCursor)
            .await,
        Err(Error::UnexpectedStatus { .. })
    ));
    assert_eq!(
        journal
            .begin_apply(btc_tip, &outcome(tip(102, 3)), &undo(2))
            .await,
        Err(Error::OperationInProgress(btc_tip))
    );
    assert_eq!(
        journal.get_pending_entry().await.unwrap().unwrap().status,
        JournalStatus::RollingBack(RollbackStage::RemoveEndorsements)
    );
    for stage in ROLLBACK_STAGES {
        journal
            .complete_rollback_stage(btc_tip, stage)
            .await
            .unwrap();
        journal
            .complete_rollback_stage(btc_tip, stage)
            .await
            .unwrap();
    }
    assert!(journal.get_pending_entry().await.unwrap().is_none());
}

#[tokio::test]
async fn history_rolls_back_from_the_tip_and_retains_both_branches_at_the_same_height() {
    let initial = tip(100, 1);
    let first = tip(101, 2);
    let second = tip(102, 3);
    let replacement = tip(101, 4);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    for (previous, next) in [(initial, first), (first, second)] {
        journal
            .begin_apply(previous, &outcome(next), &undo(1))
            .await
            .unwrap();
        finish_apply(&journal, next).await;
    }
    assert!(matches!(
        journal.begin_rollback(first).await,
        Err(Error::BlockTipMismatch { .. })
    ));
    assert!(journal.get_pending_entry().await.unwrap().is_none());
    finish_rollback(&journal, second).await;
    finish_rollback(&journal, first).await;
    journal
        .begin_apply(initial, &outcome(replacement), &undo(2))
        .await
        .unwrap();
    finish_apply(&journal, replacement).await;
    assert_eq!(
        journal
            .begin_apply(replacement, &outcome(second), &undo(2))
            .await,
        Err(Error::BlockTipMismatch {
            expected: first,
            actual: replacement
        })
    );
    for (btc_tip, expected) in [
        (first, JournalStatus::RolledBack),
        (second, JournalStatus::RolledBack),
        (replacement, JournalStatus::Applied),
    ] {
        assert_eq!(
            journal.get_entry(btc_tip).await.unwrap().unwrap().status,
            expected
        );
    }
}

#[tokio::test]
async fn an_empty_rewards_plan_is_prepared_and_can_be_applied_and_rolled_back() {
    let initial = tip(100, 1);
    let btc_tip = tip(101, 2);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    journal
        .begin_apply(initial, &outcome(btc_tip), &ConsensusUndo::default())
        .await
        .unwrap();
    advance_to_rewards(&journal, btc_tip).await;
    journal
        .store_rewards_changes(
            btc_tip,
            &RewardsChanges {
                outcomes: Vec::new(),
                undo: RewardsUndo::default(),
            },
        )
        .await
        .unwrap();
    for stage in [
        ApplyStage::PrepareRewards,
        ApplyStage::ApplyRewards,
        ApplyStage::StoreCursor,
    ] {
        journal.complete_apply_stage(btc_tip, stage).await.unwrap();
        journal.complete_apply_stage(btc_tip, stage).await.unwrap();
    }
    finish_rollback(&journal, btc_tip).await;
    let stored = journal.get_entry(btc_tip).await.unwrap().unwrap();
    assert_eq!(stored.status, JournalStatus::RolledBack);
    assert!(stored.rewards_changes.unwrap().outcomes.is_empty());
    assert!(journal.get_pending_entry().await.unwrap().is_none());
}

#[tokio::test]
async fn a_rolled_back_block_can_start_a_fresh_application_with_new_undo() {
    let initial = tip(100, 1);
    let btc_tip = tip(101, 2);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    journal
        .begin_apply(initial, &outcome(btc_tip), &undo(1))
        .await
        .unwrap();
    assert_eq!(
        journal
            .begin_apply(initial, &outcome(btc_tip), &undo(2))
            .await,
        Err(Error::OperationInProgress(btc_tip))
    );
    finish_apply(&journal, btc_tip).await;
    assert!(
        journal
            .begin_apply(initial, &outcome(btc_tip), &undo(2))
            .await
            .is_err()
    );
    assert_eq!(
        journal
            .get_entry(btc_tip)
            .await
            .unwrap()
            .unwrap()
            .consensus_undo,
        undo(1)
    );
    finish_rollback(&journal, btc_tip).await;
    journal
        .begin_apply(initial, &outcome(btc_tip), &undo(2))
        .await
        .unwrap();
    let entry = journal.get_pending_entry().await.unwrap().unwrap();
    assert_eq!(entry.consensus_undo, undo(2));
    assert!(entry.flame_transition.is_none());
    assert!(entry.rewards_changes.is_none());
    assert_eq!(
        entry.status,
        JournalStatus::Applying(ApplyStage::RemoveEndorsements)
    );
    finish_apply(&journal, btc_tip).await;
}

#[tokio::test]
async fn invalid_cursors_and_wrong_heights_do_not_change_the_journal() {
    let initial = tip(100, 1);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    for next in [tip(100, 2), tip(102, 2), tip(101, 1)] {
        assert!(matches!(
            journal.begin_apply(initial, &outcome(next), &undo(1)).await,
            Err(Error::InvalidSuccessor { .. })
        ));
    }
    assert!(matches!(
        journal
            .begin_apply(tip(100, 9), &outcome(tip(101, 2)), &undo(1))
            .await,
        Err(Error::BlockTipMismatch { .. })
    ));
    assert_eq!(
        journal.begin_rollback(initial).await,
        Err(Error::EntryNotFound(initial))
    );
    assert!(journal.get_pending_entry().await.unwrap().is_none());
    assert!(journal.state.read().unwrap().entries.is_empty());
    let btc_tip = tip(101, 2);
    journal
        .begin_apply(initial, &outcome(btc_tip), &undo(1))
        .await
        .unwrap();
    let wrong_height = tip(102, 2);
    assert!(matches!(
        journal.get_entry(wrong_height).await,
        Err(Error::BlockTipMismatch { .. })
    ));
    assert!(matches!(
        journal
            .complete_apply_stage(wrong_height, ApplyStage::RemoveEndorsements)
            .await,
        Err(Error::BlockTipMismatch { .. })
    ));
    assert_eq!(
        journal.get_pending_entry().await.unwrap().unwrap().status,
        JournalStatus::Applying(ApplyStage::RemoveEndorsements)
    );
    let initial = tip(u64::MAX, 1);
    let overflow = InMemoryBitcoinBlockJournal::new(initial);
    assert!(matches!(
        overflow
            .begin_apply(initial, &outcome(tip(0, 2)), &undo(1))
            .await,
        Err(Error::InvalidSuccessor { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_clones_can_start_only_one_operation() {
    let initial = tip(100, 1);
    let journal = InMemoryBitcoinBlockJournal::new(initial);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for hash in [2, 3] {
        let journal = journal.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            journal
                .begin_apply(initial, &outcome(tip(101, hash)), &undo(hash))
                .await
        }));
    }
    let mut succeeded = 0;
    let mut rejected = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(()) => succeeded += 1,
            Err(Error::OperationInProgress(_)) => rejected += 1,
            other => panic!("unexpected result: {other:?}"),
        }
    }
    assert_eq!((succeeded, rejected), (1, 1));
    assert_eq!(journal.state.read().unwrap().entries.len(), 1);
    assert!(journal.get_pending_entry().await.unwrap().is_some());
}
