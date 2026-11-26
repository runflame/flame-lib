pub mod bitcoin;
pub mod canonical_chain;
mod consensus_handle;
mod consensus_manager;
pub mod flame_arrival;
pub mod journal;

pub use crate::rewards::application::rewards_changes::PrepareRewardsError;
pub use bitcoin::{BitcoinBlockProcessor, BlockProcessingError, BlockProcessor, BtcBlockRecords};
pub use canonical_chain::{ChainPathApplierError, PrepareFlameError};
pub use consensus_handle::ConsensusHandle;
pub use consensus_manager::{
    CONSENSUS_QUEUE_CAPACITY, ConsensusManager, ConsensusManagerError, ConsensusSendError,
    ConsensusStatus,
};
pub use flame_arrival::{
    FlameBlockArrivalHandler, FlameBlockProcessingError, FlameBlockProcessor, FlameProcessor,
};

#[cfg(test)]
pub(crate) mod tests;
