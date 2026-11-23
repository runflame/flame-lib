use super::in_memory_consensus_journal::InMemoryConsensusJournal;
use super::in_memory_consensus_journal::check_tip;
use super::in_memory_consensus_journal_error::InMemoryConsensusJournalError as Error;
use crate::block_processing::journal::{
    ConsensusOperation, FlameApplyStage, FlameJournal, FlameJournalEntry, FlameJournalOperation,
    FlameJournalStatus, FlameTransition, OperationId, RewardsChanges,
};
use crate::consensus::{ConsensusOutcome, ConsensusUndo};
use btc_integration::watcher::BlockId;
use flamechain::BlockTip;

#[derive(Clone)]
pub struct InMemoryFlameJournal {
    pub(super) journal: InMemoryConsensusJournal,
}

impl FlameJournal for InMemoryFlameJournal {
    type Error = Error;

    async fn begin_apply(
        &self,
        tip: BlockTip,
        btc_tip: BlockId,
        outcome: &ConsensusOutcome,
        undo: &ConsensusUndo,
    ) -> Result<OperationId, Error> {
        let mut state = self.journal.write()?;
        state.require_idle()?;
        check_tip(state.applied_btc_tip, btc_tip)?;
        state.begin(ConsensusOperation::Flame(FlameJournalOperation {
            tip,
            btc_tip,
            outcome: outcome.clone(),
            undo: undo.clone(),
            transition: None,
            rewards: None,
            status: FlameJournalStatus::Applying(FlameApplyStage::Consensus),
        }))
    }

    async fn pending_entry(&self) -> Result<Option<FlameJournalEntry>, Error> {
        let state = self.journal.read()?;
        match state.pending {
            Some(id) if matches!(state.entries[&id].operation, ConsensusOperation::Flame(_)) => {
                Ok(Some(FlameJournalEntry {
                    id,
                    operation: state.flame(id)?.clone(),
                }))
            }
            _ => Ok(None),
        }
    }

    async fn store_transition(
        &self,
        id: OperationId,
        transition: &FlameTransition,
    ) -> Result<(), Error> {
        let mut state = self.journal.write()?;
        state.require_pending(id)?;
        let entry = state.flame_mut(id)?;
        check_status(
            FlameJournalStatus::Applying(FlameApplyStage::PrepareTransition),
            entry.status,
        )?;
        if entry.transition.is_some() {
            return Err(Error::PlanAlreadyStored);
        }
        entry.transition = Some(transition.clone());
        Ok(())
    }

    async fn store_rewards(&self, id: OperationId, rewards: &RewardsChanges) -> Result<(), Error> {
        let mut state = self.journal.write()?;
        state.require_pending(id)?;
        let entry = state.flame_mut(id)?;
        check_status(
            FlameJournalStatus::Applying(FlameApplyStage::PrepareRewards),
            entry.status,
        )?;
        if entry.rewards.is_some() {
            return Err(Error::PlanAlreadyStored);
        }
        entry.rewards = Some(rewards.clone());
        Ok(())
    }

    async fn complete_apply_stage(
        &self,
        id: OperationId,
        stage: FlameApplyStage,
    ) -> Result<(), Error> {
        let mut state = self.journal.write()?;
        let expected = FlameJournalStatus::Applying(stage);
        let next = expected.next().expect("apply stages have a successor");
        if state.flame(id)?.status == next {
            return Ok(());
        }
        state.require_pending(id)?;
        let entry = state.flame_mut(id)?;
        check_status(expected, entry.status)?;
        match stage {
            FlameApplyStage::PrepareTransition if entry.transition.is_none() => {
                return Err(Error::MissingTransition);
            }
            FlameApplyStage::PrepareRewards if entry.rewards.is_none() => {
                return Err(Error::MissingRewards);
            }
            _ => {}
        }
        entry.status = next;
        if next == FlameJournalStatus::Applied {
            state.pending = None;
        }
        Ok(())
    }
}

fn check_status(expected: FlameJournalStatus, actual: FlameJournalStatus) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::UnexpectedFlameStatus { expected, actual });
    }
    Ok(())
}
