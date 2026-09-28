use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    sync::{Arc, RwLock},
};

use flamechain::CoreFlameHeight;

use super::{PendingReward, RewardsStorage};

type RewardsByPredicate = BTreeMap<([u8; 32], [u8; 32]), PendingReward>;

#[derive(Clone, Default)]
pub struct InMemoryRewardsStorage {
    rewards: Arc<RwLock<BTreeMap<CoreFlameHeight, RewardsByPredicate>>>,
}

impl InMemoryRewardsStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_pending_rewards(
        &self,
        target_block_height: CoreFlameHeight,
    ) -> Result<Vec<PendingReward>, InMemoryRewardsStorageError> {
        let rewards = self
            .rewards
            .read()
            .map_err(|_| InMemoryRewardsStorageError::LockPoisoned)?;
        Ok(rewards
            .get(&target_block_height)
            .into_iter()
            .flat_map(|rewards| rewards.values())
            .map(clone_reward)
            .collect())
    }
}

impl RewardsStorage for InMemoryRewardsStorage {
    type Error = InMemoryRewardsStorageError;

    async fn store_pending_reward(
        &self,
        target_block_height: CoreFlameHeight,
        reward: &PendingReward,
    ) -> Result<(), Self::Error> {
        let key = (
            reward.flame_predicate.to_point().to_bytes(),
            reward.access_predicate.to_point().to_bytes(),
        );
        let mut rewards = self
            .rewards
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        match rewards.entry(target_block_height).or_default().entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(clone_reward(reward));
                Ok(())
            }
            Entry::Occupied(entry) if entry.get().amount == reward.amount => Ok(()),
            Entry::Occupied(entry) => Err(Self::Error::ConflictingReward {
                target_block_height,
                stored_amount: entry.get().amount,
                requested_amount: reward.amount,
            }),
        }
    }
}

fn clone_reward(reward: &PendingReward) -> PendingReward {
    PendingReward {
        flame_predicate: reward.flame_predicate.clone(),
        access_predicate: reward.access_predicate.clone(),
        amount: reward.amount,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryRewardsStorageError {
    LockPoisoned,
    ConflictingReward {
        target_block_height: CoreFlameHeight,
        stored_amount: u64,
        requested_amount: u64,
    },
}

impl fmt::Display for InMemoryRewardsStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "rewards storage lock poisoned"),
            Self::ConflictingReward {
                target_block_height,
                stored_amount,
                requested_amount,
            } => write!(
                formatter,
                "conflicting reward at core height {target_block_height:?}: stored amount {stored_amount}, requested {requested_amount}"
            ),
        }
    }
}

impl std::error::Error for InMemoryRewardsStorageError {}

#[cfg(test)]
mod tests;
