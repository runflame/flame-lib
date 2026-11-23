use crate::block_processing::journal::{FlameJournalStatus, JournalStatus, OperationId};

use btc_integration::watcher::BlockId;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InMemoryConsensusJournalError {
    #[error("consensus journal lock poisoned")]
    LockPoisoned,
    #[error("journal operation {0:?} is still in progress")]
    OperationInProgress(OperationId),
    #[error("journal operation {0:?} is not pending")]
    NotPending(OperationId),
    #[error("journal operation {0:?} does not exist")]
    MissingEntry(OperationId),
    #[error("journal operation {0:?} belongs to a different processor")]
    WrongOperationKind(OperationId),
    #[error("journal operation ID overflow")]
    IdOverflow,
    #[error("Bitcoin block tip mismatch: expected {expected:?}, got {actual:?}")]
    BlockTipMismatch { expected: BlockId, actual: BlockId },
    #[error("{next:?} is not a successor of {previous:?}")]
    InvalidSuccessor { previous: BlockId, next: BlockId },
    #[error("Bitcoin journal entry not found for {0:?}")]
    BitcoinEntryNotFound(BlockId),
    #[error("Bitcoin journal entry already exists for {0:?}")]
    BitcoinEntryAlreadyExists(BlockId),
    #[error("Bitcoin journal status mismatch: expected {expected:?}, got {actual:?}")]
    UnexpectedBitcoinStatus {
        expected: JournalStatus,
        actual: JournalStatus,
    },
    #[error("Flame journal status mismatch: expected {expected:?}, got {actual:?}")]
    UnexpectedFlameStatus {
        expected: FlameJournalStatus,
        actual: FlameJournalStatus,
    },
    #[error("a saved plan cannot be overwritten")]
    PlanAlreadyStored,
    #[error("Flame transition has not been prepared")]
    MissingTransition,
    #[error("reward changes have not been prepared")]
    MissingRewards,
}
