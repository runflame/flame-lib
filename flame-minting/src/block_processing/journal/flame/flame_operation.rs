use super::flame_journal_status::FlameJournalStatus;
use crate::block_processing::canonical_chain::FlameTransition;
use crate::block_processing::journal::operation::JournalEntry;
use crate::consensus::{ConsensusOutcome, ConsensusUndo};
use crate::rewards::application::rewards_changes::RewardsChanges;
use btc_integration::watcher::BlockId;
use flamechain::BlockTip;

#[derive(Clone, Debug)]
pub struct FlameJournalOperation {
    pub tip: BlockTip,
    pub btc_tip: BlockId,
    pub outcome: ConsensusOutcome,
    pub undo: ConsensusUndo,
    pub transition: Option<FlameTransition>,
    pub rewards: Option<RewardsChanges>,
    pub status: FlameJournalStatus,
}

pub type FlameJournalEntry = JournalEntry<FlameJournalOperation>;
