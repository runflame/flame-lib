use std::{
    fmt,
    sync::{Arc, RwLock},
};

use btc_integration::BtcBlockTip;

use crate::consensus::MintingOutcome;

use super::MintingJournal;

#[derive(Clone, Default)]
pub struct InMemoryMintingJournal {
    pending: Arc<RwLock<Option<PendingOutcome>>>,
}

struct PendingOutcome {
    outcome: MintingOutcome,
    intent: Option<(u64, bool)>,
}

impl InMemoryMintingJournal {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MintingJournal for InMemoryMintingJournal {
    type Error = InMemoryMintingJournalError;

    async fn write_pending_outcome(&self, outcome: &MintingOutcome) -> Result<(), Self::Error> {
        let mut pending = self
            .pending
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        if pending.is_some() {
            return Err(Self::Error::PendingOutcomeExists);
        }
        *pending = Some(PendingOutcome {
            outcome: outcome.clone(),
            intent: None,
        });
        Ok(())
    }

    async fn get_pending_outcome(&self) -> Result<Option<MintingOutcome>, Self::Error> {
        let pending = self.pending.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(pending.as_ref().map(|pending| pending.outcome.clone()))
    }

    async fn get_intent(&self) -> Result<Option<(u64, bool)>, Self::Error> {
        let pending = self.pending.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(pending.as_ref().and_then(|pending| pending.intent))
    }

    async fn write_intent(&self, intent: u64, ended: bool) -> Result<(), Self::Error> {
        let mut pending = self
            .pending
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        let pending = pending.as_mut().ok_or(Self::Error::NoPendingOutcome)?;
        pending.intent = Some((intent, ended));
        Ok(())
    }

    async fn mark_outcome_applied(&self, btc_block: BtcBlockTip) -> Result<(), Self::Error> {
        let mut pending = self
            .pending
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?;
        let expected = pending
            .as_ref()
            .ok_or(Self::Error::NoPendingOutcome)?
            .outcome
            .next_btc_cursor;
        if btc_block != expected {
            return Err(Self::Error::BlockTipMismatch {
                expected,
                actual: btc_block,
            });
        }
        *pending = None;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryMintingJournalError {
    LockPoisoned,
    PendingOutcomeExists,
    NoPendingOutcome,
    BlockTipMismatch {
        expected: BtcBlockTip,
        actual: BtcBlockTip,
    },
}

impl fmt::Display for InMemoryMintingJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "minting journal lock poisoned"),
            Self::PendingOutcomeExists => write!(formatter, "minting outcome is already pending"),
            Self::NoPendingOutcome => write!(formatter, "no pending minting outcome"),
            Self::BlockTipMismatch { expected, actual } => write!(
                formatter,
                "minting outcome block tip mismatch: expected {expected:?}, got {actual:?}"
            ),
        }
    }
}

impl std::error::Error for InMemoryMintingJournalError {}
