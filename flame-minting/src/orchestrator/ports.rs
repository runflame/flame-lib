use std::{future::Future, num::NonZeroUsize, sync::Arc};

use btc_integration::{
    BtcBlockTip, HistoryError, HistoryUpdate, IndexedBlock, ProtocolIndexer, ShutdownError,
    StartupError, btc::rpc::RpcApi,
};
use flame_chain_service::ChainAccess;
use flame_storage::{CanonicalStorage, ChainStorage};
use tokio::sync::watch;

use crate::consensus::{
    ConsensusStorage, MintingEngine, MintingEngineError, MintingJournal, MintingOutcome,
    MintingOutcomeApplier, MintingOutcomeApplierError, MintingProtocolParams,
};

pub trait ConsensusIndexer: Send + Sync + 'static {
    fn startup(&self) -> impl Future<Output = Result<(), StartupError>> + Send;
    fn shutdown(&self) -> impl Future<Output = Result<(), ShutdownError>> + Send;
    fn subscribe(&self) -> watch::Receiver<Option<BtcBlockTip>>;
    fn get_history(
        &self,
        cursor: BtcBlockTip,
        limit: NonZeroUsize,
    ) -> impl Future<Output = Result<HistoryUpdate, HistoryError>> + Send;
}

impl<R: RpcApi + 'static> ConsensusIndexer for ProtocolIndexer<R> {
    async fn startup(&self) -> Result<(), StartupError> {
        self.startup().await
    }

    async fn shutdown(&self) -> Result<(), ShutdownError> {
        self.shutdown().await
    }

    fn subscribe(&self) -> watch::Receiver<Option<BtcBlockTip>> {
        self.subscribe()
    }

    async fn get_history(
        &self,
        cursor: BtcBlockTip,
        limit: NonZeroUsize,
    ) -> Result<HistoryUpdate, HistoryError> {
        self.get_history(cursor, limit).await
    }
}

pub trait ConsensusEngine: Send + Sync + 'static {
    type Error: Send + 'static;

    fn get_minting_outcome(
        &self,
        block: IndexedBlock,
    ) -> impl Future<Output = Result<MintingOutcome, Self::Error>> + Send;
}

pub struct MintingEngineFactory<C, H, S> {
    pub protocol_params: Arc<MintingProtocolParams>,
    pub consensus_storage: Arc<C>,
    pub chain_storage: Arc<H>,
    pub canonical_storage: Arc<S>,
}

impl<C, H, S> ConsensusEngine for MintingEngineFactory<C, H, S>
where
    C: ConsensusStorage + Send + Sync + 'static,
    H: ChainStorage + Send + Sync + 'static,
    S: CanonicalStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H::Error: Send + 'static,
{
    type Error = MintingEngineError<C::Error, H::Error>;

    async fn get_minting_outcome(
        &self,
        block: IndexedBlock,
    ) -> Result<MintingOutcome, Self::Error> {
        MintingEngine::new(
            Arc::new(block),
            self.protocol_params.clone(),
            self.consensus_storage.clone(),
            self.chain_storage.clone(),
            self.canonical_storage.clone(),
        )
        .get_minting_outcome()
        .await
    }
}

pub trait ConsensusApplier: Send + 'static {
    type Error: Send + 'static;

    fn apply(
        &mut self,
        outcome: &MintingOutcome,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

impl<C, H, J, S> ConsensusApplier for MintingOutcomeApplier<C, H, J, S>
where
    C: ConsensusStorage + Send + Sync + 'static,
    H: ChainAccess + Send + Sync + 'static,
    J: MintingJournal + Send + Sync + 'static,
    S: CanonicalStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H::Error: Send + 'static,
    J::Error: Send + 'static,
    S::Error: Send + 'static,
{
    type Error = MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>;

    async fn apply(&mut self, outcome: &MintingOutcome) -> Result<(), Self::Error> {
        self.apply(outcome).await
    }
}
