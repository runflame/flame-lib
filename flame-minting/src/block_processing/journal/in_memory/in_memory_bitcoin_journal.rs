use super::consensus_journal_state::ConsensusJournalState as State;
use super::in_memory_consensus_journal::InMemoryConsensusJournal;
use super::in_memory_consensus_journal::check_tip;
use super::in_memory_consensus_journal_error::InMemoryConsensusJournalError as Error;
use crate::block_processing::BtcBlockRecords;
use crate::block_processing::journal::{
    ApplyStage, BitcoinBlockJournalEntry, BitcoinJournal, BitcoinJournalEntry,
    BitcoinJournalOperation, BitcoinOperationKind, ConsensusOperation, FlameTransition,
    JournalStatus, OperationId, RewardsChanges, RollbackStage,
};
use crate::consensus::{BitcoinRollbackPlan, ConsensusUndo, IncludedEndorsement, MintingOutcome};
use btc_integration::watcher::BlockId;

#[derive(Clone)]
pub struct InMemoryBitcoinJournal {
    pub(super) journal: InMemoryConsensusJournal,
}

impl InMemoryBitcoinJournal {
    fn complete_stage(&self, id: OperationId, expected: JournalStatus) -> Result<(), Error> {
        let mut state = self.journal.write()?;
        let next = expected.next().expect("operation stages have a successor");
        if state.bitcoin(id)?.entry.status == next {
            return Ok(());
        }
        state.require_pending(id)?;
        let entry = &mut state.bitcoin_mut(id)?.entry;
        check_status(expected, entry.status)?;
        match expected {
            JournalStatus::Applying(ApplyStage::PrepareFlameTransition)
            | JournalStatus::RollingBack(RollbackStage::PrepareFlameTransition)
                if entry.flame_transition.is_none() =>
            {
                return Err(Error::MissingTransition);
            }
            JournalStatus::Applying(ApplyStage::PrepareRewards)
            | JournalStatus::RollingBack(RollbackStage::PrepareRewards)
                if entry.rewards_changes.is_none() =>
            {
                return Err(Error::MissingRewards);
            }
            _ => {}
        }
        entry.status = next;
        match next {
            JournalStatus::Applied => {
                state.applied_btc_tip = entry.btc_tip;
                state.pending = None;
            }
            JournalStatus::RolledBack => {
                state.applied_btc_tip = entry.previous_btc_tip;
                state.pending = None;
            }
            _ => {}
        }
        Ok(())
    }
}

impl BitcoinJournal for InMemoryBitcoinJournal {
    type Error = Error;

    async fn begin_apply(
        &self,
        previous_btc_tip: BlockId,
        block: &BtcBlockRecords,
        outcome: &MintingOutcome,
        undo: &ConsensusUndo,
    ) -> Result<OperationId, Error> {
        let mut state = self.journal.write()?;
        state.require_idle()?;
        check_tip(state.applied_btc_tip, previous_btc_tip)?;
        let tip = block.id;
        check_tip(tip, outcome.next_btc_cursor)?;
        if previous_btc_tip.height.checked_add(1) != Some(tip.height)
            || previous_btc_tip.hash == tip.hash
        {
            return Err(Error::InvalidSuccessor {
                previous: previous_btc_tip,
                next: tip,
            });
        }
        if let Some(id) = state.bitcoin_id(tip)? {
            let entry = &state.bitcoin(id)?.entry;
            check_tip(entry.previous_btc_tip, previous_btc_tip)?;
            if entry.status != JournalStatus::RolledBack {
                return Err(Error::BitcoinEntryAlreadyExists(tip));
            }
        }
        state.begin(ConsensusOperation::Bitcoin(BitcoinJournalOperation {
            kind: BitcoinOperationKind::Apply,
            block: block.clone(),
            entry: BitcoinBlockJournalEntry::new(previous_btc_tip, outcome.clone(), undo.clone()),
            rollback_plan: None,
        }))
    }

    async fn get_entry(&self, tip: BlockId) -> Result<Option<BitcoinJournalEntry>, Error> {
        let state = self.journal.read()?;
        state
            .bitcoin_id(tip)?
            .map(|id| bitcoin_snapshot(&state, id))
            .transpose()
    }

    async fn pending_entry(&self) -> Result<Option<BitcoinJournalEntry>, Error> {
        let state = self.journal.read()?;
        match state.pending {
            Some(id) if matches!(state.entries[&id].operation, ConsensusOperation::Bitcoin(_)) => {
                Ok(Some(bitcoin_snapshot(&state, id)?))
            }
            _ => Ok(None),
        }
    }

    async fn store_flame_transition(
        &self,
        id: OperationId,
        transition: &FlameTransition,
    ) -> Result<(), Error> {
        let mut state = self.journal.write()?;
        state.require_pending(id)?;
        let entry = &mut state.bitcoin_mut(id)?.entry;
        let expected = if matches!(entry.status, JournalStatus::RollingBack(_)) {
            JournalStatus::RollingBack(RollbackStage::PrepareFlameTransition)
        } else {
            JournalStatus::Applying(ApplyStage::PrepareFlameTransition)
        };
        check_status(expected, entry.status)?;
        if entry.flame_transition.is_some() {
            return Err(Error::PlanAlreadyStored);
        }
        entry.flame_transition = Some(transition.clone());
        Ok(())
    }

    async fn store_rewards_changes(
        &self,
        id: OperationId,
        changes: &RewardsChanges,
    ) -> Result<(), Error> {
        let mut state = self.journal.write()?;
        state.require_pending(id)?;
        let entry = &mut state.bitcoin_mut(id)?.entry;
        let expected = if matches!(entry.status, JournalStatus::RollingBack(_)) {
            JournalStatus::RollingBack(RollbackStage::PrepareRewards)
        } else {
            JournalStatus::Applying(ApplyStage::PrepareRewards)
        };
        check_status(expected, entry.status)?;
        if entry.rewards_changes.is_some() {
            return Err(Error::PlanAlreadyStored);
        }
        entry.rewards_changes = Some(changes.clone());
        Ok(())
    }

    async fn complete_apply_stage(&self, id: OperationId, stage: ApplyStage) -> Result<(), Error> {
        self.complete_stage(id, JournalStatus::Applying(stage))
    }

    async fn begin_rollback(
        &self,
        tip: BlockId,
        plan: &BitcoinRollbackPlan,
    ) -> Result<OperationId, Error> {
        let mut state = self.journal.write()?;
        state.require_idle()?;
        check_tip(state.applied_btc_tip, tip)?;
        let id = state
            .bitcoin_id(tip)?
            .ok_or(Error::BitcoinEntryNotFound(tip))?;
        let mut operation = state.bitcoin(id)?.clone();
        check_status(JournalStatus::Applied, operation.entry.status)?;
        operation.kind = BitcoinOperationKind::Rollback;
        operation.entry.status = JournalStatus::RollingBack(RollbackStage::RemoveEndorsements);
        operation.rollback_plan = Some(plan.clone());
        operation.entry.flame_transition = None;
        operation.entry.rewards_changes = None;
        state.begin(ConsensusOperation::Bitcoin(operation))
    }

    async fn get_applied_endorsements(&self) -> Result<Vec<IncludedEndorsement>, Error> {
        let state = self.journal.read()?;
        let mut endorsements = Vec::new();
        for &id in state.bitcoin_entries.values() {
            let operation = state.bitcoin(id)?;
            if operation.entry.status == JournalStatus::Applied {
                endorsements.extend(operation.block.endorsements.iter().cloned().map(
                    |endorsement| IncludedEndorsement {
                        btc_block: operation.block.id,
                        endorsement,
                    },
                ));
            }
        }
        Ok(endorsements)
    }

    async fn complete_rollback_stage(
        &self,
        id: OperationId,
        stage: RollbackStage,
    ) -> Result<(), Error> {
        self.complete_stage(id, JournalStatus::RollingBack(stage))
    }
}

fn bitcoin_snapshot(state: &State, id: OperationId) -> Result<BitcoinJournalEntry, Error> {
    Ok(BitcoinJournalEntry {
        id,
        operation: state.bitcoin(id)?.clone(),
    })
}

fn check_status(expected: JournalStatus, actual: JournalStatus) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::UnexpectedBitcoinStatus { expected, actual });
    }
    Ok(())
}
