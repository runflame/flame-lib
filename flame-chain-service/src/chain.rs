pub mod in_memory;

pub use in_memory::{InMemoryChain, InMemoryChainError};

use flame_storage::ChainStorage;
use flamechain::{BlockHash, BlockTip, FlameBlockValidator};

pub struct ChainManager<S, C: ChainStorage, V: FlameBlockValidator> {
    pub state_storage: S,
    pub chain_storage: C,
    pub block_validator: V,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported(BlockHash),
    AlreadyKnown(BlockHash),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainPath {
    pub detach: Vec<BlockTip>,
    pub attach: Vec<BlockTip>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangesOutcome {
    pub detached: Vec<BlockHash>,
    pub attached: Vec<BlockHash>,
}
