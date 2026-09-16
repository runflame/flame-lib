use std::future::Future;

use super::PendingReward;

pub trait RewardsStorage {
    type Error;

    fn store_pending_reward(
        &self,
        reward: &PendingReward,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
