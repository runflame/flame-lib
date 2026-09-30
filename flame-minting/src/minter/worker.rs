use std::{fmt::Debug, sync::Arc};

use btc_integration::BtcBlockTip;
use flame_chain_service::CoreBlockSource;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{MinterJournal, ports::VoteSender, voter::Voter};

pub(super) struct MinterWorker<C, S, J> {
    core_block_source: Arc<C>,
    voter: Arc<Voter<S, J>>,
    subscription: watch::Receiver<Option<BtcBlockTip>>,
    cancellation: CancellationToken,
}

impl<C, S, J> MinterWorker<C, S, J>
where
    C: CoreBlockSource + Send + Sync,
    C::Error: Debug + Send,
    S: VoteSender,
    S::Error: Debug,
    J: MinterJournal<TransactionId = S::TransactionId> + Send + Sync,
    J::Error: Debug + Send,
{
    pub fn new(
        core_block_source: Arc<C>,
        voter: Arc<Voter<S, J>>,
        subscription: watch::Receiver<Option<BtcBlockTip>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            core_block_source,
            voter,
            subscription,
            cancellation,
        }
    }

    pub async fn run(mut self) {
        let mut last_tip = None;
        loop {
            if self.cancellation.is_cancelled() {
                return;
            }
            let tip = *self.subscription.borrow_and_update();
            if let Some(tip) = tip.filter(|tip| Some(*tip) != last_tip) {
                last_tip = Some(tip);
                self.process_tip(tip).await;
            }
            tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => return,
                result = self.subscription.changed() => {
                    if result.is_err() {
                        log::warn!("minter BTC indexer subscription closed");
                        return;
                    }
                }
            }
        }
    }

    async fn process_tip(&self, tip: BtcBlockTip) {
        let Some(target_height) = tip.height.checked_add(1) else {
            log::error!("minter target BTC height overflow");
            return;
        };
        let result = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return,
            result = self.core_block_source.get_core_block(target_height) => result,
        };
        let block = match result {
            Ok(Some(block)) => block,
            Ok(None) => {
                log::info!("No core block for the BTC target height {target_height}");
                return;
            }
            Err(error) => {
                log::error!(
                    "failed to load core block for target BTC height {target_height}: {error:?}"
                );
                return;
            }
        };
        if self.cancellation.is_cancelled() || *self.subscription.borrow() != Some(tip) {
            return;
        }
        if !block
            .header
            .core_block
            .as_ref()
            .is_some_and(|core| u64::from(core.target_btc_height) == target_height)
        {
            log::error!(
                "core block source returned an invalid block for target BTC height {target_height}"
            );
            return;
        }
        if let Err(error) = self.voter.vote(block).await {
            log::error!("failed to vote for target BTC height {target_height}: {error:?}");
        }
    }
}
