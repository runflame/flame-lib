use std::fmt;

pub mod block;
pub mod codec;
pub mod mempool;
pub mod storage;
pub mod utreexo;

pub use block::{
    AppliedBlock, Block, BlockHeader, BlockLimits, BlockTx, Blockchain, ChainError, ChainParams,
    ContractLeaf, ExecutionKind, ExecutionRecord, ReorgOutcome, StateCommitment,
};
pub use mempool::{Mempool, MempoolEntry, MempoolError, MempoolPolicy, RebaseReport};
pub use storage::{ActorInfo, Lease, SPARKS_PER_FLAME, StorageError, StorageParams, StoredActor};
pub use utreexo::utreexo_hasher;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FlameNetwork {
    Regtest = 0,
    Testnet = 1,
    Mainnet = 2,
}

impl FlameNetwork {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Regtest),
            1 => Some(Self::Testnet),
            2 => Some(Self::Mainnet),
            _ => None,
        }
    }
}

impl From<FlameNetwork> for u8 {
    fn from(network: FlameNetwork) -> Self {
        network.as_u8()
    }
}

impl TryFrom<u8> for FlameNetwork {
    type Error = InvalidFlameNetwork;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_u8(value).ok_or(InvalidFlameNetwork(value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidFlameNetwork(pub u8);

impl fmt::Display for InvalidFlameNetwork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown Flame network id {}", self.0)
    }
}

impl std::error::Error for InvalidFlameNetwork {}

/// The hash of a Flame block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockHash([u8; 32]);

impl BlockHash {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for BlockHash {
    fn from(bytes: [u8; 32]) -> Self {
        Self::new(bytes)
    }
}

impl From<BlockHash> for [u8; 32] {
    fn from(block_hash: BlockHash) -> Self {
        block_hash.into_bytes()
    }
}

impl AsRef<[u8; 32]> for BlockHash {
    fn as_ref(&self) -> &[u8; 32] {
        self.as_bytes()
    }
}

impl AsRef<[u8]> for BlockHash {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
