use crate::block_processing::BtcBlockRecords;
use crate::block_processing::journal::{
    ApplyStage, BitcoinJournalEntry, FlameTransition, OperationId, RewardsChanges, RollbackStage,
};
use crate::consensus::{BitcoinRollbackPlan, ConsensusUndo, IncludedEndorsement, MintingOutcome};
use btc_integration::watcher::BlockId;
use std::future::Future;

pub trait BitcoinJournal: Send + Sync + 'static {
    type Error: Send + 'static;

    fn begin_apply(
        &self,
        previous_btc_tip: BlockId,
        block: &BtcBlockRecords,
        outcome: &MintingOutcome,
        undo: &ConsensusUndo,
    ) -> impl Future<Output = Result<OperationId, Self::Error>> + Send;

    fn get_entry(
        &self,
        btc_tip: BlockId,
    ) -> impl Future<Output = Result<Option<BitcoinJournalEntry>, Self::Error>> + Send;

    fn pending_entry(
        &self,
    ) -> impl Future<Output = Result<Option<BitcoinJournalEntry>, Self::Error>> + Send;

    fn store_flame_transition(
        &self,
        id: OperationId,
        transition: &FlameTransition,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn store_rewards_changes(
        &self,
        id: OperationId,
        changes: &RewardsChanges,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn complete_apply_stage(
        &self,
        id: OperationId,
        stage: ApplyStage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn begin_rollback(
        &self,
        btc_tip: BlockId,
        plan: &BitcoinRollbackPlan,
    ) -> impl Future<Output = Result<OperationId, Self::Error>> + Send;

    fn get_applied_endorsements(
        &self,
    ) -> impl Future<Output = Result<Vec<IncludedEndorsement>, Self::Error>> + Send;

    fn complete_rollback_stage(
        &self,
        id: OperationId,
        stage: RollbackStage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
