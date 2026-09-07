use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{oneshot, watch},
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use super::{error::HistoryError, indexed_block_source::IndexedBlockSource};
use crate::btc::{
    bitcoin_facade::BitcoinFacade,
    rpc::{BtcBlockTip, RpcApi},
};

pub(super) struct IndexerWorker<R> {
    rpc: Arc<BitcoinFacade<R>>,
    blocks: Arc<IndexedBlockSource<R>>,
    subscribers: watch::Sender<Option<BtcBlockTip>>,
    cancellation: CancellationToken,
}

impl<R: RpcApi> IndexerWorker<R> {
    pub fn new(
        rpc: Arc<BitcoinFacade<R>>,
        blocks: Arc<IndexedBlockSource<R>>,
        subscribers: watch::Sender<Option<BtcBlockTip>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            rpc,
            blocks,
            subscribers,
            cancellation,
        }
    }

    pub async fn run(self, bootstrap: oneshot::Sender<()>) {
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => {},
            _ = self.observe(bootstrap) => {},
        }
    }

    async fn observe(&self, bootstrap: oneshot::Sender<()>) {
        let mut bootstrap = Some(bootstrap);
        let mut previous = None;
        loop {
            let result = async {
                if let Some(tip) = previous {
                    self.rpc.wait_for_next_block(tip).await?;
                }
                // Long-poll announcements may already be stale by the time they are read.
                let tip = self.rpc.best_block_tip().await?;
                if previous != Some(tip) {
                    self.blocks.observe_tip(tip).await?;
                    self.subscribers.send_replace(Some(tip));
                    previous = Some(tip);
                    if let Some(bootstrap) = bootstrap.take() {
                        let _ = bootstrap.send(());
                    }
                }
                Ok::<_, HistoryError>(())
            }
            .await;
            if let Err(error) = result {
                log::error!("protocol indexer observation failed: {error}");
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
