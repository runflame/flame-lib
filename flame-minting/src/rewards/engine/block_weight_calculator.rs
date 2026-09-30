use std::collections::BTreeMap;

use btc_integration::BtcBlockTip;
use flamechain::{BlockHeader, BlockTip, CoreBlockTip};
use flamevm::Predicate;

use crate::consensus::{ConsensusStorage, MintingProtocolParams};

pub type PredicatePair = ([u8; 32], [u8; 32]);
pub type BlockWeights = BTreeMap<PredicatePair, RewardWeight>;

pub struct RewardWeight {
    pub flame_predicate: Predicate,
    pub access_predicate: Predicate,
    pub weight: f64,
}

pub struct BlockWeightCalculator<'a, C: ConsensusStorage> {
    pub header: &'a BlockHeader,
    pub btc_tip: BtcBlockTip,
    pub consensus_storage: &'a C,
    pub protocol_params: &'a MintingProtocolParams,
}

impl<C: ConsensusStorage> BlockWeightCalculator<'_, C> {
    pub async fn calculate(self) -> Result<BlockWeights, BlockWeightCalculatorError<C::Error>> {
        let core =
            self.header
                .core_block
                .as_ref()
                .ok_or(BlockWeightCalculatorError::NotCoreBlock(
                    self.header.block_tip(),
                ))?;
        let tip = CoreBlockTip {
            hash: self.header.id(),
            height: core.height,
        };
        let target_height = u64::from(core.target_btc_height);
        let mut votes = self
            .consensus_storage
            .get_votes_for_block(tip)
            .await
            .map_err(BlockWeightCalculatorError::ConsensusStorage)?;
        votes.sort_by_key(|vote| vote.original.vote.auth().minter().p2wsh());

        let mut weights = BTreeMap::new();
        for vote in votes {
            let included = &vote.original;
            if included.btc_block.height > self.btc_tip.height {
                continue;
            }
            let delay = included.btc_block.height.checked_sub(target_height).ok_or(
                BlockWeightCalculatorError::VoteBeforeTarget {
                    inclusion_height: included.btc_block.height,
                    target_height,
                },
            )?;
            if delay > u64::from(self.protocol_params.max_vote_delay) {
                continue;
            }
            let minter = included.vote.auth().minter();
            let mut active = self
                .consensus_storage
                .get_active_minter_acquisitions_at_height(
                    &minter.p2wsh(),
                    target_height,
                    self.protocol_params,
                )
                .await
                .map_err(BlockWeightCalculatorError::ConsensusStorage)?;
            if active.is_double_signed {
                continue;
            }
            active.acquisitions.sort_by_key(|acquisition| {
                (
                    acquisition.acquisition.txid(),
                    acquisition.acquisition.output_index(),
                )
            });
            let delay_divisor = 2.0_f64.powi(i32::try_from(delay).unwrap_or(i32::MAX));
            for acquisition in active.acquisitions {
                let weight = acquisition.acquisition.amount().to_sat() as f64
                    / f64::from(acquisition.duration(self.protocol_params))
                    / delay_divisor;
                if weight == 0.0 {
                    continue;
                }
                let flame_predicate = minter.flame_predicate();
                let access_predicate = &acquisition.acquisition.data().access_predicate;
                let key = (
                    flame_predicate.to_point().to_bytes(),
                    access_predicate.to_point().to_bytes(),
                );
                let entry = weights.entry(key).or_insert_with(|| RewardWeight {
                    flame_predicate: flame_predicate.to_opaque(),
                    access_predicate: access_predicate.to_opaque(),
                    weight: 0.0,
                });
                entry.weight += weight;
            }
        }
        Ok(weights)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockWeightCalculatorError<C> {
    ConsensusStorage(C),
    NotCoreBlock(BlockTip),
    VoteBeforeTarget {
        inclusion_height: u64,
        target_height: u64,
    },
}
