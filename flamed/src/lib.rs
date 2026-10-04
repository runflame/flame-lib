//! `flamed`, the Flame node.
//!
//! One archival node. It opens a chain from a derived genesis, keeps a
//! current Utreexo proof for every unspent contract, indexes history by
//! contract and by predicate, archives every block it accepts, and answers
//! wallets over JSON-RPC 2.0.
//!
//! Everything that exists only because minting and consensus are not
//! designed yet — genesis allocations and the interval minter — is behind
//! the `devnet` feature. A build without it is an honest node: it can open
//! and serve a chain, and it can make neither blocks nor funds.
//!
//! The devnet items are gated `#[cfg(any(test, feature = "devnet"))]`, not
//! on the feature alone, so this crate's own tests compile them under a
//! plain `cargo test`. The binary is the one exception, and `main.rs` says
//! why.

pub mod cells;
pub mod config;
pub mod genesis;
pub mod index;
mod inspect;
#[cfg(any(test, feature = "devnet"))]
pub mod minter;
pub mod node;
pub mod rpc;
pub mod store;
pub mod utxos;

pub use config::{ChainParamsFile, GenesisFile, NodeConfig};
pub use node::{Node, NodeError, ProofStatus, SharedNode, TipInfo, TxStatus};
pub use rpc::{serve, FlamedRpc};
pub use store::BlockStore;

#[cfg(any(test, feature = "devnet"))]
pub use minter::Minter;

#[cfg(test)]
mod tests;
