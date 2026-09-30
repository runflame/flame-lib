use std::collections::BTreeMap;

use flamechain::{CoreFlameHeight, SPARKS_PER_FLAME};

use super::block_weight_calculator::{BlockWeights, PredicatePair};
use crate::rewards::PendingReward;

const INITIAL_REWARD: u64 = 50 * SPARKS_PER_FLAME;
const HALVING_INTERVAL: u32 = 210_000;

pub type BlockRewards = BTreeMap<PredicatePair, PendingReward>;

pub struct BlockRewardCalculator {
    pub weights: BlockWeights,
    pub block_height: CoreFlameHeight,
}

impl BlockRewardCalculator {
    // TODO: better algorithm
    pub fn calculate(self) -> BlockRewards {
        let total_weight: f64 = self.weights.values().map(|entry| entry.weight).sum();
        if total_weight == 0.0 {
            return BlockRewards::new();
        }
        let inflation = calculate_block_reward(self.block_height);
        self.weights
            .into_iter()
            .map(|(key, entry)| {
                let share = entry.weight / total_weight;
                let amount = (inflation as f64 * share) as u64;
                (
                    key,
                    PendingReward {
                        flame_predicate: entry.flame_predicate,
                        access_predicate: entry.access_predicate,
                        amount,
                    },
                )
            })
            .collect()
    }
}

fn calculate_block_reward(height: CoreFlameHeight) -> u64 {
    let halvings = height.as_u32() / HALVING_INTERVAL;
    INITIAL_REWARD.checked_shr(halvings).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewards::engine::block_weight_calculator::RewardWeight;
    use flamevm::{Predicate, Scalar};

    fn weights(values: &[f64]) -> BlockWeights {
        values
            .iter()
            .enumerate()
            .map(|(index, &weight)| {
                let flame_predicate = Predicate::opaque(Predicate::unspendable_key());
                let point = Predicate::unspendable_key().decompress().unwrap();
                let access_predicate = Predicate::opaque(
                    (point * Scalar::from(index as u64 + 1).to_dalek()).compress(),
                );
                let key = (
                    flame_predicate.to_point().to_bytes(),
                    access_predicate.to_point().to_bytes(),
                );
                (
                    key,
                    RewardWeight {
                        flame_predicate,
                        access_predicate,
                        weight,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn distributes_inflation_by_weight_and_preserves_predicates() {
        for (height, inflation) in [(209_999, 5_000_000_000_u64), (210_000, 2_500_000_000)] {
            let weights = weights(&[1.0, 3.0]);
            let expected: BTreeMap<_, _> = weights
                .iter()
                .map(|(key, entry)| (*key, (inflation as f64 * entry.weight / 4.0) as u64))
                .collect();
            let rewards = BlockRewardCalculator {
                weights,
                block_height: height.into(),
            }
            .calculate();
            assert_eq!(rewards.len(), 2);
            for (key, reward) in rewards {
                assert_eq!(reward.amount, expected[&key]);
                assert_eq!(reward.flame_predicate.to_point().to_bytes(), key.0);
                assert_eq!(reward.access_predicate.to_point().to_bytes(), key.1);
            }
        }
    }

    #[test]
    fn rounds_down_to_sparks_without_redistributing_the_remainder() {
        let rewards = BlockRewardCalculator {
            weights: weights(&[1.0, 1.0, 1.0]),
            block_height: 1.into(),
        }
        .calculate();
        assert_eq!(rewards.len(), 3);
        assert!(
            rewards
                .values()
                .all(|reward| reward.amount == 1_666_666_666)
        );
    }

    #[test]
    fn handles_empty_weights_zero_total_and_exhausted_emission() {
        for weights in [BlockWeights::new(), weights(&[0.0, 0.0])] {
            assert!(
                BlockRewardCalculator {
                    weights,
                    block_height: 1.into()
                }
                .calculate()
                .is_empty()
            );
        }
        for height in [33 * HALVING_INTERVAL, 64 * HALVING_INTERVAL, u32::MAX] {
            let rewards = BlockRewardCalculator {
                weights: weights(&[1.0, 3.0]),
                block_height: height.into(),
            }
            .calculate();
            assert_eq!(rewards.len(), 2);
            assert!(rewards.values().all(|reward| reward.amount == 0));
        }
    }
}
