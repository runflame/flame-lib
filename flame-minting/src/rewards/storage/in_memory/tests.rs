use flamevm::{Predicate, Scalar};

use crate::rewards::{RewardsApplier, RewardsOutcome};

use super::*;

fn predicate(tag: u64) -> Predicate {
    let point = Predicate::unspendable_key().decompress().unwrap();
    Predicate::opaque((point * Scalar::from(tag).to_dalek()).compress())
}

fn reward(flame: u64, access: u64, amount: u64) -> PendingReward {
    PendingReward {
        flame_predicate: predicate(flame),
        access_predicate: predicate(access),
        amount,
    }
}

fn contents(rewards: &[PendingReward]) -> Vec<([u8; 32], [u8; 32], u64)> {
    rewards
        .iter()
        .map(|reward| {
            (
                reward.flame_predicate.to_point().to_bytes(),
                reward.access_predicate.to_point().to_bytes(),
                reward.amount,
            )
        })
        .collect()
}

#[tokio::test]
async fn replaying_an_outcome_preserves_amounts_without_duplicates() {
    let storage = InMemoryRewardsStorage::new();
    let applier = RewardsApplier {
        rewards_storage: storage.clone(),
    };
    let outcome = RewardsOutcome {
        target_block_height: 200.into(),
        rewards: vec![reward(1, 2, u64::MAX), reward(1, 3, 0)],
    };
    applier.apply(&outcome).await.unwrap();
    applier.apply(&outcome).await.unwrap();
    let stored = storage.get_pending_rewards(200.into()).await.unwrap();
    let mut expected = contents(&outcome.rewards);
    expected.sort();
    assert_eq!(contents(&stored), expected);
}

#[tokio::test]
async fn reads_separate_heights_and_both_predicates_and_return_independent_snapshots() {
    let storage = InMemoryRewardsStorage::new();
    assert!(
        storage
            .get_pending_rewards(200.into())
            .await
            .unwrap()
            .is_empty()
    );
    let rewards = vec![reward(2, 1, 20), reward(1, 2, 30), reward(1, 1, 10)];
    for reward in &rewards {
        storage
            .store_pending_reward(200.into(), reward)
            .await
            .unwrap();
    }
    storage
        .store_pending_reward(300.into(), &reward(1, 1, 40))
        .await
        .unwrap();
    let mut expected = contents(&rewards);
    expected.sort();
    let mut snapshot = storage.get_pending_rewards(200.into()).await.unwrap();
    assert_eq!(contents(&snapshot), expected);
    snapshot[0].amount = 999;
    snapshot[0].flame_predicate = predicate(9);
    assert_eq!(
        contents(&storage.get_pending_rewards(200.into()).await.unwrap()),
        expected
    );
    assert_eq!(
        contents(&storage.get_pending_rewards(300.into()).await.unwrap()),
        contents(&[reward(1, 1, 40)])
    );
}

#[tokio::test]
async fn conflicting_amount_does_not_overwrite_a_stored_reward() {
    let storage = InMemoryRewardsStorage::new();
    storage
        .store_pending_reward(200.into(), &reward(1, 2, 10))
        .await
        .unwrap();
    assert_eq!(
        storage
            .clone()
            .store_pending_reward(200.into(), &reward(1, 2, 20))
            .await,
        Err(InMemoryRewardsStorageError::ConflictingReward {
            target_block_height: 200.into(),
            stored_amount: 10,
            requested_amount: 20,
        })
    );
    assert_eq!(
        contents(&storage.get_pending_rewards(200.into()).await.unwrap()),
        contents(&[reward(1, 2, 10)])
    );
}
