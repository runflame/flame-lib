use std::{collections::BTreeMap, ops::RangeInclusive, sync::Arc};

use btc_integration::BtcBlockTip;
use flame_storage::ChainStorage;
use flamechain::{BlockHeader, BlockTip, CoreFlameHeight};

use crate::consensus::{ConsensusStorage, MintingProtocolParams};

use self::block_weight_calculator::{
    BlockWeightCalculator, BlockWeightCalculatorError, PredicatePair,
};
use self::reward_calculation::BlockRewardCalculator;
use super::{PendingReward, RewardsOutcome};

pub mod block_weight_calculator;
pub mod reward_calculation;

#[cfg(test)]
pub(crate) mod tests;

type CycleRewards = BTreeMap<PredicatePair, PendingReward>;

pub struct RewardsEngine<H: ChainStorage, C: ConsensusStorage> {
    pub chain_storage: Arc<H>,
    pub consensus_storage: Arc<C>,
    pub protocol_params: Arc<MintingProtocolParams>,
}

impl<H: ChainStorage, C: ConsensusStorage> RewardsEngine<H, C> {
    pub async fn calculate(
        &self,
        header: &BlockHeader,
        btc_tip: BtcBlockTip,
    ) -> Result<Option<RewardsOutcome>, RewardsEngineError<H::Error, C::Error>> {
        let Some(core) = &header.core_block else {
            return Ok(None);
        };
        let height = core.height.as_u32();
        if height % 100 != 50 {
            return Ok(None);
        }
        let Some(start) = height.checked_sub(149) else {
            return Ok(None);
        };
        let target_block_height = height
            .checked_add(50)
            .ok_or(RewardsEngineError::TargetBlockHeightOverflow)?
            .into();
        let range = start..=height - 50;
        let rewards = self.calculate_cycle(header, btc_tip, range).await?;
        Ok(Some(RewardsOutcome {
            target_block_height,
            rewards,
        }))
    }

    async fn calculate_cycle(
        &self,
        header: &BlockHeader,
        btc_tip: BtcBlockTip,
        range: RangeInclusive<u32>,
    ) -> Result<Vec<PendingReward>, RewardsEngineError<H::Error, C::Error>> {
        let headers = self.load_cycle_headers(header, range).await?;
        let mut rewards = CycleRewards::new();
        for header in headers {
            self.calculate_block(&header, btc_tip, &mut rewards).await?;
        }
        Ok(rewards.into_values().collect())
    }

    async fn load_cycle_headers(
        &self,
        header: &BlockHeader,
        range: RangeInclusive<u32>,
    ) -> Result<Vec<BlockHeader>, RewardsEngineError<H::Error, C::Error>> {
        let mut current = header.clone();
        let mut expected_height = *range.end();
        let mut headers = Vec::new();
        loop {
            let parent_height = current
                .height
                .checked_sub(1)
                .ok_or(RewardsEngineError::IncompleteCycle)?;
            let parent_tip = BlockTip {
                hash: current.parent,
                height: parent_height.into(),
            };
            current = self
                .chain_storage
                .get_block_header_by_block_tip(parent_tip)
                .await
                .map_err(RewardsEngineError::ChainStorage)?
                .ok_or(RewardsEngineError::MissingBlock(parent_tip))?;

            let Some(core) = &current.core_block else {
                continue;
            };
            let height = core.height.as_u32();
            if height > *range.end() {
                continue;
            }
            if height != expected_height {
                return Err(RewardsEngineError::UnexpectedCoreHeight {
                    expected: expected_height.into(),
                    actual: core.height,
                });
            }
            headers.push(current.clone());
            if height == *range.start() {
                headers.reverse();
                return Ok(headers);
            }
            expected_height -= 1;
        }
    }

    async fn calculate_block(
        &self,
        header: &BlockHeader,
        btc_tip: BtcBlockTip,
        rewards: &mut CycleRewards,
    ) -> Result<(), RewardsEngineError<H::Error, C::Error>> {
        let core = header.core_block.as_ref().ok_or_else(|| {
            RewardsEngineError::BlockWeight(BlockWeightCalculatorError::NotCoreBlock(
                header.block_tip(),
            ))
        })?;
        let weights = BlockWeightCalculator {
            header,
            btc_tip,
            consensus_storage: self.consensus_storage.as_ref(),
            protocol_params: &self.protocol_params,
        }
        .calculate()
        .await
        .map_err(RewardsEngineError::BlockWeight)?;
        let block_rewards = BlockRewardCalculator {
            weights,
            block_height: core.height,
        }
        .calculate();
        for (key, reward) in block_rewards {
            Self::accumulate_reward(rewards, key, reward)?;
        }
        Ok(())
    }

    fn accumulate_reward(
        rewards: &mut CycleRewards,
        key: PredicatePair,
        reward: PendingReward,
    ) -> Result<(), RewardsEngineError<H::Error, C::Error>> {
        let amount = reward.amount;
        let reward = rewards.entry(key).or_insert_with(|| PendingReward {
            amount: 0,
            ..reward
        });
        reward.amount = reward
            .amount
            .checked_add(amount)
            .ok_or(RewardsEngineError::RewardOverflow)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RewardsEngineError<H, C> {
    ChainStorage(H),
    BlockWeight(BlockWeightCalculatorError<C>),
    RewardOverflow,
    TargetBlockHeightOverflow,
    MissingBlock(BlockTip),
    IncompleteCycle,
    UnexpectedCoreHeight {
        expected: CoreFlameHeight,
        actual: CoreFlameHeight,
    },
}
