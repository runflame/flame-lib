use super::{DefaultConsensusLoop, defaults};
use crate::ConsensusHandle;
use crate::block_processing::bitcoin::consensus_loop as consensus;
use crate::block_processing::bitcoin::in_memory_cursor_storage::InMemoryCursorStorage;
use crate::block_processing::{
    BlockProcessor, ConsensusManagerError, FlameProcessor, journal::consensus::ConsensusJournal,
};
use crate::consensus::storage::InMemoryConsensusStorage;
use crate::minter::minter_lifecycle::MinterLifecycle;
use crate::minter::minter_manager::{
    ShutdownError as MinterShutdownError, StartupError as MinterStartupError,
};
use crate::minter::sending::minting_sender::{MintingSender, SenderStartupError};
use crate::minter::sending::sender_lifecycle::SenderLifecycle;
use crate::minter::{EndorsementPolicy, MinterManager, journal::in_memory::InMemoryMinterJournal};
use crate::orchestrator::core_block_notifier::CoreBlockNotifier;
use btc_integration::{
    BitcoinConfig, IdentityConfig, MinterIdentity,
    identity::impls::LocalKey,
    watcher::{
        BlockId, ChainSource, SourceError,
        impls::{RecentBlocks, RpcSource},
    },
};
use flame_chain_service::{ChainAccess, CoreBlockSource};
use flame_storage::{chain::InMemoryChainStorage, state::canonical::InMemoryCanonicalStorage};
use std::{fmt::Debug, sync::Arc};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub enum OrchestratorError<C> {
    SenderStartup(SenderStartupError),
    Consensus(C),
    MinterStartup(MinterStartupError),
    MinterShutdown(MinterShutdownError),
}

type OrchestratorResult<E> = Result<(), OrchestratorError<consensus::ConsensusLoopError<E>>>;

pub struct MintingOrchestrator<S, I, C, M, H: ChainAccess, N: CoreBlockNotifier> {
    pub identity: MinterIdentity,
    pub btc_sender: S,
    pub btc_source: I,
    pub consensus_manager: C,
    pub minter_manager: M,
    pub chain_manager: H,
    pub core_block_notifier: N,
}

impl<B, H, N>
    MintingOrchestrator<
        Arc<MintingSender>,
        Arc<RpcSource>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core_block_notifier: N,
        core_block_source: B,
        chain_manager: H,
        bitcoin_config: BitcoinConfig,
        initial_btc_cursor: BlockId,
        identity_config: IdentityConfig,
        identity_key: LocalKey,
        endorsement_policy: EndorsementPolicy,
    ) -> Result<Self, SourceError> {
        let btc_source = Arc::new(RpcSource::new(&bitcoin_config)?);
        let identity = identity_config.identity(&identity_key);
        let btc_sender = Arc::new(MintingSender::new(
            bitcoin_config,
            identity.clone(),
            Arc::new(identity_key),
        ));
        let consensus_manager = defaults::consensus_loop(chain_manager.clone(), initial_btc_cursor);
        let minter_manager = MinterManager::new(
            Arc::new(core_block_source),
            btc_sender.clone(),
            Arc::new(InMemoryMinterJournal::new()),
            endorsement_policy,
        );

        Ok(Self {
            identity,
            btc_sender,
            btc_source,
            consensus_manager,
            minter_manager,
            chain_manager,
            core_block_notifier,
        })
    }

    pub fn get_consensus_storage(&self) -> &InMemoryConsensusStorage {
        let processor = self.consensus_manager.manager().bitcoin_processor();
        &processor.engine.consensus_storage
    }

    pub fn get_chain_storage(&self) -> &InMemoryChainStorage {
        let processor = self.consensus_manager.manager().bitcoin_processor();
        &processor.engine.chain_storage
    }

    pub fn get_canonical_storage(&self) -> &InMemoryCanonicalStorage {
        let processor = self.consensus_manager.manager().bitcoin_processor();
        &processor.engine.canonical_storage
    }

    pub fn get_cursor_storage(&self) -> &InMemoryCursorStorage {
        let processor = self.consensus_manager.manager().bitcoin_processor();
        &processor.cursor_storage
    }
}

impl<S, I, B, F, J, M, H, N> MintingOrchestrator<S, I, consensus::ConsensusLoop<B, F, J>, M, H, N>
where
    B: BlockProcessor,
    F: FlameProcessor,
    J: ConsensusJournal,
    H: ChainAccess,
    N: CoreBlockNotifier,
{
    pub fn consensus_handle(&self) -> ConsensusHandle<B, F, J> {
        self.consensus_manager.consensus_handle()
    }
}

impl<S, I, B, F, J, M, H, N>
    MintingOrchestrator<S, std::sync::Arc<I>, consensus::ConsensusLoop<B, F, J>, M, H, N>
where
    S: SenderLifecycle,
    I: ChainSource + 'static,
    B: BlockProcessor,
    F: FlameProcessor,
    J: ConsensusJournal,
    M: MinterLifecycle,
    H: ChainAccess,
    N: CoreBlockNotifier,
{
    pub async fn startup(
        &self,
    ) -> OrchestratorResult<ConsensusManagerError<B::Error, F::Error, J::Error>> {
        self.btc_sender
            .startup()
            .await
            .map_err(OrchestratorError::SenderStartup)?;
        self.consensus_manager
            .startup(self.btc_source.clone(), RecentBlocks::default())
            .await
            .map_err(OrchestratorError::Consensus)?;
        if let Err(error) = self
            .minter_manager
            .startup(self.consensus_manager.subscribe())
            .await
        {
            self.consensus_manager
                .shutdown()
                .await
                .map_err(OrchestratorError::Consensus)?;
            return Err(OrchestratorError::MinterStartup(error));
        }
        Ok(())
    }

    pub async fn shutdown(
        &self,
    ) -> OrchestratorResult<ConsensusManagerError<B::Error, F::Error, J::Error>> {
        let (minter, consensus) = tokio::join!(
            self.minter_manager.shutdown(),
            self.consensus_manager.shutdown(),
        );
        minter
            .map_err(OrchestratorError::MinterShutdown)
            .and(consensus.map_err(OrchestratorError::Consensus))
    }
}
