use crate::block_processing::journal::{
    ApplyStage, BitcoinBlockJournal, BitcoinBlockJournalEntry, FlameTransition, JournalStatus,
    RewardsChanges, RollbackStage,
};
use crate::consensus::{ConsensusUndo, MintingOutcome};
use InMemoryBitcoinBlockJournalError as Error;
use bitcoin::BlockHash;
use btc_integration::watcher::BlockId;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct InMemoryBitcoinBlockJournal {
    state: Arc<RwLock<BitcoinBlockJournalState>>,
}

struct BitcoinBlockJournalState {
    applied_tip: BlockId,
    pending_tip: Option<BlockId>,
    entries: BTreeMap<BlockHash, BitcoinBlockJournalEntry>,
}

impl InMemoryBitcoinBlockJournal {
    pub fn new(initial_btc_tip: BlockId) -> Self {
        Self {
            state: Arc::new(RwLock::new(BitcoinBlockJournalState {
                applied_tip: initial_btc_tip,
                pending_tip: None,
                entries: BTreeMap::new(),
            })),
        }
    }

    fn complete_stage(&self, btc_tip: BlockId, expected: JournalStatus) -> Result<(), Error> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        let next = expected.next().expect("operation stages have a successor");
        let entry = state.entry(btc_tip)?;
        if entry.status == next {
            return Ok(());
        }
        let entry = state.pending_entry(btc_tip, expected)?;
        match expected {
            JournalStatus::Applying(ApplyStage::PrepareFlameTransition)
                if entry.flame_transition.is_none() =>
            {
                return Err(Error::MissingFlameTransition);
            }
            JournalStatus::Applying(ApplyStage::PrepareRewards)
                if entry.rewards_changes.is_none() =>
            {
                return Err(Error::MissingRewardsChanges);
            }
            _ => {}
        }
        entry.status = next;
        let previous_tip = entry.previous_btc_tip;
        match next {
            JournalStatus::Applied => {
                state.applied_tip = btc_tip;
                state.pending_tip = None;
            }
            JournalStatus::RolledBack => {
                state.applied_tip = previous_tip;
                state.pending_tip = None;
            }
            _ => {}
        }
        Ok(())
    }
}

impl BitcoinBlockJournalState {
    fn require_idle(&self) -> Result<(), Error> {
        match self.pending_tip {
            Some(tip) => Err(Error::OperationInProgress(tip)),
            None => Ok(()),
        }
    }

    fn entry(&self, btc_tip: BlockId) -> Result<&BitcoinBlockJournalEntry, Error> {
        let entry = self
            .entries
            .get(&btc_tip.hash)
            .ok_or(Error::EntryNotFound(btc_tip))?;
        check_tip(entry.btc_tip, btc_tip)?;
        Ok(entry)
    }

    fn pending_entry(
        &mut self,
        btc_tip: BlockId,
        expected: JournalStatus,
    ) -> Result<&mut BitcoinBlockJournalEntry, Error> {
        check_tip(self.pending_tip.ok_or(Error::NoPendingOperation)?, btc_tip)?;
        let entry = self
            .entries
            .get_mut(&btc_tip.hash)
            .ok_or(Error::EntryNotFound(btc_tip))?;
        check_tip(entry.btc_tip, btc_tip)?;
        check_status(expected, entry.status)?;
        Ok(entry)
    }
}

impl BitcoinBlockJournal for InMemoryBitcoinBlockJournal {
    type Error = Error;

    async fn begin_apply(
        &self,
        previous_btc_tip: BlockId,
        outcome: &MintingOutcome,
        undo: &ConsensusUndo,
    ) -> Result<(), Self::Error> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        state.require_idle()?;
        check_tip(state.applied_tip, previous_btc_tip)?;
        let btc_tip = outcome.next_btc_cursor;
        if previous_btc_tip.height.checked_add(1) != Some(btc_tip.height)
            || previous_btc_tip.hash == btc_tip.hash
        {
            return Err(Error::InvalidSuccessor {
                previous: previous_btc_tip,
                next: btc_tip,
            });
        }
        if let Some(entry) = state.entries.get(&btc_tip.hash) {
            check_tip(entry.btc_tip, btc_tip)?;
            check_tip(entry.previous_btc_tip, previous_btc_tip)?;
            if entry.status != JournalStatus::RolledBack {
                return Err(Error::EntryAlreadyExists(btc_tip));
            }
        }
        state.entries.insert(
            btc_tip.hash,
            BitcoinBlockJournalEntry::new(previous_btc_tip, outcome.clone(), undo.clone()),
        );
        state.pending_tip = Some(btc_tip);
        Ok(())
    }

    async fn get_entry(
        &self,
        btc_tip: BlockId,
    ) -> Result<Option<BitcoinBlockJournalEntry>, Self::Error> {
        let state = self.state.read().map_err(|_| Error::LockPoisoned)?;
        if !state.entries.contains_key(&btc_tip.hash) {
            return Ok(None);
        }
        Ok(Some(state.entry(btc_tip)?.clone()))
    }

    async fn get_pending_entry(&self) -> Result<Option<BitcoinBlockJournalEntry>, Self::Error> {
        let state = self.state.read().map_err(|_| Error::LockPoisoned)?;
        state
            .pending_tip
            .map(|tip| state.entry(tip).cloned())
            .transpose()
    }

    async fn store_flame_transition(
        &self,
        btc_tip: BlockId,
        transition: &FlameTransition,
    ) -> Result<(), Self::Error> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        let entry = state.pending_entry(
            btc_tip,
            JournalStatus::Applying(ApplyStage::PrepareFlameTransition),
        )?;
        if entry.flame_transition.is_some() {
            return Err(Error::FlameTransitionAlreadyPrepared);
        }
        entry.flame_transition = Some(transition.clone());
        Ok(())
    }

    async fn store_rewards_changes(
        &self,
        btc_tip: BlockId,
        changes: &RewardsChanges,
    ) -> Result<(), Self::Error> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        let entry =
            state.pending_entry(btc_tip, JournalStatus::Applying(ApplyStage::PrepareRewards))?;
        if entry.rewards_changes.is_some() {
            return Err(Error::RewardsChangesAlreadyPrepared);
        }
        entry.rewards_changes = Some(changes.clone());
        Ok(())
    }

    async fn complete_apply_stage(
        &self,
        btc_tip: BlockId,
        stage: ApplyStage,
    ) -> Result<(), Self::Error> {
        self.complete_stage(btc_tip, JournalStatus::Applying(stage))
    }

    async fn begin_rollback(&self, btc_tip: BlockId) -> Result<(), Self::Error> {
        let mut state = self.state.write().map_err(|_| Error::LockPoisoned)?;
        state.require_idle()?;
        check_tip(state.applied_tip, btc_tip)?;
        let entry = state
            .entries
            .get_mut(&btc_tip.hash)
            .ok_or(Error::EntryNotFound(btc_tip))?;
        check_tip(entry.btc_tip, btc_tip)?;
        check_status(JournalStatus::Applied, entry.status)?;
        entry.status = JournalStatus::RollingBack(RollbackStage::RemoveEndorsements);
        state.pending_tip = Some(btc_tip);
        Ok(())
    }

    async fn complete_rollback_stage(
        &self,
        btc_tip: BlockId,
        stage: RollbackStage,
    ) -> Result<(), Self::Error> {
        self.complete_stage(btc_tip, JournalStatus::RollingBack(stage))
    }
}

fn check_tip(expected: BlockId, actual: BlockId) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::BlockTipMismatch { expected, actual });
    }
    Ok(())
}

fn check_status(expected: JournalStatus, actual: JournalStatus) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::UnexpectedStatus { expected, actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InMemoryBitcoinBlockJournalError {
    #[error("Bitcoin block journal lock poisoned")]
    LockPoisoned,
    #[error("a journal operation is already in progress for {0:?}")]
    OperationInProgress(BlockId),
    #[error("no journal operation is in progress")]
    NoPendingOperation,
    #[error("journal entry not found for {0:?}")]
    EntryNotFound(BlockId),
    #[error("journal entry already exists for {0:?}")]
    EntryAlreadyExists(BlockId),
    #[error("Bitcoin block tip mismatch: expected {expected:?}, got {actual:?}")]
    BlockTipMismatch { expected: BlockId, actual: BlockId },
    #[error("{next:?} is not a successor of {previous:?}")]
    InvalidSuccessor { previous: BlockId, next: BlockId },
    #[error("journal status mismatch: expected {expected:?}, got {actual:?}")]
    UnexpectedStatus {
        expected: JournalStatus,
        actual: JournalStatus,
    },
    #[error("Flame transition has already been prepared")]
    FlameTransitionAlreadyPrepared,
    #[error("reward changes have already been prepared")]
    RewardsChangesAlreadyPrepared,
    #[error("Flame transition has not been prepared")]
    MissingFlameTransition,
    #[error("reward changes have not been prepared")]
    MissingRewardsChanges,
}
