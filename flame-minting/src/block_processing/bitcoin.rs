pub mod bitcoin_block_processor;
pub mod block_processor;
pub mod consensus_loop;
pub mod cursor_storage;
pub mod in_memory_cursor_storage;

pub use bitcoin_block_processor::{BitcoinBlockProcessor, BlockProcessingError};
pub use block_processor::BlockProcessor;
pub use btc_integration::BtcBlockRecords;
pub use cursor_storage::CursorStorage;
