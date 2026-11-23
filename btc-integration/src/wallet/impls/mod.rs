//! The implementations of the wallets.
//!
//! | Implementation | Trait | What it is |
//! |---|---|---|
//! | [`RpcWallet`] | [`BtcWallet`](crate::wallet::BtcWallet) | an ordinary wallet of Bitcoin Core, over RPC |
//! | [`RpcWatchOnlyWallet`] | [`WatchOnlyWallet`](crate::wallet::WatchOnlyWallet) | the wallet of Bitcoin Core that watches the script of a minter, over RPC |
//! | `MemoryWallet` | both | a wallet over Bitcoin in memory, for tests and for a devnet without bitcoind; needs the `memory-btc` feature |

#[cfg(feature = "memory-btc")]
mod memory_wallet;
mod rpc_client;
mod rpc_wallet;
mod rpc_watch_only_wallet;

#[cfg(feature = "memory-btc")]
pub use memory_wallet::MemoryWallet;
pub use rpc_wallet::RpcWallet;
pub use rpc_watch_only_wallet::RpcWatchOnlyWallet;
