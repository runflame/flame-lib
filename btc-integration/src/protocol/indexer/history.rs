use std::{num::NonZeroUsize, sync::Arc};

use super::{
    bitcoin_chain_update_planner::{BitcoinChainUpdatePlanner, ChainUpdatePlan},
    error::HistoryError,
    indexed_block_source::IndexedBlockSource,
    types::{HistoryChange, HistoryUpdate, IndexedBlock, MAX_HISTORY_BLOCKS},
};
use crate::btc::{
    bitcoin_facade::BitcoinFacade,
    rpc::{BtcBlockTip, RpcApi},
};

pub(super) struct HistoryReader<R> {
    bitcoin: Arc<BitcoinFacade<R>>,
    planner: BitcoinChainUpdatePlanner<R>,
    blocks: Arc<IndexedBlockSource<R>>,
}

impl<R: RpcApi> HistoryReader<R> {
    pub fn new(bitcoin: Arc<BitcoinFacade<R>>, blocks: Arc<IndexedBlockSource<R>>) -> Self {
        Self {
            planner: BitcoinChainUpdatePlanner::new(Arc::clone(&bitcoin)),
            bitcoin,
            blocks,
        }
    }

    pub async fn get_history(
        &self,
        cursor: BtcBlockTip,
        max_blocks: NonZeroUsize,
    ) -> Result<HistoryUpdate, HistoryError> {
        if max_blocks.get() > MAX_HISTORY_BLOCKS {
            return Err(HistoryError::InvalidLimit);
        }
        let target = self.bitcoin.best_block_tip().await?;
        let plan = self
            .planner
            .plan_update(cursor, target, max_blocks.get())
            .await?;
        let blocks = self.blocks.load_many(&plan.new_blocks).await?;
        if !self.bitcoin.is_canonical(target).await? {
            return Err(HistoryError::ChainChanged);
        }
        Ok(build_update(target, plan, blocks))
    }
}

fn build_update(
    target_tip: BtcBlockTip,
    plan: ChainUpdatePlan,
    new_blocks: Vec<IndexedBlock>,
) -> HistoryUpdate {
    let next_cursor = new_blocks
        .last()
        .map_or(plan.common_ancestor, |block| block.btc_block_tip);
    let change = if plan.removed_block_tips.is_empty() {
        HistoryChange::Extension { new_blocks }
    } else {
        HistoryChange::Reorg {
            removed_block_tips: plan.removed_block_tips,
            new_blocks,
        }
    };
    HistoryUpdate {
        target_tip,
        change,
        next_cursor,
    }
}

#[cfg(test)]
#[path = "unit-tests/history_tests.rs"]
mod tests;
