use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    sync::{Arc, RwLock},
};

use btc_integration::{AuthenticatedMintingVote, MintingVoteData};
use corepc_client::bitcoin::Txid;
use flamechain::{BlockHash, CoreFlameHeight};

use super::{MinterJournal, VoteReservation};

#[derive(Clone, Default)]
pub struct InMemoryMinterJournal {
    votes: Arc<RwLock<BTreeMap<CoreFlameHeight, VoteRecord>>>,
}

struct VoteRecord {
    vote: MintingVoteData,
    state: VoteState,
}

enum VoteState {
    Reserved,
    Sent(Txid),
    Committed(AuthenticatedMintingVote),
}

impl VoteRecord {
    fn check_vote(&self, vote: &MintingVoteData) -> Result<(), InMemoryMinterJournalError> {
        if self.vote != *vote {
            return Err(InMemoryMinterJournalError::ConflictingVote {
                height: vote.block_height(),
                expected: self.vote.block_hash(),
                actual: vote.block_hash(),
            });
        }
        Ok(())
    }

    fn transaction_id(&self) -> Option<Txid> {
        match &self.state {
            VoteState::Reserved => None,
            VoteState::Sent(txid) => Some(*txid),
            VoteState::Committed(vote) => Some(vote.txid()),
        }
    }

    fn check_transaction_id(&self, actual: Txid) -> Result<(), InMemoryMinterJournalError> {
        if let Some(expected) = self.transaction_id() {
            if expected != actual {
                return Err(InMemoryMinterJournalError::TransactionIdMismatch { expected, actual });
            }
        }
        Ok(())
    }
}

impl InMemoryMinterJournal {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MinterJournal for InMemoryMinterJournal {
    type Error = InMemoryMinterJournalError;
    type TransactionId = Txid;

    async fn write_reserve_vote(
        &self,
        vote: &MintingVoteData,
    ) -> Result<VoteReservation, Self::Error> {
        let mut votes = self.votes.write().map_err(|_| Self::Error::LockPoisoned)?;
        match votes.entry(vote.block_height()) {
            Entry::Vacant(entry) => {
                entry.insert(VoteRecord {
                    vote: vote.clone(),
                    state: VoteState::Reserved,
                });
                Ok(VoteReservation::Reserved)
            }
            Entry::Occupied(entry) => {
                entry.get().check_vote(vote)?;
                Ok(VoteReservation::AlreadyReserved)
            }
        }
    }

    async fn write_sent_vote(
        &self,
        vote: &MintingVoteData,
        transaction_id: &Txid,
    ) -> Result<(), Self::Error> {
        let mut votes = self.votes.write().map_err(|_| Self::Error::LockPoisoned)?;
        let record = votes
            .get_mut(&vote.block_height())
            .ok_or(Self::Error::VoteNotReserved(vote.block_height()))?;
        record.check_vote(vote)?;
        record.check_transaction_id(*transaction_id)?;
        if matches!(record.state, VoteState::Reserved) {
            record.state = VoteState::Sent(*transaction_id);
        }
        Ok(())
    }

    async fn write_committed_vote(
        &self,
        vote: &AuthenticatedMintingVote,
    ) -> Result<(), Self::Error> {
        let mut votes = self.votes.write().map_err(|_| Self::Error::LockPoisoned)?;
        let record = votes
            .get_mut(&vote.block_height())
            .ok_or(Self::Error::VoteNotReserved(vote.block_height()))?;
        record.check_vote(&vote.output().data)?;
        if record.transaction_id().is_none() {
            return Err(Self::Error::VoteNotSent(vote.block_height()));
        }
        record.check_transaction_id(vote.txid())?;
        if matches!(record.state, VoteState::Sent(_)) {
            record.state = VoteState::Committed(vote.clone());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryMinterJournalError {
    LockPoisoned,
    VoteNotReserved(CoreFlameHeight),
    VoteNotSent(CoreFlameHeight),
    ConflictingVote {
        height: CoreFlameHeight,
        expected: BlockHash,
        actual: BlockHash,
    },
    TransactionIdMismatch {
        expected: Txid,
        actual: Txid,
    },
}

impl fmt::Display for InMemoryMinterJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "minter journal lock poisoned"),
            Self::VoteNotReserved(height) => {
                write!(formatter, "vote at core height {height:?} is not reserved")
            }
            Self::VoteNotSent(height) => {
                write!(formatter, "vote at core height {height:?} is not sent")
            }
            Self::ConflictingVote {
                height,
                expected,
                actual,
            } => write!(
                formatter,
                "conflicting vote at core height {height:?}: expected {expected:?}, got {actual:?}"
            ),
            Self::TransactionIdMismatch { expected, actual } => write!(
                formatter,
                "vote transaction ID mismatch: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for InMemoryMinterJournalError {}

#[cfg(test)]
mod tests;
