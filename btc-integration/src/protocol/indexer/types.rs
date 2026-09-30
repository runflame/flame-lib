use crate::{
    btc::rpc::BtcBlockTip,
    protocol::{Acquisition, AuthenticatedMintingVote},
};

pub(super) const MAX_HISTORY_BLOCKS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedBlock {
    pub btc_block_tip: BtcBlockTip,
    pub acquisitions: Vec<Acquisition>,
    pub votes: Vec<AuthenticatedMintingVote>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryUpdate {
    pub target_tip: BtcBlockTip,
    pub change: HistoryChange,
    pub next_cursor: BtcBlockTip,
}

impl HistoryUpdate {
    pub fn get_indexed_blocks(&self) -> &[IndexedBlock] {
        match &self.change {
            HistoryChange::Extension { new_blocks } | HistoryChange::Reorg { new_blocks, .. } => {
                new_blocks
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryChange {
    Extension {
        new_blocks: Vec<IndexedBlock>,
    },
    Reorg {
        removed_block_tips: Vec<BtcBlockTip>,
        new_blocks: Vec<IndexedBlock>,
    },
}
