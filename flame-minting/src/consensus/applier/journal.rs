use std::future::Future;

use btc_integration::BtcBlockTip;

use crate::consensus::MintingOutcome;

pub mod in_memory;

pub use in_memory::{InMemoryMintingJournal, InMemoryMintingJournalError};

pub trait MintingJournal {
    type Error;

    /// Stores a single pending outcome. Returns an error without overwriting it
    /// if a pending outcome already exists.
    fn write_pending_outcome(
        &self,
        outcome: &MintingOutcome,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_pending_outcome(
        &self,
    ) -> impl Future<Output = Result<Option<MintingOutcome>, Self::Error>> + Send;

    fn get_intent(&self) -> impl Future<Output = Result<Option<(u64, bool)>, Self::Error>> + Send;

    fn write_intent(
        &self,
        intent: u64,
        ended: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn mark_outcome_applied(
        &self,
        btc_block: BtcBlockTip,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
