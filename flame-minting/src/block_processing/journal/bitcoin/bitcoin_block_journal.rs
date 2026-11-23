use crate::block_processing::journal::{
    ApplyStage, BitcoinBlockJournalEntry, FlameTransition, RewardsChanges, RollbackStage,
};
use crate::consensus::{ConsensusUndo, MintingOutcome};
use btc_integration::watcher::BlockId;
use std::future::Future;

pub trait BitcoinBlockJournal {
    type Error;

    fn begin_apply(
        &self,
        previous_btc_tip: BlockId,
        outcome: &MintingOutcome,
        undo: &ConsensusUndo,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_entry(
        &self,
        btc_tip: BlockId,
    ) -> impl Future<Output = Result<Option<BitcoinBlockJournalEntry>, Self::Error>> + Send;

    fn get_pending_entry(
        &self,
    ) -> impl Future<Output = Result<Option<BitcoinBlockJournalEntry>, Self::Error>> + Send;

    fn store_flame_transition(
        &self,
        btc_tip: BlockId,
        transition: &FlameTransition,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn store_rewards_changes(
        &self,
        btc_tip: BlockId,
        changes: &RewardsChanges,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn complete_apply_stage(
        &self,
        btc_tip: BlockId,
        stage: ApplyStage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn begin_rollback(
        &self,
        btc_tip: BlockId,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn complete_rollback_stage(
        &self,
        btc_tip: BlockId,
        stage: RollbackStage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
