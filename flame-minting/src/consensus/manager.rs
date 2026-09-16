use flame_storage::ChainStorage;

use crate::rewards::RewardsStorage;

use super::{ConsensusStorage, MintingEngine};

pub struct MintingManager<C: ConsensusStorage, R: RewardsStorage, H: ChainStorage> {
    pub consensus_engine: MintingEngine,
    pub consensus_storage: C,
    pub rewards_storage: R,
    pub chain_storage: H,
}
