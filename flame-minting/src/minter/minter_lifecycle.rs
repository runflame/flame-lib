use crate::minter::minter_manager::{
    ShutdownError as MinterShutdownError, StartupError as MinterStartupError,
};
use crate::minter::sending::endorsement_sender::EndorsementSender;
use crate::minter::{MinterJournal, MinterManager};
use btc_integration::watcher::BlockId;
use flame_chain_service::CoreBlockSource;
use std::{fmt::Debug, future::Future};
use tokio::sync::watch;

pub trait MinterLifecycle {
    fn startup(
        &self,
        _subscription: watch::Receiver<Option<BlockId>>,
    ) -> impl Future<Output = Result<(), MinterStartupError>> + Send {
        async { unimplemented!() }
    }

    fn shutdown(&self) -> impl Future<Output = Result<(), MinterShutdownError>> + Send {
        async { unimplemented!() }
    }
}

impl<C, S, J> MinterLifecycle for MinterManager<C, S, J>
where
    C: CoreBlockSource + Send + Sync + 'static,
    C::Error: Debug + Send,
    S: EndorsementSender,
    S::Error: Debug,
    J: MinterJournal<TransactionId = S::TransactionId> + Send + Sync + 'static,
    J::Error: Debug + Send,
{
    async fn startup(
        &self,
        subscription: watch::Receiver<Option<BlockId>>,
    ) -> Result<(), MinterStartupError> {
        self.startup(subscription).await
    }

    async fn shutdown(&self) -> Result<(), MinterShutdownError> {
        self.shutdown().await
    }
}
