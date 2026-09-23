use flame_chain_service::ChainAccess;
use flame_storage::ChainStorage;

use crate::consensus::ConsensusStorage;
use crate::core_block_notifier::CoreBlockNotifier;
use crate::minter::manager::{
    ShutdownError as MinterShutdownError, StartupError as MinterStartupError,
};
use crate::rewards::RewardsStorage;

pub use btc_integration::HistoryUpdate as BtcEvent;

pub mod consensus;
pub mod cursor_storage;
pub mod ports;

#[derive(Debug)]
pub enum OrchestratorError<C> {
    Consensus(C),
    MinterStartup(MinterStartupError),
    MinterShutdown(MinterShutdownError),
}

type OrchestratorResult<E, A, K, R, C, D> =
    Result<(), OrchestratorError<consensus::ConsensusLoopError<E, A, K, R, C, D>>>;

pub struct MintingOrchestrator<S, I, C, M, H: ChainAccess, N: CoreBlockNotifier> {
    pub btc_sender: S,
    pub btc_indexer: I,
    pub consensus_manager: C,
    pub minter_manager: M,
    pub chain_manager: H,
    pub core_block_notifier: N,
}

impl<S, I, E, A, K, R, C, D, M, H, N>
    MintingOrchestrator<S, std::sync::Arc<I>, consensus::ConsensusLoop<E, A, K, R, C, D>, M, H, N>
where
    I: ports::ConsensusIndexer,
    E: ports::ConsensusEngine,
    A: ports::ConsensusApplier,
    K: cursor_storage::CursorStorage,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    C: ChainStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    D: ConsensusStorage + Send + Sync + 'static,
    D::Error: Send + 'static,
    M: ports::MinterLifecycle,
    H: ChainAccess,
    N: CoreBlockNotifier,
{
    pub async fn startup(
        &self,
    ) -> OrchestratorResult<E::Error, A::Error, K::Error, R::Error, C::Error, D::Error> {
        self.consensus_manager
            .startup(self.btc_indexer.clone())
            .await
            .map_err(OrchestratorError::Consensus)?;
        self.minter_manager
            .startup(self.btc_indexer.subscribe())
            .await
            .map_err(OrchestratorError::MinterStartup)
    }

    pub async fn shutdown(
        &self,
    ) -> OrchestratorResult<E::Error, A::Error, K::Error, R::Error, C::Error, D::Error> {
        self.minter_manager
            .shutdown()
            .await
            .map_err(OrchestratorError::MinterShutdown)?;
        self.consensus_manager
            .shutdown()
            .await
            .map_err(OrchestratorError::Consensus)
    }
}
