use btc_integration::{
    BtcBlockTip, HistoryError, HistoryUpdate, ProtocolIndexer, ShutdownError, StartupError,
    btc::rpc::ChainSource,
};
use std::{future::Future, num::NonZeroUsize};
use tokio::sync::watch;

pub trait ConsensusIndexer: Send + Sync + 'static {
    fn startup(&self) -> impl Future<Output = Result<(), StartupError>> + Send {
        async { unimplemented!() }
    }

    fn shutdown(&self) -> impl Future<Output = Result<(), ShutdownError>> + Send {
        async { unimplemented!() }
    }

    fn subscribe(&self) -> watch::Receiver<Option<BtcBlockTip>> {
        unimplemented!()
    }

    fn get_history(
        &self,
        _cursor: BtcBlockTip,
        _limit: NonZeroUsize,
    ) -> impl Future<Output = Result<HistoryUpdate, HistoryError>> + Send {
        async { unimplemented!() }
    }
}

impl<R: ChainSource + 'static> ConsensusIndexer for ProtocolIndexer<R> {
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
