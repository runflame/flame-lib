use flamechain::CoreFlameHeight;

use super::PendingReward;

pub struct RewardsOutcome {
    pub target_block_height: CoreFlameHeight,
    pub rewards: Vec<PendingReward>,
}
