use super::consensus_journal_state::ConsensusJournalState;
use super::in_memory_consensus_journal_error::InMemoryConsensusJournalError;
use super::{
    in_memory_bitcoin_journal::InMemoryBitcoinJournal,
    in_memory_flame_journal::InMemoryFlameJournal,
};
use crate::block_processing::journal::{ConsensusJournal, ConsensusJournalEntry};
use InMemoryConsensusJournalError as Error;
use btc_integration::watcher::BlockId;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone)]
pub struct InMemoryConsensusJournal {
    pub(super) state: Arc<RwLock<ConsensusJournalState>>,
}

impl InMemoryConsensusJournal {
    pub fn new(initial_btc_tip: BlockId) -> Self {
        Self {
            state: Arc::new(RwLock::new(ConsensusJournalState {
                next_id: 0,
                applied_btc_tip: initial_btc_tip,
                pending: None,
                entries: BTreeMap::new(),
                bitcoin_entries: BTreeMap::new(),
            })),
        }
    }

    pub(super) fn read(&self) -> Result<RwLockReadGuard<'_, ConsensusJournalState>, Error> {
        self.state.read().map_err(|_| Error::LockPoisoned)
    }

    pub(super) fn write(&self) -> Result<RwLockWriteGuard<'_, ConsensusJournalState>, Error> {
        self.state.write().map_err(|_| Error::LockPoisoned)
    }
}

impl ConsensusJournal for InMemoryConsensusJournal {
    type Error = Error;
    type Bitcoin = InMemoryBitcoinJournal;
    type Flame = InMemoryFlameJournal;

    fn bitcoin_journal(&self) -> Self::Bitcoin {
        InMemoryBitcoinJournal {
            journal: self.clone(),
        }
    }

    fn flame_journal(&self) -> Self::Flame {
        InMemoryFlameJournal {
            journal: self.clone(),
        }
    }

    async fn latest_entry(&self) -> Result<Option<ConsensusJournalEntry>, Error> {
        Ok(self
            .read()?
            .entries
            .last_key_value()
            .map(|(_, entry)| entry.clone()))
    }
}

pub(super) fn check_tip(expected: BlockId, actual: BlockId) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::BlockTipMismatch { expected, actual });
    }
    Ok(())
}
