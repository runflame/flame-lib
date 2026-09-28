use std::future::Future;

use btc_integration::BtcBlockTip;

pub mod in_memory;

pub use in_memory::{InMemoryCursorStorage, InMemoryCursorStorageError};

pub trait CursorStorage: Send + Sync + 'static {
    type Error: Send + 'static;

    fn get_cursor(&self) -> impl Future<Output = Result<BtcBlockTip, Self::Error>> + Send;

    fn store_cursor(
        &self,
        cursor: BtcBlockTip,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
