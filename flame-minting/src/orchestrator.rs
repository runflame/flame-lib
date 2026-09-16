use flame_chain_service::ChainAccess;

use crate::core_block_notifier::CoreBlockNotifier;

pub use btc_integration::HistoryUpdate as BtcEvent;

pub struct MintingOrchestrator<S, I, C, M, H: ChainAccess, N: CoreBlockNotifier> {
    pub btc_sender: S,
    pub btc_indexer: I,
    pub consensus_manager: C,
    pub minter_manager: M,
    pub chain_manager: H,
    pub core_block_notifier: N,
}
