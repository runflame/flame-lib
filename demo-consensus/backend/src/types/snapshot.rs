use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};

use super::{
    BitcoinBlockHash, BitcoinBlockTip, BitcoinTxid, FlameBlockHash, FlameBlockTip, MinterId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoSnapshot {
    pub bitcoin: BitcoinChainSnapshot,
    pub flame: FlameChainSnapshot,
    pub minters: Vec<MinterSnapshot>,
    pub acquisitions: Vec<AcquisitionSnapshot>,
    pub votes: Vec<VoteSnapshot>,
    pub consensus: ConsensusSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitcoinChainSnapshot {
    pub start_height: u64,
    pub tip: BitcoinBlockTip,
    pub blocks: Vec<BitcoinBlockSnapshot>,
    pub mempool: Vec<BitcoinTxid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitcoinBlockSnapshot {
    pub tip: BitcoinBlockTip,
    pub parent_hash: Option<BitcoinBlockHash>,
    pub transactions: Vec<BitcoinTxid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlameChainSnapshot {
    pub canonical_tip: Option<FlameBlockTip>,
    pub blocks: Vec<FlameBlockSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlameBlockSnapshot {
    pub tip: FlameBlockTip,
    pub parent_hash: Option<FlameBlockHash>,
    pub core: Option<CoreBlockSnapshot>,
    pub is_canonical: bool,
    pub parent_weight: u64,
    pub effective_power: u64,
    pub block_weight: u32,
    pub chain_weight: u128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreBlockSnapshot {
    pub height: u32,
    pub target_btc_height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinterSnapshot {
    pub id: MinterId,
    pub name: String,
    pub p2wsh_address: String,
    pub automatic_voting: bool,
    pub is_double_signed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcquisitionSnapshot {
    pub txid: BitcoinTxid,
    pub output_index: u32,
    pub minter_id: MinterId,
    pub amount_sats: u64,
    pub duration_blocks: u16,
    pub transaction_status: TransactionStatus,
    pub processing_status: AcquisitionProcessingStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteSnapshot {
    pub txid: BitcoinTxid,
    pub output_index: u32,
    pub minter_id: MinterId,
    pub core_height: u32,
    pub block_hash: FlameBlockHash,
    pub transaction_status: TransactionStatus,
    pub processing_status: VoteProcessingStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TransactionStatus {
    Mempool,
    Confirmed { block: BitcoinBlockTip },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AcquisitionProcessingStatus {
    Unprocessed,
    Accepted {
        activates_at_btc_height: u64,
        expires_at_btc_height_exclusive: u64,
    },
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VoteProcessingStatus {
    Unprocessed,
    PendingBlock,
    Accepted { effective_minting_power: u64 },
    InvalidatedByDoubleSign,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusSnapshot {
    pub btc_cursor: BitcoinBlockTip,
    pub heaviest_tip: Option<FlameBlockTip>,
    pub parameters: ProtocolParameters,
    pub double_signs: Vec<DoubleSignSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolParameters {
    pub acquisition_maturity: u32,
    pub default_acquisition_duration: NonZeroU16,
    pub min_acquisition_duration: NonZeroU16,
    pub max_vote_delay: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoubleSignSnapshot {
    pub minter_id: MinterId,
    pub core_height: u32,
    pub votes: Vec<VoteSnapshot>,
}
