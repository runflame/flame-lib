use std::future::Future;

use flamechain::CoreFlameHeight;

use super::PendingReward;

pub trait RewardsStorage {
    type Error;

    fn store_pending_reward(
        &self,
        target_block_height: CoreFlameHeight,
        reward: &PendingReward,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
