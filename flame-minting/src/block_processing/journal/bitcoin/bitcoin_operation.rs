use super::journal_status::{ApplyStage, JournalStatus};
use crate::block_processing::BtcBlockRecords;
use crate::block_processing::canonical_chain::FlameTransition;
use crate::block_processing::journal::operation::JournalEntry;
use crate::consensus::{BitcoinRollbackPlan, ConsensusUndo, MintingOutcome};
use crate::rewards::application::rewards_changes::RewardsChanges;
use btc_integration::watcher::BlockId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitcoinOperationKind {
    Apply,
    Rollback,
}

#[derive(Clone, Debug)]
pub struct BitcoinJournalOperation {
    pub kind: BitcoinOperationKind,
    pub block: BtcBlockRecords,
    pub entry: BitcoinBlockJournalEntry,
    pub rollback_plan: Option<BitcoinRollbackPlan>,
}

#[derive(Clone, Debug)]
pub struct BitcoinBlockJournalEntry {
    pub btc_tip: BlockId,
    pub previous_btc_tip: BlockId,
    pub minting_outcome: MintingOutcome,
    pub consensus_undo: ConsensusUndo,
    pub flame_transition: Option<FlameTransition>,
    pub rewards_changes: Option<RewardsChanges>,
    pub status: JournalStatus,
}

impl BitcoinBlockJournalEntry {
    pub fn new(
        previous_btc_tip: BlockId,
        minting_outcome: MintingOutcome,
        consensus_undo: ConsensusUndo,
    ) -> Self {
        Self {
            btc_tip: minting_outcome.next_btc_cursor,
            previous_btc_tip,
            minting_outcome,
            consensus_undo,
            flame_transition: None,
            rewards_changes: None,
            status: JournalStatus::Applying(ApplyStage::RemoveEndorsements),
        }
    }
}

pub type BitcoinJournalEntry = JournalEntry<BitcoinJournalOperation>;
