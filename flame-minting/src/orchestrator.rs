use std::{fmt::Debug, sync::Arc};

use btc_integration::btc::rpc::Core31RpcApi;
use btc_integration::{
    BitcoinConfig, BitcoinFacade, BtcBlockTip, IdentityConfig, IdentityError, IdentityManager,
    ProtocolIndexerV31,
    identity::storage::{InMemorySecretStorage, MissingSecret},
};
use flame_chain_service::{ChainAccess, CoreBlockSource};
use flame_storage::ChainStorage;

use crate::consensus::ConsensusStorage;
use crate::core_block_notifier::CoreBlockNotifier;
use crate::minter::manager::{
    ShutdownError as MinterShutdownError, StartupError as MinterStartupError,
};
use crate::minter::{MinterManager, journal::in_memory::InMemoryMinterJournal};
use crate::rewards::RewardsStorage;

pub use btc_integration::HistoryUpdate as BtcEvent;

pub mod consensus;
pub mod cursor_storage;
mod defaults;
pub mod ports;
pub mod sender;

pub use defaults::DefaultConsensusLoop;
use sender::{MintingSender, SenderStartupError};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub enum OrchestratorError<C> {
    Identity(IdentityError<MissingSecret>),
    SenderStartup(SenderStartupError),
    Consensus(C),
    MinterStartup(MinterStartupError),
    MinterShutdown(MinterShutdownError),
}

type OrchestratorResult<E, A, K, R, C, D> =
    Result<(), OrchestratorError<consensus::ConsensusLoopError<E, A, K, R, C, D>>>;

pub struct MintingOrchestrator<S, I, C, M, H: ChainAccess, N: CoreBlockNotifier> {
    pub identity_manager: Arc<IdentityManager>,
    pub btc_sender: S,
    pub btc_indexer: I,
    pub consensus_manager: C,
    pub minter_manager: M,
    pub chain_manager: H,
    pub core_block_notifier: N,
}

impl<B, H, N>
    MintingOrchestrator<
        Arc<MintingSender>,
        Arc<ProtocolIndexerV31>,
        DefaultConsensusLoop<H>,
        MinterManager<B, MintingSender, InMemoryMinterJournal>,
        H,
        N,
    >
where
    B: CoreBlockSource + Send + Sync + 'static,
    B::Error: Debug + Send,
    H: ChainAccess + Clone + Send + Sync + 'static,
    H::Error: Send + 'static,
    N: CoreBlockNotifier,
{
    pub fn new(
        core_block_notifier: N,
        core_block_source: B,
        chain_manager: H,
        bitcoin_config: BitcoinConfig,
        initial_btc_cursor: BtcBlockTip,
        identity_config: IdentityConfig,
    ) -> Result<Self, corepc_client::client_sync::Error> {
        let rpc = Arc::new(Core31RpcApi::new(
            &bitcoin_config.node_rpc_url,
            bitcoin_config.auth.clone(),
        )?);
        let btc_indexer = Arc::new(ProtocolIndexerV31::new(Arc::new(BitcoinFacade::new(rpc))));
        let identity_manager = Arc::new(IdentityManager::new(
            Arc::new(InMemorySecretStorage::default()),
            identity_config,
        ));
        let btc_sender = Arc::new(MintingSender::new(bitcoin_config, identity_manager.clone()));
        let consensus_manager = defaults::consensus_loop(chain_manager.clone(), initial_btc_cursor);
        let minter_manager = MinterManager::new(
            Arc::new(core_block_source),
            btc_sender.clone(),
            Arc::new(InMemoryMinterJournal::new()),
        );

        Ok(Self {
            identity_manager,
            btc_sender,
            btc_indexer,
            consensus_manager,
            minter_manager,
            chain_manager,
            core_block_notifier,
        })
    }
}

impl<S, I, E, A, K, R, C, D, M, H, N>
    MintingOrchestrator<S, std::sync::Arc<I>, consensus::ConsensusLoop<E, A, K, R, C, D>, M, H, N>
where
    S: ports::SenderLifecycle,
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
        self.identity_manager
            .startup()
            .await
            .map_err(OrchestratorError::Identity)?;
        self.btc_sender
            .startup()
            .await
            .map_err(OrchestratorError::SenderStartup)?;
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
