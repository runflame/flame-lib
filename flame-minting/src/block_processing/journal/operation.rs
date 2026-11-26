use super::bitcoin::bitcoin_operation::BitcoinJournalOperation;
use super::bitcoin::journal_status::JournalStatus;
use super::flame::flame_journal_status::FlameJournalStatus;
use super::flame::flame_operation::FlameJournalOperation;
use btc_integration::watcher::BlockId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationId(pub u64);

#[derive(Clone, Debug)]
pub enum ConsensusOperation {
    Bitcoin(BitcoinJournalOperation),
    Flame(FlameJournalOperation),
}

#[derive(Clone, Debug)]
pub struct JournalEntry<T> {
    pub id: OperationId,
    pub operation: T,
}

pub type ConsensusJournalEntry = JournalEntry<ConsensusOperation>;

impl ConsensusJournalEntry {
    pub fn is_complete(&self) -> bool {
        match &self.operation {
            ConsensusOperation::Bitcoin(operation) => matches!(
                operation.entry.status,
                JournalStatus::Applied | JournalStatus::RolledBack
            ),
            ConsensusOperation::Flame(entry) => matches!(entry.status, FlameJournalStatus::Applied),
        }
    }

    pub fn btc_tip(&self) -> BlockId {
        match &self.operation {
            ConsensusOperation::Bitcoin(operation) => operation.entry.btc_tip,
            ConsensusOperation::Flame(entry) => entry.btc_tip,
        }
    }
}
