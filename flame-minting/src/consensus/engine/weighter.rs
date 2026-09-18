use std::collections::{HashMap, hash_map::Entry};

use flame_storage::ChainStorage;
use flamechain::{BlockTip, CoreBlockTip};

use crate::consensus::{
    ConsensusStorage, IncludedAcquisition, IncludedVote, MinterAcquisitions, MintingProtocolParams,
    WeightedBlockHeader, WeightedVote,
};

pub use super::vote_weight::VoteWeightError;
use super::vote_weight::weight_vote;

pub struct Weighter<'a, C, H> {
    pub votes: &'a [IncludedVote],
    pub removed_votes: &'a [WeightedVote],
    pub new_acquisitions: &'a [IncludedAcquisition],
    pub consensus_storage: &'a C,
    pub chain_storage: &'a H,
    pub protocol_params: &'a MintingProtocolParams,
}

impl<C: ConsensusStorage, H: ChainStorage> Weighter<'_, C, H> {
    /// Returns updated headers with raw effective power and parent chain weight,
    /// including all known descendants affected by the votes.
    pub async fn weigh(&self) -> Result<WeighterResult, WeighterError<C::Error, H::Error>> {
        let mut votes_by_block: HashMap<CoreBlockTip, BlockVotes<'_>> = HashMap::new();
        for vote in self.votes {
            votes_by_block
                .entry(vote.vote.block_tip())
                .or_default()
                .added
                .push(vote);
        }
        for vote in self.removed_votes {
            votes_by_block
                .entry(vote.original.vote.block_tip())
                .or_default()
                .removed
                .push(vote);
        }

        let mut weighted_blocks = HashMap::new();
        let mut weighted_votes = Vec::with_capacity(self.votes.len());
        let mut parent_deltas: HashMap<BlockTip, i128> = HashMap::new();
        for (core_tip, votes) in votes_by_block {
            let header = self
                .chain_storage
                .get_block_header(core_tip)
                .await
                .map_err(WeighterError::ChainStorage)?
                .ok_or(WeighterError::MissingVotedBlock(core_tip))?;
            let tip = header.block_tip();
            let (header, children) = self
                .chain_storage
                .get_block_header_with_children(tip)
                .await
                .map_err(WeighterError::ChainStorage)?
                .ok_or(WeighterError::MissingBlock(tip))?;
            let core = header
                .core_block
                .as_ref()
                .ok_or(WeighterError::MissingVotedBlock(core_tip))?;
            let target = u64::from(core.target_btc_height);
            let weighted = self.weighted_block(&mut weighted_blocks, tip).await?;
            let old_weight = weighted.effective_power.checked_ilog2().unwrap_or(0);
            for vote in votes.removed {
                weighted.effective_power = weighted
                    .effective_power
                    .checked_sub(vote.effective_minting_power)
                    .ok_or(WeighterError::EffectivePowerUnderflow(tip))?;
            }
            let mut acquisitions = if votes.added.is_empty() {
                HashMap::new()
            } else {
                self.consensus_storage
                    .get_active_acquisitions_at_target_height(target, self.protocol_params)
                    .await
                    .map_err(WeighterError::ConsensusStorage)?
            };
            for acquisition in self
                .new_acquisitions
                .iter()
                .filter(|acquisition| acquisition.is_active_at(target, self.protocol_params))
            {
                acquisitions
                    .entry(acquisition.acquisition.data().minter_p2wsh)
                    .or_insert_with(|| MinterAcquisitions {
                        is_double_signed: false,
                        acquisitions: vec![],
                    })
                    .acquisitions
                    .push(acquisition.clone());
            }
            for vote in votes.added {
                let minter = vote.vote.auth().minter().p2wsh();
                let active = acquisitions
                    .get(&minter)
                    .filter(|minter| !minter.is_double_signed)
                    .map(|minter| minter.acquisitions.as_slice())
                    .unwrap_or_default();
                let vote = weight_vote(vote.clone(), target, active, self.protocol_params)
                    .map_err(WeighterError::VoteWeight)?;
                weighted.effective_power = weighted
                    .effective_power
                    .checked_add(vote.effective_minting_power)
                    .ok_or(WeighterError::EffectivePowerOverflow(tip))?;
                weighted_votes.push(vote);
            }
            let weight_delta = i128::from(weighted.effective_power.checked_ilog2().unwrap_or(0))
                - i128::from(old_weight);
            for child in children {
                let child_tip = child.block_tip();
                self.weighted_block(&mut weighted_blocks, child_tip).await?;
                *parent_deltas.entry(child_tip).or_default() += weight_delta;
            }
        }

        // Apply the net change so additions and removals on different ancestors
        // cannot produce a temporary overflow that depends on iteration order.
        for (tip, delta) in parent_deltas {
            let weighted = weighted_blocks
                .get_mut(&tip)
                .expect("descendant was loaded");
            let parent_weight = i128::from(weighted.parent_weight) + delta;
            weighted.parent_weight = u64::try_from(parent_weight).map_err(|_| {
                if parent_weight < 0 {
                    WeighterError::CumulativeWeightUnderflow(tip)
                } else {
                    WeighterError::CumulativeWeightOverflow(tip)
                }
            })?;
        }
        Ok(WeighterResult {
            weighted_blocks,
            weighted_votes,
        })
    }

    async fn weighted_block<'a>(
        &self,
        blocks: &'a mut HashMap<BlockTip, WeightedBlockHeader>,
        tip: BlockTip,
    ) -> Result<&'a mut WeightedBlockHeader, WeighterError<C::Error, H::Error>> {
        match blocks.entry(tip) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let weighted = self
                    .consensus_storage
                    .get_cumulative_weight(tip)
                    .await
                    .map_err(WeighterError::ConsensusStorage)?
                    .ok_or(WeighterError::MissingWeight(tip))?;
                Ok(entry.insert(weighted))
            }
        }
    }
}

#[derive(Default)]
struct BlockVotes<'a> {
    added: Vec<&'a IncludedVote>,
    removed: Vec<&'a WeightedVote>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WeighterResult {
    pub weighted_blocks: HashMap<BlockTip, WeightedBlockHeader>,
    pub weighted_votes: Vec<WeightedVote>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WeighterError<C, H> {
    ConsensusStorage(C),
    ChainStorage(H),
    MissingVotedBlock(CoreBlockTip),
    MissingBlock(BlockTip),
    MissingWeight(BlockTip),
    EffectivePowerOverflow(BlockTip),
    EffectivePowerUnderflow(BlockTip),
    CumulativeWeightOverflow(BlockTip),
    CumulativeWeightUnderflow(BlockTip),
    VoteWeight(VoteWeightError),
}
