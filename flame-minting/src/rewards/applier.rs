use super::{RewardsOutcome, RewardsStorage};

pub struct RewardsApplier<S: RewardsStorage> {
    pub rewards_storage: S,
}

impl<S: RewardsStorage> RewardsApplier<S> {
    pub async fn apply(&self, outcome: &RewardsOutcome) -> Result<(), S::Error> {
        for reward in &outcome.rewards {
            self.rewards_storage
                .store_pending_reward(outcome.target_block_height, reward)
                .await?;
        }
        Ok(())
    }
}
