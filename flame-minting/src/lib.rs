pub mod block_processing;
pub mod consensus;
pub mod minter;
pub mod orchestrator;
pub mod rewards;

pub use block_processing::{
    BtcBlockRecords, CONSENSUS_QUEUE_CAPACITY, ConsensusHandle, ConsensusSendError,
    ConsensusStatus, FlameBlockArrivalHandler,
};
pub use orchestrator::MintingOrchestrator;

pub mod core_block_notifier {
    pub use crate::orchestrator::core_block_notifier::*;
}
