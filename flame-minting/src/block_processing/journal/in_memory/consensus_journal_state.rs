use super::in_memory_consensus_journal::check_tip;
use super::in_memory_consensus_journal_error::InMemoryConsensusJournalError as Error;
use crate::block_processing::journal::{
    BitcoinJournalOperation, ConsensusJournalEntry, ConsensusOperation, FlameJournalOperation,
    OperationId,
};

use bitcoin::BlockHash;
use btc_integration::watcher::BlockId;
use std::collections::BTreeMap;

pub(super) struct ConsensusJournalState {
    pub(super) next_id: u64,
    pub(super) applied_btc_tip: BlockId,
    pub(super) pending: Option<OperationId>,
    pub(super) entries: BTreeMap<OperationId, ConsensusJournalEntry>,
    pub(super) bitcoin_entries: BTreeMap<BlockHash, OperationId>,
}

impl ConsensusJournalState {
    pub(super) fn require_idle(&self) -> Result<(), Error> {
        if let Some(id) = self.pending {
            return Err(Error::OperationInProgress(id));
        }
        Ok(())
    }

    pub(super) fn require_pending(&self, id: OperationId) -> Result<(), Error> {
        if self.pending != Some(id) {
            return Err(Error::NotPending(id));
        }
        Ok(())
    }

    pub(super) fn begin(&mut self, operation: ConsensusOperation) -> Result<OperationId, Error> {
        self.require_idle()?;
        let id = OperationId(self.next_id);
        let next_id = self.next_id.checked_add(1).ok_or(Error::IdOverflow)?;
        if let ConsensusOperation::Bitcoin(operation) = &operation {
            self.bitcoin_entries
                .insert(operation.entry.btc_tip.hash, id);
        }
        self.entries
            .insert(id, ConsensusJournalEntry { id, operation });
        self.next_id = next_id;
        self.pending = Some(id);
        Ok(id)
    }

    pub(super) fn bitcoin(&self, id: OperationId) -> Result<&BitcoinJournalOperation, Error> {
        match &self
            .entries
            .get(&id)
            .ok_or(Error::MissingEntry(id))?
            .operation
        {
            ConsensusOperation::Bitcoin(operation) => Ok(operation),
            ConsensusOperation::Flame(_) => Err(Error::WrongOperationKind(id)),
        }
    }

    pub(super) fn bitcoin_mut(
        &mut self,
        id: OperationId,
    ) -> Result<&mut BitcoinJournalOperation, Error> {
        match &mut self
            .entries
            .get_mut(&id)
            .ok_or(Error::MissingEntry(id))?
            .operation
        {
            ConsensusOperation::Bitcoin(operation) => Ok(operation),
            ConsensusOperation::Flame(_) => Err(Error::WrongOperationKind(id)),
        }
    }

    pub(super) fn flame(&self, id: OperationId) -> Result<&FlameJournalOperation, Error> {
        match &self
            .entries
            .get(&id)
            .ok_or(Error::MissingEntry(id))?
            .operation
        {
            ConsensusOperation::Flame(entry) => Ok(entry),
            ConsensusOperation::Bitcoin(_) => Err(Error::WrongOperationKind(id)),
        }
    }

    pub(super) fn flame_mut(
        &mut self,
        id: OperationId,
    ) -> Result<&mut FlameJournalOperation, Error> {
        match &mut self
            .entries
            .get_mut(&id)
            .ok_or(Error::MissingEntry(id))?
            .operation
        {
            ConsensusOperation::Flame(entry) => Ok(entry),
            ConsensusOperation::Bitcoin(_) => Err(Error::WrongOperationKind(id)),
        }
    }

    pub(super) fn bitcoin_id(&self, tip: BlockId) -> Result<Option<OperationId>, Error> {
        let Some(&id) = self.bitcoin_entries.get(&tip.hash) else {
            return Ok(None);
        };
        check_tip(self.bitcoin(id)?.entry.btc_tip, tip)?;
        Ok(Some(id))
    }
}
