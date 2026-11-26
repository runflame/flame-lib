#[cfg(feature = "memory-btc")]
mod memory_wallet;
mod rpc_client;
mod rpc_wallet;
mod rpc_watch_only_wallet;

#[cfg(feature = "memory-btc")]
pub use memory_wallet::MemoryWallet;
pub use rpc_wallet::RpcWallet;
pub use rpc_watch_only_wallet::RpcWatchOnlyWallet;
