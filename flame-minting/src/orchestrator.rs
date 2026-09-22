use flame_chain_service::ChainAccess;

use crate::core_block_notifier::CoreBlockNotifier;

pub use btc_integration::HistoryUpdate as BtcEvent;

pub mod consensus;
pub mod cursor_storage;
pub mod ports;

pub struct MintingOrchestrator<S, I, C, M, H: ChainAccess, N: CoreBlockNotifier> {
    pub btc_sender: S,
    pub btc_indexer: I,
    pub consensus_manager: C,
    pub minter_manager: M,
    pub chain_manager: H,
    pub core_block_notifier: N,
}

impl<S, I, E, A, K, M, H, N>
    MintingOrchestrator<S, std::sync::Arc<I>, consensus::ConsensusLoop<E, A, K>, M, H, N>
where
    I: ports::ConsensusIndexer,
    E: ports::ConsensusEngine,
    A: ports::ConsensusApplier,
    K: cursor_storage::CursorStorage,
    H: ChainAccess,
    N: CoreBlockNotifier,
{
    pub async fn startup(
        &self,
    ) -> Result<(), consensus::ConsensusLoopError<E::Error, A::Error, K::Error>> {
        self.consensus_manager
            .startup(self.btc_indexer.clone())
            .await
    }

    pub async fn shutdown(
        &self,
    ) -> Result<(), consensus::ConsensusLoopError<E::Error, A::Error, K::Error>> {
        self.consensus_manager.shutdown().await
    }
}
