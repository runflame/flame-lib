use super::flame_journal_status::FlameApplyStage;
use crate::block_processing::journal::{
    FlameJournalEntry, FlameTransition, OperationId, RewardsChanges,
};
use crate::consensus::{ConsensusOutcome, ConsensusUndo};
use btc_integration::watcher::BlockId;
use flamechain::BlockTip;
use std::future::Future;

pub trait FlameJournal: Send + Sync + 'static {
    type Error: Send + 'static;

    fn begin_apply(
        &self,
        tip: BlockTip,
        btc_tip: BlockId,
        outcome: &ConsensusOutcome,
        undo: &ConsensusUndo,
    ) -> impl Future<Output = Result<OperationId, Self::Error>> + Send;

    fn pending_entry(
        &self,
    ) -> impl Future<Output = Result<Option<FlameJournalEntry>, Self::Error>> + Send;

    fn store_transition(
        &self,
        id: OperationId,
        transition: &FlameTransition,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn store_rewards(
        &self,
        id: OperationId,
        rewards: &RewardsChanges,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn complete_apply_stage(
        &self,
        id: OperationId,
        stage: FlameApplyStage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
