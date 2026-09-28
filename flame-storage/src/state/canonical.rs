use std::future::Future;

use flamechain::{BlockHash, BlockTip, Blockchain};

pub mod in_memory;

pub use in_memory::{InMemoryCanonicalStorage, InMemoryCanonicalStorageError};

pub trait CanonicalStorage {
    type Error;

    fn get_tip(&self) -> impl Future<Output = Result<Option<BlockTip>, Self::Error>> + Send;

    fn get_state(
        &self,
    ) -> impl Future<Output = Result<Option<(BlockHash, Blockchain)>, Self::Error>> + Send;
    fn commit_state(
        &self,
        state: &Blockchain,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
