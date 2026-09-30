use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Serialize};

use super::{BitcoinTxid, FlameBlockHash, MinterId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBitcoinBlocksRequest {
    pub count: NonZeroU32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateFlameBlockRequest {
    pub parent_hash: FlameBlockHash,
    pub target_btc_height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMinterRequest {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendAcquisitionRequest {
    pub minter_id: MinterId,
    pub amount_sats: NonZeroU64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendVoteRequest {
    pub minter_id: MinterId,
    pub core_height: u32,
    pub block_hash: FlameBlockHash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmittedTransaction {
    pub txid: BitcoinTxid,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetRequest {}
