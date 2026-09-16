use btc_integration::{Acquisition, AuthenticatedMintingVote};
use flamechain::BlockHash;

pub struct PendingReward {
    pub core_block: BlockHash,
    pub core_block_height: u64,
    pub acquisition: Acquisition,
    pub vote: AuthenticatedMintingVote,
    pub amount_sparks: u64,
}
