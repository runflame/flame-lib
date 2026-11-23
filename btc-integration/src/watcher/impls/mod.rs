//! The implementations of the traits of this crate.
//!
//! | Implementation | Trait | What it is |
//! |---|---|---|
//! | [`MemoryStorage`] | [`BtcStorage`](crate::watcher::BtcStorage) | every block it was given, in memory |
//! | [`RecentBlocks`] | [`BtcStorage`](crate::watcher::BtcStorage) | the latest blocks only, in memory; the rest stays in Bitcoin Core |
//! | `MemorySource` | [`ChainSource`](crate::watcher::ChainSource) | Bitcoin in memory (`MemoryBtc`), for tests and for a devnet without bitcoind; needs the `memory-btc` feature |
//! | [`RpcSource`] | [`ChainSource`](crate::watcher::ChainSource) | RPC to Bitcoin Core |
//!
//! The rest of the crate works through the traits and does not name any of
//! these types.

#[cfg(feature = "memory-btc")]
mod memory_source;
mod memory_storage;
mod recent_blocks;
mod rpc_source;

#[cfg(feature = "memory-btc")]
pub use memory_source::{Lag, MemoryBtc, MemorySource, Rejected};
pub use memory_storage::MemoryStorage;
pub use recent_blocks::{DEFAULT_RECENT_BLOCKS, RecentBlocks};
pub use rpc_source::RpcSource;
