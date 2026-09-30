use std::{cmp::Reverse, collections::BTreeMap};

use flamechain::BlockTip;

use crate::consensus::WeightedBlockHeader;

use super::InMemoryConsensusStorageError;

#[derive(Default)]
pub(super) struct WeightedHeaderStorage {
    headers: BTreeMap<BlockTip, WeightedBlockHeader>,
}

impl WeightedHeaderStorage {
    pub(super) fn store_cumulative_weight(
        &mut self,
        tip: BlockTip,
        weighted_block: &WeightedBlockHeader,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if tip != weighted_block.header.block_tip() {
            return Err(InMemoryConsensusStorageError::BlockTipMismatch);
        }
        self.headers.insert(tip, weighted_block.clone());
        Ok(())
    }

    pub(super) fn get_cumulative_weight(&self, tip: BlockTip) -> Option<WeightedBlockHeader> {
        self.headers.get(&tip).cloned()
    }

    pub(super) fn get_block_tip_with_most_weight(&self) -> Option<BlockTip> {
        self.headers
            .iter()
            .max_by_key(|(tip, weighted)| (chain_weight(weighted), Reverse(tip.hash)))
            .map(|(tip, _)| *tip)
    }
}

fn chain_weight(weighted: &WeightedBlockHeader) -> u128 {
    let block_weight = if weighted.header.core_block.is_some() {
        weighted.effective_power.checked_ilog2().unwrap_or(0)
    } else {
        0
    };
    u128::from(weighted.parent_weight) + u128::from(block_weight)
}
