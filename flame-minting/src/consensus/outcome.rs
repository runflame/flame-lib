use std::collections::BTreeMap;

use btc_integration::{Acquisition, AuthenticatedMintingVote, BtcBlockTip, MinterP2wsh};
use flamechain::{BlockHash, BlockTip, CoreFlameHeight};

/// Outcome for 1 new BTC block
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintingOutcome {
    /// New valid votes.
    pub accepted_votes: Vec<WeightedVote>,
    /// New valid acquisitions.
    pub accepted_acquisitions: Vec<IncludedAcquisition>,
    /// Double votes.
    pub double_signs: Vec<DoubleSign>,
    /// Votes for the blocks that node doesn't yet have.
    pub pending_votes: BTreeMap<BlockTip, PendingVotes>,
    /// The processed cursor.
    pub next_btc_cursor: BtcBlockTip,
    /// Canonical Flame tip against which this plan was calculated.
    pub expected_canonical_tip: Option<BlockTip>,
    pub selected_tip: Option<BlockTip>,
    /// Old tip first, excluding the common ancestor.
    pub detach: Vec<BlockTip>,
    /// New tip first, excluding the common ancestor.
    pub attach: Vec<BlockTip>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncludedVote {
    pub btc_block: BtcBlockTip,
    pub vote: AuthenticatedMintingVote,
}

impl IncludedVote {
    pub fn block_height(&self) -> u32 {
        self.vote.output().data.block_height()
    }

    pub fn block_hash(&self) -> BlockHash {
        self.vote.output().data.block_hash()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightedVote {
    pub original: IncludedVote,
    pub effective_minting_power: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncludedAcquisition {
    pub btc_block: BtcBlockTip,
    pub acquisition: Acquisition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoubleSign {
    pub minter: MinterP2wsh,
    pub target_flame_height: CoreFlameHeight,
    pub votes: Vec<IncludedVote>,
}

pub type PendingVotes = Vec<IncludedVote>;
