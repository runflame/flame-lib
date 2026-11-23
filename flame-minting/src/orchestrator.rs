pub mod core_block_notifier;
mod defaults;
mod minting_orchestrator;

pub use btc_integration::watcher::BtcEvent;
pub use defaults::DefaultConsensusLoop;
pub use minting_orchestrator::{MintingOrchestrator, OrchestratorError};

pub use crate::block_processing::bitcoin::consensus_loop as consensus;
pub use crate::block_processing::bitcoin::cursor_storage;
pub use crate::minter::sending::minting_sender as sender;

pub mod ports {
    pub use crate::minter::minter_lifecycle::MinterLifecycle;
    pub use crate::minter::sending::sender_lifecycle::SenderLifecycle;
}
