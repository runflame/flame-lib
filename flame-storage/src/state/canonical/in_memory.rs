use std::{
    fmt,
    sync::{Arc, RwLock},
};

use flamechain::{BlockHash, BlockTip, Blockchain};

use super::CanonicalStorage;

#[derive(Clone, Default)]
pub struct InMemoryCanonicalStorage {
    state: Arc<RwLock<Option<Blockchain>>>,
}

impl InMemoryCanonicalStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CanonicalStorage for InMemoryCanonicalStorage {
    type Error = InMemoryCanonicalStorageError;

    async fn get_tip(&self) -> Result<Option<BlockTip>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state.as_ref().map(|state| BlockTip {
            hash: state.tip(),
            height: state.height().into(),
        }))
    }

    async fn get_state(&self) -> Result<Option<(BlockHash, Blockchain)>, Self::Error> {
        let state = self.state.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(state.as_ref().map(|state| (state.tip(), state.clone())))
    }

    async fn commit_state(&self, state: &Blockchain) -> Result<(), Self::Error> {
        let snapshot = state.clone();
        let mut stored = self.state.write().map_err(|_| Self::Error::LockPoisoned)?;
        *stored = Some(snapshot);
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryCanonicalStorageError {
    LockPoisoned,
}

impl fmt::Display for InMemoryCanonicalStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "canonical storage lock poisoned"),
        }
    }
}

impl std::error::Error for InMemoryCanonicalStorageError {}
