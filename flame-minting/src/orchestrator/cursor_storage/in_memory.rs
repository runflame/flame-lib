use std::{
    fmt,
    sync::{Arc, RwLock},
};

use btc_integration::BtcBlockTip;

use super::CursorStorage;

#[derive(Clone)]
pub struct InMemoryCursorStorage {
    cursor: Arc<RwLock<BtcBlockTip>>,
}

impl InMemoryCursorStorage {
    pub fn new(initial_cursor: BtcBlockTip) -> Self {
        Self {
            cursor: Arc::new(RwLock::new(initial_cursor)),
        }
    }
}

impl CursorStorage for InMemoryCursorStorage {
    type Error = InMemoryCursorStorageError;

    async fn get_cursor(&self) -> Result<BtcBlockTip, Self::Error> {
        let cursor = self.cursor.read().map_err(|_| Self::Error::LockPoisoned)?;
        Ok(*cursor)
    }

    async fn store_cursor(&self, cursor: BtcBlockTip) -> Result<(), Self::Error> {
        let mut stored = self.cursor.write().map_err(|_| Self::Error::LockPoisoned)?;
        *stored = cursor;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryCursorStorageError {
    LockPoisoned,
}

impl fmt::Display for InMemoryCursorStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "cursor storage lock poisoned"),
        }
    }
}

impl std::error::Error for InMemoryCursorStorageError {}
