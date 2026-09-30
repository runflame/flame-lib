use std::sync::Arc;

use futures_util::{StreamExt, TryStreamExt, stream};
use tokio::sync::Semaphore;

use super::{
    error::{HistoryError, history_rpc_error},
    recent_block_cache::RecentBlockCache,
    types::IndexedBlock,
};
use crate::{
    btc::{
        bitcoin_facade::BitcoinFacade,
        rpc::{BtcBlockTip, BtcTransactionWithPrevouts, RpcApi},
    },
    protocol::{Acquisition, validate_transaction_votes},
};

const PARALLEL_LOADS: usize = 4;

pub(super) struct IndexedBlockSource<R> {
    bitcoin: Arc<BitcoinFacade<R>>,
    cache: RecentBlockCache,
    loads: Semaphore,
}

impl<R: RpcApi> IndexedBlockSource<R> {
    pub fn new(bitcoin: Arc<BitcoinFacade<R>>) -> Self {
        Self {
            bitcoin,
            cache: RecentBlockCache::default(),
            loads: Semaphore::new(PARALLEL_LOADS),
        }
    }

    pub async fn load(&self, tip: BtcBlockTip) -> Result<IndexedBlock, HistoryError> {
        if let Some(block) = self.cache.get(tip.hash) {
            return Ok(block);
        }
        let _permit = self
            .loads
            .acquire()
            .await
            .expect("load semaphore is never closed");
        // Another request may have populated the cache while this one waited for a slot.
        if let Some(block) = self.cache.get(tip.hash) {
            return Ok(block);
        }
        let transactions = self
            .bitcoin
            .transactions_with_prevouts_in_block(tip.hash)
            .await
            .map_err(history_rpc_error)?;
        let block = index_transactions(tip, transactions)?;
        self.cache.insert_if_current(block.clone());
        Ok(block)
    }

    pub async fn load_many(&self, tips: &[BtcBlockTip]) -> Result<Vec<IndexedBlock>, HistoryError> {
        stream::iter(tips.iter().copied().map(|tip| self.load(tip)))
            .buffered(PARALLEL_LOADS)
            .try_collect()
            .await
    }

    /// Only the tip observer updates the window; historical reads cannot move it backwards.
    pub async fn observe_tip(&self, tip: BtcBlockTip) -> Result<(), HistoryError> {
        let window = self
            .bitcoin
            .recent_block_tips(tip, RecentBlockCache::CAPACITY)
            .await
            .map_err(history_rpc_error)?;
        if !self.bitcoin.is_canonical(tip).await? {
            return Err(HistoryError::ChainChanged);
        }
        self.cache.replace_window(window);
        Ok(())
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
    }
}

fn index_transactions(
    tip: BtcBlockTip,
    transactions: Vec<BtcTransactionWithPrevouts>,
) -> Result<IndexedBlock, HistoryError> {
    let mut block = IndexedBlock {
        btc_block_tip: tip,
        acquisitions: Vec::new(),
        votes: Vec::new(),
    };
    for transaction in transactions {
        block
            .acquisitions
            .extend(Acquisition::from_tx(&transaction.transaction));
        block
            .votes
            .extend(validate_transaction_votes(&transaction)?);
    }
    Ok(block)
}

#[cfg(test)]
#[path = "unit-tests/indexed_block_source_tests.rs"]
mod tests;
