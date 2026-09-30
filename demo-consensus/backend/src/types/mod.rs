mod requests;
mod snapshot;

pub use requests::*;
pub use snapshot::*;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitcoinBlockHash(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FlameBlockHash(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitcoinTxid(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MinterId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitcoinBlockTip {
    pub hash: BitcoinBlockHash,
    pub height: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlameBlockTip {
    pub hash: FlameBlockHash,
    pub height: u64,
}
