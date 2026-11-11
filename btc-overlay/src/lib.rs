//! A fake Bitcoin Core for Flame nodes: real mainnet headers, simulated transactions.
//!
//! One overlay is shared by all nodes, which point their Bitcoin RPC URL at it, so they
//! all read the same blocks. A block's contents are fixed the first time it is connected
//! and kept per hash for good, across reorgs and restarts. Transactions sent with
//! `sendrawtransaction` go into the next new block. Each client can be given a lag, to see
//! new blocks later than the others (see [`lag`]).

pub mod blockbook;
mod descriptor;
pub mod headers;
pub mod lag;
pub mod overlay;
pub mod rpc;
pub mod state;

pub use overlay::{Overlay, SyncError, Tip};
