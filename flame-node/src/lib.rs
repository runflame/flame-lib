use flame_chain_service::ChainAccess;
use flame_minting::{MintingOrchestrator, core_block_notifier::CoreBlockNotifier};

pub struct Node<S, I, C, M, H: ChainAccess, N: CoreBlockNotifier> {
    pub consensus: MintingOrchestrator<S, I, C, M, H, N>,
}
