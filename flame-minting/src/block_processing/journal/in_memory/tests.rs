use super::InMemoryConsensusJournalError as Error;
use crate::block_processing::BtcBlockRecords;
use crate::block_processing::journal::*;
use crate::consensus::{BitcoinRollbackPlan, ConsensusOutcome, ConsensusUndo, MintingOutcome};
use ::bitcoin::BlockHash;
use ::bitcoin::hashes::Hash;
use btc_integration::watcher::BlockId;
use flame_chain_service::ChainPath;
use flamechain::BlockTip;
use flamechain::{BlockHash as FlameBlockHash, CoreBlockTip};
use std::{collections::BTreeMap, sync::Arc};

use crate::{
    consensus::engine::tests::endorsement_for_tip,
    rewards::{RewardsOutcome, RewardsUndo},
};

use super::*;

fn btc_tip(height: u64, hash: u8) -> BlockId {
    BlockId {
        hash: BlockHash::from_byte_array([hash; 32]),
        height,
    }
}

fn flame_tip(hash: u8) -> BlockTip {
    BlockTip {
        hash: FlameBlockHash::new([hash; 32]),
        height: 1.into(),
    }
}

fn block(tip: BlockId) -> BtcBlockRecords {
    BtcBlockRecords {
        id: tip,
        burns: Vec::new(),
        endorsements: vec![endorsement_for_tip(
            1,
            CoreBlockTip {
                hash: flame_tip(1).hash,
                height: 1.into(),
            },
        )],
    }
}

fn outcome(tip: BlockId) -> MintingOutcome {
    MintingOutcome {
        next_btc_cursor: tip,
        consensus: ConsensusOutcome::default(),
    }
}

fn undo(hash: u8) -> ConsensusUndo {
    ConsensusUndo {
        weighted_blocks: BTreeMap::from([(flame_tip(hash), None)]),
        ..ConsensusUndo::default()
    }
}

fn transition() -> FlameTransition {
    FlameTransition {
        previous_tip: flame_tip(1),
        next_tip: flame_tip(2),
        path: ChainPath::default(),
        attached_core_headers: Vec::new(),
    }
}

fn rewards() -> RewardsChanges {
    RewardsChanges {
        outcomes: vec![RewardsOutcome {
            target_block_height: 1.into(),
            rewards: Vec::new(),
        }],
        undo: RewardsUndo::default(),
    }
}

async fn finish_bitcoin(journal: &InMemoryConsensusJournal, id: OperationId) {
    while let Some(snapshot) = journal.bitcoin_journal().pending_entry().await.unwrap() {
        assert_eq!(snapshot.id, id);
        let bitcoin = journal.bitcoin_journal();
        match snapshot.operation.entry.status {
            JournalStatus::Applying(stage) => {
                match stage {
                    ApplyStage::PrepareFlameTransition => {
                        bitcoin
                            .store_flame_transition(id, &transition())
                            .await
                            .unwrap();
                    }
                    ApplyStage::PrepareRewards => {
                        bitcoin.store_rewards_changes(id, &rewards()).await.unwrap();
                    }
                    _ => {}
                }
                bitcoin.complete_apply_stage(id, stage).await.unwrap();
                bitcoin.complete_apply_stage(id, stage).await.unwrap();
            }
            JournalStatus::RollingBack(stage) => {
                match stage {
                    RollbackStage::PrepareFlameTransition => bitcoin
                        .store_flame_transition(id, &transition())
                        .await
                        .unwrap(),
                    RollbackStage::PrepareRewards => {
                        bitcoin.store_rewards_changes(id, &rewards()).await.unwrap()
                    }
                    _ => {}
                }
                bitcoin.complete_rollback_stage(id, stage).await.unwrap();
                bitcoin.complete_rollback_stage(id, stage).await.unwrap();
            }
            status => panic!("unexpected pending status: {status:?}"),
        }
        let latest = journal.clone().latest_entry().await.unwrap().unwrap();
        assert_eq!(latest.id, id);
        assert_eq!(
            latest.is_complete(),
            journal.read().unwrap().pending.is_none()
        );
    }
}

async fn finish_flame(journal: &InMemoryConsensusJournal, id: OperationId) {
    while let Some(snapshot) = journal.flame_journal().pending_entry().await.unwrap() {
        assert_eq!(snapshot.id, id);
        let FlameJournalStatus::Applying(stage) = snapshot.operation.status else {
            panic!("expected a pending arrival");
        };
        let flame = journal.flame_journal();
        match stage {
            FlameApplyStage::PrepareTransition => {
                flame.store_transition(id, &transition()).await.unwrap();
            }
            FlameApplyStage::PrepareRewards => {
                flame.store_rewards(id, &rewards()).await.unwrap();
            }
            _ => {}
        }
        flame.complete_apply_stage(id, stage).await.unwrap();
        flame.complete_apply_stage(id, stage).await.unwrap();
        let latest = journal.clone().latest_entry().await.unwrap().unwrap();
        assert_eq!(latest.id, id);
        assert_eq!(
            latest.is_complete(),
            journal.read().unwrap().pending.is_none()
        );
    }
}

#[tokio::test]
async fn creating_views_does_not_create_an_operation() {
    let journal = InMemoryConsensusJournal::new(btc_tip(100, 1));
    let bitcoin = journal.bitcoin_journal();
    let flame = journal.flame_journal();
    assert!(journal.latest_entry().await.unwrap().is_none());
    assert!(bitcoin.pending_entry().await.unwrap().is_none());
    assert!(flame.pending_entry().await.unwrap().is_none());
    assert!(bitcoin.get_entry(btc_tip(101, 2)).await.unwrap().is_none());
}

#[tokio::test]
async fn bitcoin_flame_and_rollback_share_one_order_and_one_pending_operation() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    let bitcoin = journal.bitcoin_journal();
    let flame = journal.flame_journal();
    let apply = bitcoin
        .begin_apply(initial, &block(next), &outcome(next), &undo(1))
        .await
        .unwrap();
    assert_eq!(apply, OperationId(0));
    assert!(flame.pending_entry().await.unwrap().is_none());
    assert_eq!(
        flame
            .begin_apply(
                flame_tip(1),
                initial,
                &ConsensusOutcome::default(),
                &undo(1)
            )
            .await,
        Err(Error::OperationInProgress(apply))
    );
    finish_bitcoin(&journal, apply).await;
    let arrival = flame
        .begin_apply(flame_tip(1), next, &ConsensusOutcome::default(), &undo(2))
        .await
        .unwrap();
    assert_eq!(arrival, OperationId(1));
    assert!(bitcoin.pending_entry().await.unwrap().is_none());
    assert_eq!(
        bitcoin
            .begin_rollback(next, &BitcoinRollbackPlan::default())
            .await,
        Err(Error::OperationInProgress(arrival))
    );
    assert_eq!(
        bitcoin
            .begin_apply(
                next,
                &block(btc_tip(102, 3)),
                &outcome(btc_tip(102, 3)),
                &undo(3)
            )
            .await,
        Err(Error::OperationInProgress(arrival))
    );
    let latest = journal.latest_entry().await.unwrap().unwrap();
    assert_eq!(latest.btc_tip(), next);
    assert!(!latest.is_complete());
    assert!(matches!(latest.operation, ConsensusOperation::Flame(_)));
    finish_flame(&journal, arrival).await;
    let rollback = bitcoin
        .begin_rollback(next, &BitcoinRollbackPlan::default())
        .await
        .unwrap();
    assert_eq!(rollback, OperationId(2));
    finish_bitcoin(&journal, rollback).await;
    let state = journal.read().unwrap();
    assert_eq!(state.applied_btc_tip, initial);
    assert_eq!(state.entries.len(), 3);
    assert!(
        state
            .entries
            .values()
            .all(ConsensusJournalEntry::is_complete)
    );
    let ConsensusOperation::Bitcoin(original) = &state.entries[&apply].operation else {
        panic!()
    };
    assert_eq!(original.kind, BitcoinOperationKind::Apply);
    assert_eq!(original.entry.status, JournalStatus::Applied);
    let ConsensusOperation::Flame(original) = &state.entries[&arrival].operation else {
        panic!()
    };
    assert_eq!(original.status, FlameJournalStatus::Applied);
}

#[tokio::test]
async fn saved_bitcoin_input_and_undo_survive_rollback_and_reapplication() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    let bitcoin = journal.bitcoin_journal();
    let mut input = block(next);
    let mut original_outcome = outcome(next);
    let mut original_undo = undo(1);
    let apply = bitcoin
        .begin_apply(initial, &input, &original_outcome, &original_undo)
        .await
        .unwrap();
    input.endorsements.clear();
    original_outcome.next_btc_cursor = initial;
    original_undo.weighted_blocks.clear();
    let mut snapshot = bitcoin.pending_entry().await.unwrap().unwrap();
    snapshot.operation.block.endorsements.clear();
    snapshot
        .operation
        .entry
        .consensus_undo
        .weighted_blocks
        .clear();
    let stored = bitcoin.get_entry(next).await.unwrap().unwrap();
    assert_eq!(stored.operation.block, block(next));
    assert!(
        stored
            .operation
            .entry
            .minting_outcome
            .consensus
            .accepted_endorsements
            .is_empty()
    );
    assert_eq!(stored.operation.entry.consensus_undo, undo(1));
    assert_eq!(stored.operation.entry.minting_outcome, outcome(next));
    finish_bitcoin(&journal, apply).await;
    let rollback = bitcoin
        .begin_rollback(next, &BitcoinRollbackPlan::default())
        .await
        .unwrap();
    let stored = bitcoin.pending_entry().await.unwrap().unwrap();
    assert_eq!(stored.operation.kind, BitcoinOperationKind::Rollback);
    assert_eq!(stored.operation.block, block(next));
    assert_eq!(stored.operation.entry.consensus_undo, undo(1));
    assert_eq!(stored.operation.entry.flame_transition, None);
    finish_bitcoin(&journal, rollback).await;
    assert_eq!(
        bitcoin
            .get_entry(next)
            .await
            .unwrap()
            .unwrap()
            .operation
            .entry
            .status,
        JournalStatus::RolledBack
    );
    let reapplied = bitcoin
        .begin_apply(initial, &block(next), &outcome(next), &undo(2))
        .await
        .unwrap();
    assert_eq!(reapplied, OperationId(2));
    assert_eq!(
        bitcoin
            .pending_entry()
            .await
            .unwrap()
            .unwrap()
            .operation
            .entry
            .consensus_undo,
        undo(2)
    );
    let state = journal.read().unwrap();
    let ConsensusOperation::Bitcoin(original) = &state.entries[&apply].operation else {
        panic!()
    };
    assert_eq!(original.entry.consensus_undo, undo(1));
    assert_eq!(original.entry.status, JournalStatus::Applied);
    assert_eq!(state.entries.len(), 3);
}

#[tokio::test]
async fn bitcoin_plans_must_be_saved_before_advancing_and_cannot_be_overwritten() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    let bitcoin = journal.bitcoin_journal();
    let id = bitcoin
        .begin_apply(initial, &block(next), &outcome(next), &undo(1))
        .await
        .unwrap();
    assert!(matches!(
        bitcoin
            .complete_apply_stage(id, ApplyStage::StoreCursor)
            .await,
        Err(Error::UnexpectedBitcoinStatus { .. })
    ));
    assert!(matches!(
        bitcoin.store_flame_transition(id, &transition()).await,
        Err(Error::UnexpectedBitcoinStatus { .. })
    ));
    for stage in [
        ApplyStage::RemoveEndorsements,
        ApplyStage::StoreEndorsements,
        ApplyStage::StoreAcquisitions,
        ApplyStage::StoreBlockWeights,
        ApplyStage::StoreDoubleSigns,
        ApplyStage::StorePendingEndorsements,
    ] {
        bitcoin.complete_apply_stage(id, stage).await.unwrap();
    }
    assert_eq!(
        bitcoin
            .complete_apply_stage(id, ApplyStage::PrepareFlameTransition)
            .await,
        Err(Error::MissingTransition)
    );
    bitcoin
        .store_flame_transition(id, &transition())
        .await
        .unwrap();
    assert_eq!(
        bitcoin.store_flame_transition(id, &transition()).await,
        Err(Error::PlanAlreadyStored)
    );
    bitcoin
        .complete_apply_stage(id, ApplyStage::PrepareFlameTransition)
        .await
        .unwrap();
    bitcoin
        .complete_apply_stage(id, ApplyStage::ApplyFlameTransition)
        .await
        .unwrap();
    assert_eq!(
        bitcoin
            .complete_apply_stage(id, ApplyStage::PrepareRewards)
            .await,
        Err(Error::MissingRewards)
    );
    let mut plan = rewards();
    bitcoin.store_rewards_changes(id, &plan).await.unwrap();
    plan.outcomes.clear();
    assert_eq!(
        bitcoin.store_rewards_changes(id, &plan).await,
        Err(Error::PlanAlreadyStored)
    );
    bitcoin
        .complete_apply_stage(id, ApplyStage::PrepareRewards)
        .await
        .unwrap();
    let stored = bitcoin
        .pending_entry()
        .await
        .unwrap()
        .unwrap()
        .operation
        .entry;
    assert_eq!(stored.flame_transition, Some(transition()));
    assert_eq!(stored.rewards_changes.unwrap().outcomes.len(), 1);
    finish_bitcoin(&journal, id).await;
}

#[tokio::test]
async fn flame_plans_and_snapshots_keep_recovery_data_without_changing_the_btc_cursor() {
    let initial = btc_tip(100, 1);
    let journal = InMemoryConsensusJournal::new(initial);
    let flame = journal.flame_journal();
    let mut original_undo = undo(1);
    let id = flame
        .begin_apply(
            flame_tip(2),
            initial,
            &ConsensusOutcome::default(),
            &original_undo,
        )
        .await
        .unwrap();
    original_undo.weighted_blocks.clear();
    assert!(matches!(
        flame
            .complete_apply_stage(id, FlameApplyStage::RemovePending)
            .await,
        Err(Error::UnexpectedFlameStatus { .. })
    ));
    flame
        .complete_apply_stage(id, FlameApplyStage::Consensus)
        .await
        .unwrap();
    assert_eq!(
        flame
            .complete_apply_stage(id, FlameApplyStage::PrepareTransition)
            .await,
        Err(Error::MissingTransition)
    );
    let mut plan = transition();
    flame.store_transition(id, &plan).await.unwrap();
    plan.next_tip = flame_tip(3);
    assert_eq!(
        flame.store_transition(id, &plan).await,
        Err(Error::PlanAlreadyStored)
    );
    flame
        .complete_apply_stage(id, FlameApplyStage::PrepareTransition)
        .await
        .unwrap();
    flame
        .complete_apply_stage(id, FlameApplyStage::ApplyTransition)
        .await
        .unwrap();
    assert_eq!(
        flame
            .complete_apply_stage(id, FlameApplyStage::PrepareRewards)
            .await,
        Err(Error::MissingRewards)
    );
    flame.store_rewards(id, &rewards()).await.unwrap();
    assert_eq!(
        flame.store_rewards(id, &rewards()).await,
        Err(Error::PlanAlreadyStored)
    );
    flame
        .complete_apply_stage(id, FlameApplyStage::PrepareRewards)
        .await
        .unwrap();
    let mut snapshot = flame.pending_entry().await.unwrap().unwrap();
    snapshot.operation.undo.weighted_blocks.clear();
    snapshot
        .operation
        .rewards
        .as_mut()
        .unwrap()
        .outcomes
        .clear();
    let stored = journal
        .clone()
        .flame_journal()
        .pending_entry()
        .await
        .unwrap()
        .unwrap()
        .operation;
    assert_eq!(stored.tip, flame_tip(2));
    assert_eq!(stored.undo, undo(1));
    assert_eq!(stored.transition, Some(transition()));
    assert_eq!(stored.rewards.unwrap().outcomes.len(), 1);
    finish_flame(&journal, id).await;
    assert_eq!(journal.read().unwrap().applied_btc_tip, initial);
}

#[tokio::test]
async fn operation_ids_reject_the_wrong_view_and_stale_writes() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    let bitcoin = journal.bitcoin_journal();
    let flame = journal.flame_journal();
    let apply = bitcoin
        .begin_apply(initial, &block(next), &outcome(next), &undo(1))
        .await
        .unwrap();
    assert_eq!(
        flame
            .complete_apply_stage(apply, FlameApplyStage::Consensus)
            .await,
        Err(Error::WrongOperationKind(apply))
    );
    assert_eq!(
        flame.store_transition(apply, &transition()).await,
        Err(Error::WrongOperationKind(apply))
    );
    assert_eq!(
        bitcoin
            .complete_apply_stage(OperationId(99), ApplyStage::RemoveEndorsements)
            .await,
        Err(Error::MissingEntry(OperationId(99)))
    );
    finish_bitcoin(&journal, apply).await;
    let arrival = flame
        .begin_apply(flame_tip(1), next, &ConsensusOutcome::default(), &undo(2))
        .await
        .unwrap();
    assert_eq!(
        bitcoin.store_rewards_changes(apply, &rewards()).await,
        Err(Error::NotPending(apply))
    );
    assert_eq!(
        bitcoin
            .complete_apply_stage(arrival, ApplyStage::StoreCursor)
            .await,
        Err(Error::WrongOperationKind(arrival))
    );
    finish_flame(&journal, arrival).await;
    let rollback = bitcoin
        .begin_rollback(next, &BitcoinRollbackPlan::default())
        .await
        .unwrap();
    assert_eq!(
        bitcoin
            .complete_rollback_stage(apply, RollbackStage::RemoveEndorsements)
            .await,
        Err(Error::NotPending(apply))
    );
    assert_eq!(
        flame.store_transition(arrival, &transition()).await,
        Err(Error::NotPending(arrival))
    );
    assert_eq!(journal.latest_entry().await.unwrap().unwrap().id, rollback);
}

#[tokio::test]
async fn invalid_btc_inputs_and_flame_context_do_not_create_operations() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    let bitcoin = journal.bitcoin_journal();
    for next in [btc_tip(100, 2), btc_tip(102, 2), btc_tip(101, 1)] {
        assert!(matches!(
            bitcoin
                .begin_apply(initial, &block(next), &outcome(next), &undo(1))
                .await,
            Err(Error::InvalidSuccessor { .. })
        ));
    }
    assert!(matches!(
        bitcoin
            .begin_apply(initial, &block(next), &outcome(btc_tip(101, 3)), &undo(1))
            .await,
        Err(Error::BlockTipMismatch { .. })
    ));
    assert!(matches!(
        journal
            .flame_journal()
            .begin_apply(flame_tip(1), next, &ConsensusOutcome::default(), &undo(1))
            .await,
        Err(Error::BlockTipMismatch { .. })
    ));
    assert_eq!(
        bitcoin
            .begin_rollback(initial, &BitcoinRollbackPlan::default())
            .await,
        Err(Error::BitcoinEntryNotFound(initial))
    );
    assert!(journal.latest_entry().await.unwrap().is_none());
    let id = bitcoin
        .begin_apply(initial, &block(next), &outcome(next), &undo(1))
        .await
        .unwrap();
    assert_eq!(id, OperationId(0));
    assert!(matches!(
        bitcoin.get_entry(btc_tip(102, 2)).await,
        Err(Error::BlockTipMismatch { .. })
    ));
}

#[tokio::test]
async fn rollback_uses_btc_history_even_when_the_latest_operation_is_a_flame_arrival() {
    let initial = btc_tip(100, 1);
    let first = btc_tip(101, 2);
    let second = btc_tip(102, 3);
    let replacement = btc_tip(101, 4);
    let journal = InMemoryConsensusJournal::new(initial);
    let bitcoin = journal.bitcoin_journal();
    for (previous, next) in [(initial, first), (first, second)] {
        let id = bitcoin
            .begin_apply(previous, &block(next), &outcome(next), &undo(1))
            .await
            .unwrap();
        finish_bitcoin(&journal, id).await;
    }
    let arrival = journal
        .flame_journal()
        .begin_apply(flame_tip(1), second, &ConsensusOutcome::default(), &undo(1))
        .await
        .unwrap();
    finish_flame(&journal, arrival).await;
    assert!(matches!(
        bitcoin
            .begin_rollback(first, &BitcoinRollbackPlan::default())
            .await,
        Err(Error::BlockTipMismatch { .. })
    ));
    for tip in [second, first] {
        let id = bitcoin
            .begin_rollback(tip, &BitcoinRollbackPlan::default())
            .await
            .unwrap();
        finish_bitcoin(&journal, id).await;
    }
    let id = bitcoin
        .begin_apply(
            initial,
            &block(replacement),
            &outcome(replacement),
            &undo(2),
        )
        .await
        .unwrap();
    finish_bitcoin(&journal, id).await;
    for (tip, status) in [
        (first, JournalStatus::RolledBack),
        (second, JournalStatus::RolledBack),
        (replacement, JournalStatus::Applied),
    ] {
        assert_eq!(
            bitcoin
                .get_entry(tip)
                .await
                .unwrap()
                .unwrap()
                .operation
                .entry
                .status,
            status
        );
    }
    let state = journal.read().unwrap();
    assert_eq!(state.applied_btc_tip, replacement);
    let ConsensusOperation::Flame(entry) = &state.entries[&arrival].operation else {
        panic!()
    };
    assert_eq!(entry.status, FlameJournalStatus::Applied);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_bitcoin_and_flame_views_can_start_only_one_operation() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let bitcoin = journal.bitcoin_journal();
    let flame = journal.flame_journal();
    let bitcoin_barrier = barrier.clone();
    let bitcoin_task = tokio::spawn(async move {
        bitcoin_barrier.wait().await;
        bitcoin
            .begin_apply(initial, &block(next), &outcome(next), &undo(1))
            .await
    });
    let flame_task = tokio::spawn(async move {
        barrier.wait().await;
        flame
            .begin_apply(
                flame_tip(1),
                initial,
                &ConsensusOutcome::default(),
                &undo(2),
            )
            .await
    });
    let results = [bitcoin_task.await.unwrap(), flame_task.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(Error::OperationInProgress(OperationId(0))))
            .count(),
        1
    );
    assert_eq!(journal.read().unwrap().entries.len(), 1);
}

#[tokio::test]
async fn id_overflow_preserves_history_and_the_btc_index() {
    let initial = btc_tip(100, 1);
    let next = btc_tip(101, 2);
    let journal = InMemoryConsensusJournal::new(initial);
    journal.write().unwrap().next_id = u64::MAX;
    assert_eq!(
        journal
            .bitcoin_journal()
            .begin_apply(initial, &block(next), &outcome(next), &undo(1))
            .await,
        Err(Error::IdOverflow)
    );
    assert_eq!(
        journal
            .flame_journal()
            .begin_apply(
                flame_tip(1),
                initial,
                &ConsensusOutcome::default(),
                &undo(1)
            )
            .await,
        Err(Error::IdOverflow)
    );
    let state = journal.read().unwrap();
    assert!(state.entries.is_empty());
    assert!(state.bitcoin_entries.is_empty());
    assert!(state.pending.is_none());
    assert_eq!(state.applied_btc_tip, initial);
}
