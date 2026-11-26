use super::{EndorsementReservation, MinterJournal};
use bitcoin::Txid;
use btc_integration::{AuthenticatedMintingEndorsement, MintingEndorsementData};
use flamechain::{BlockHash, CoreFlameHeight};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    sync::{Arc, RwLock},
};

#[derive(Clone, Default)]
pub struct InMemoryMinterJournal {
    endorsements: Arc<RwLock<BTreeMap<CoreFlameHeight, EndorsementRecord>>>,
}

struct EndorsementRecord {
    endorsement: MintingEndorsementData,
    state: EndorsementState,
}

enum EndorsementState {
    Reserved,
    Sent(Txid),
    Committed(AuthenticatedMintingEndorsement),
}

impl EndorsementRecord {
    fn check_endorsement(
        &self,
        endorsement: &MintingEndorsementData,
    ) -> Result<(), InMemoryMinterJournalError> {
        if self.endorsement != *endorsement {
            return Err(InMemoryMinterJournalError::ConflictingEndorsement {
                height: endorsement.block_height(),
                expected: self.endorsement.block_hash(),
                actual: endorsement.block_hash(),
            });
        }
        Ok(())
    }

    fn transaction_id(&self) -> Option<Txid> {
        match &self.state {
            EndorsementState::Reserved => None,
            EndorsementState::Sent(txid) => Some(*txid),
            EndorsementState::Committed(endorsement) => Some(endorsement.txid()),
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

    async fn write_reserve_endorsement(
        &self,
        endorsement: &MintingEndorsementData,
    ) -> Result<EndorsementReservation, Self::Error> {
        let mut endorsements = self
            .endorsements
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        match endorsements.entry(endorsement.block_height()) {
            Entry::Vacant(entry) => {
                entry.insert(EndorsementRecord {
                    endorsement: endorsement.clone(),
                    state: EndorsementState::Reserved,
                });
                Ok(EndorsementReservation::Reserved)
            }
            Entry::Occupied(entry) => {
                entry.get().check_endorsement(endorsement)?;
                Ok(EndorsementReservation::AlreadyReserved)
            }
        }
    }

    async fn write_sent_endorsement(
        &self,
        endorsement: &MintingEndorsementData,
        transaction_id: &Txid,
    ) -> Result<(), Self::Error> {
        let mut endorsements = self
            .endorsements
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        let record = endorsements.get_mut(&endorsement.block_height()).ok_or(
            Self::Error::EndorsementNotReserved(endorsement.block_height()),
        )?;
        record.check_endorsement(endorsement)?;
        record.check_transaction_id(*transaction_id)?;
        if matches!(record.state, EndorsementState::Reserved) {
            record.state = EndorsementState::Sent(*transaction_id);
        }
        Ok(())
    }

    async fn write_committed_endorsement(
        &self,
        endorsement: &AuthenticatedMintingEndorsement,
    ) -> Result<(), Self::Error> {
        let mut endorsements = self
            .endorsements
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        let record = endorsements.get_mut(&endorsement.block_height()).ok_or(
            Self::Error::EndorsementNotReserved(endorsement.block_height()),
        )?;
        record.check_endorsement(&endorsement.output().data)?;
        if record.transaction_id().is_none() {
            return Err(Self::Error::EndorsementNotSent(endorsement.block_height()));
        }
        record.check_transaction_id(endorsement.txid())?;
        if matches!(record.state, EndorsementState::Sent(_)) {
            record.state = EndorsementState::Committed(endorsement.clone());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryMinterJournalError {
    LockPoisoned,
    EndorsementNotReserved(CoreFlameHeight),
    EndorsementNotSent(CoreFlameHeight),
    ConflictingEndorsement {
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
            Self::EndorsementNotReserved(height) => {
                write!(
                    formatter,
                    "endorsement at core height {height:?} is not reserved"
                )
            }
            Self::EndorsementNotSent(height) => {
                write!(
                    formatter,
                    "endorsement at core height {height:?} is not sent"
                )
            }
            Self::ConflictingEndorsement {
                height,
                expected,
                actual,
            } => write!(
                formatter,
                "conflicting endorsement at core height {height:?}: expected {expected:?}, got {actual:?}"
            ),
            Self::TransactionIdMismatch { expected, actual } => write!(
                formatter,
                "endorsement transaction ID mismatch: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for InMemoryMinterJournalError {}

#[cfg(test)]
mod tests;
