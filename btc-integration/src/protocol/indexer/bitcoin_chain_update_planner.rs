use super::error::{HistoryError, history_rpc_error};
use crate::btc::rpc::BtcBlockHeaderInfo;
use crate::btc::{
    bitcoin_facade::BitcoinFacade,
    rpc::{BtcBlockTip, RpcApi, rpc_error_code},
};
use corepc_client::bitcoin::BlockHash;
use corepc_client::client_sync::Error as RpcError;
use std::{collections::VecDeque, sync::Arc};

pub(super) struct ChainUpdatePlan {
    pub common_ancestor: BtcBlockTip,
    pub removed_block_tips: Vec<BtcBlockTip>,
    pub new_blocks: Vec<BtcBlockTip>,
}

pub(super) struct BitcoinChainUpdatePlanner<R> {
    rpc: Arc<BitcoinFacade<R>>,
}

impl<R: RpcApi> BitcoinChainUpdatePlanner<R> {
    pub fn new(rpc: Arc<BitcoinFacade<R>>) -> Self {
        Self { rpc }
    }

    pub async fn plan_update(
        &self,
        cursor: BtcBlockTip,
        target: BtcBlockTip,
        limit: usize,
    ) -> Result<ChainUpdatePlan, HistoryError> {
        let mut old = self
            .rpc
            .block_header_info(cursor.hash)
            .await
            .map_err(|error| {
                if rpc_error_code(&error) == Some(-5) {
                    HistoryError::UnknownCursor(cursor)
                } else {
                    HistoryError::Rpc(error)
                }
            })?;
        if old.tip != cursor {
            return Err(HistoryError::InvalidCursor(cursor));
        }
        let mut new = self
            .rpc
            .block_header_info(target.hash)
            .await
            .map_err(history_rpc_error)?;
        if new.tip != target {
            return Err(HistoryError::Rpc(RpcError::UnexpectedStructure));
        }
        let mut removed_block_tips = Vec::new();
        let mut new_blocks = VecDeque::with_capacity(limit);

        // Walk old chain backwards until it is equal to the new tip height. Situation in which
        // this code can be executed is probably never going to happen, as bitcoin reorg to the
        // lower height is almost impossible. Though, it is possible according to BTC implementation.
        while old.tip.height > new.tip.height {
            removed_block_tips.push(old.tip);
            old = self.previous_header(old).await?;
        }

        // Walk new chain backwards until its height is equal to the height of the old tip.
        while new.tip.height > old.tip.height {
            retain_page(&mut new_blocks, new.tip, limit);
            new = self.previous_header(new).await?;
        }

        // If both blocks have same height but different hash, it means we are still looking at
        // fork, so walk backwards until hash is equal.
        while old.tip.hash != new.tip.hash {
            removed_block_tips.push(old.tip);
            retain_page(&mut new_blocks, new.tip, limit);
            old = self.previous_header(old).await?;
            new = self.previous_header(new).await?;
        }

        Ok(ChainUpdatePlan {
            common_ancestor: old.tip,
            removed_block_tips,
            new_blocks: new_blocks.into(),
        })
    }

    async fn previous_header(
        &self,
        header: BtcBlockHeaderInfo,
    ) -> Result<BtcBlockHeaderInfo, HistoryError> {
        self.rpc
            .previous_header(header)
            .await
            .map_err(history_rpc_error)
    }
}

fn retain_page(page: &mut VecDeque<BtcBlockTip>, tip: BtcBlockTip, limit: usize) {
    if page.len() == limit {
        page.pop_back();
    }
    page.push_front(tip);
}
