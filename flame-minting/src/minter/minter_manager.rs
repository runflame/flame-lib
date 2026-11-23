use super::{
    EndorsementPolicy, MinterJournal, endorser::Endorser, minter_worker::MinterWorker,
    sending::endorsement_sender::EndorsementSender,
};
use btc_integration::watcher::BlockId;
use flame_chain_service::CoreBlockSource;
use std::{fmt::Debug, sync::Arc};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum StartupError {
    AlreadyRunning,
}

#[derive(Debug)]
pub enum ShutdownError {
    WorkerJoin(tokio::task::JoinError),
}

struct MinterWorkerHandle {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for MinterWorkerHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub struct MinterManager<C, S, J> {
    endorsement_policy: EndorsementPolicy,
    core_block_source: Arc<C>,
    endorser: Arc<Endorser<S, J>>,
    worker: Mutex<Option<MinterWorkerHandle>>,
}

impl<C, S, J> MinterManager<C, S, J>
where
    C: CoreBlockSource + Send + Sync + 'static,
    C::Error: Debug + Send,
    S: EndorsementSender,
    S::Error: Debug,
    J: MinterJournal<TransactionId = S::TransactionId> + Send + Sync + 'static,
    J::Error: Debug + Send,
{
    pub fn new(
        core_block_source: Arc<C>,
        sender: Arc<S>,
        journal: Arc<J>,
        endorsement_policy: EndorsementPolicy,
    ) -> Self {
        Self {
            endorsement_policy,
            core_block_source,
            endorser: Arc::new(Endorser::new(sender, journal)),
            worker: Mutex::new(None),
        }
    }

    pub async fn startup(
        &self,
        subscription: watch::Receiver<Option<BlockId>>,
    ) -> Result<(), StartupError> {
        if self.endorsement_policy == EndorsementPolicy::Manual {
            return Ok(());
        }

        let mut slot = self.worker.lock().await;
        if slot.is_some() {
            return Err(StartupError::AlreadyRunning);
        }

        let cancellation = CancellationToken::new();
        let worker = MinterWorker::new(
            self.core_block_source.clone(),
            self.endorser.clone(),
            subscription,
            cancellation.clone(),
        );
        let task = tokio::spawn(worker.run());
        *slot = Some(MinterWorkerHandle { cancellation, task });
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), ShutdownError> {
        let mut slot = self.worker.lock().await;
        let Some(worker) = slot.as_mut() else {
            return Ok(());
        };

        worker.cancellation.cancel();
        let result = (&mut worker.task).await.map_err(ShutdownError::WorkerJoin);
        *slot = None;
        result
    }
}

#[cfg(test)]
mod tests;
