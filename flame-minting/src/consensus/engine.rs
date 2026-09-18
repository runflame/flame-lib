use crate::rewards::RewardsEngine;
use btc_integration::{IndexedBlock, MinterIdentity, MinterP2wsh};
use flame_storage::{CanonicalStorage, ChainStorage};
use flamechain::CoreFlameHeight;
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    ConsensusStorage, DoubleSign, IncludedAcquisition, IncludedVote, MinterAcquisitions,
    MintingOutcome, MintingProtocolParams, WeightedVote,
};

pub struct MintingEngine<C: ConsensusStorage, H: ChainStorage, S: CanonicalStorage> {
    pub new_block: Arc<IndexedBlock>,
    pub protocol_params: Arc<MintingProtocolParams>,
    pub consensus_storage: Arc<C>,
    pub chain_storage: Arc<H>,
    pub canonical_storage: Arc<S>,
    pub rewards_engine: RewardsEngine,
}

impl<C: ConsensusStorage, H: ChainStorage, S: CanonicalStorage> MintingEngine<C, H, S> {
    pub fn new(
        new_block: Arc<IndexedBlock>,
        protocol_params: Arc<MintingProtocolParams>,
        consensus_storage: Arc<C>,
        chain_storage: Arc<H>,
        canonical_storage: Arc<S>,
    ) -> Self {
        Self {
            new_block,
            protocol_params,
            consensus_storage,
            chain_storage,
            canonical_storage,
            rewards_engine: RewardsEngine,
        }
    }

    pub async fn get_minting_outcome(&self) -> MintingOutcome {
        return MintingOutcome {
            accepted_votes: vec![],
            accepted_acquisitions: vec![],
            double_signs: vec![],
            pending_votes: Default::default(),
            next_btc_cursor: self.new_block.btc_block_tip,
            expected_canonical_tip: None,
            selected_tip: None,
            detach: vec![],
            attach: vec![],
        };
    }

    fn validate_acquisitions(&self) -> Vec<IncludedAcquisition> {
        self.new_block
            .acquisitions
            .iter()
            .filter(|acquisition| {
                let duration = acquisition
                    .data()
                    .duration
                    .unwrap_or(self.protocol_params.default_acquisition_duration.get());
                duration >= self.protocol_params.min_acquisition_duration.get()
            })
            .map(|acquisition| IncludedAcquisition {
                btc_block: self.new_block.btc_block_tip,
                acquisition: acquisition.clone(),
            })
            .collect()
    }

    fn is_acquisition_active(
        &self,
        acquisition: &IncludedAcquisition,
        target_btc_height: u64,
    ) -> bool {
        let duration = acquisition
            .acquisition
            .data()
            .duration
            .unwrap_or(self.protocol_params.default_acquisition_duration.get());

        // Equivalent to X + maturity <= height < X + maturity + duration,
        // without overflowing when the inclusion height is near u64::MAX.
        target_btc_height
            .checked_sub(acquisition.btc_block.height)
            .and_then(|age| age.checked_sub(u64::from(self.protocol_params.acquisition_maturity)))
            .is_some_and(|active_age| active_age < u64::from(duration))
    }

    async fn validate_votes(
        &self,
        acquisitions: &HashMap<MinterP2wsh, MinterAcquisitions>,
    ) -> Result<VoteValidationResult, VoteValidationError<C::Error, H::Error>> {
        let mut valid_votes: HashMap<MinterIdentity, Vec<IncludedVote>> = HashMap::new();
        let mut double_signs: HashMap<MinterIdentity, MinterDoubleSigns> = HashMap::new();
        let mut removed_votes = Vec::new();
        let mut pending_votes = Vec::new();

        // We need to process votes sequentially to search for double votes
        for vote in &self.new_block.votes {
            let minter = vote.auth().minter();

            let Some(minter_acquisitions) = acquisitions.get(&minter.p2wsh()) else {
                continue;
            };
            if minter_acquisitions.acquisitions.is_empty() || minter_acquisitions.is_double_signed {
                continue;
            }

            let height = vote.block_height();
            let included_vote = IncludedVote {
                btc_block: self.new_block.btc_block_tip,
                vote: vote.clone(),
            };

            let Some(core_block_header) = self
                .chain_storage
                .get_core_block_header(vote.block_tip())
                .await
                .map_err(VoteValidationError::ChainStorage)?
            else {
                pending_votes.push(included_vote);
                continue;
            };

            let target_btc_height = u64::from(core_block_header.target_btc_height);
            if self.new_block.btc_block_tip.height < target_btc_height {
                continue;
            }

            if !minter_acquisitions
                .acquisitions
                .iter()
                .any(|acquisition| self.is_acquisition_active(acquisition, target_btc_height))
            {
                continue;
            }

            if let Some(double_sign) = double_signs
                .get_mut(minter)
                .and_then(|signs| signs.double_signs.get_mut(&height))
            {
                double_sign.votes.push(included_vote);
                continue;
            }

            if let Some(existing_votes) = valid_votes.get_mut(minter) {
                if let Some(index) = existing_votes
                    .iter()
                    .position(|v| v.block_height() == vote.block_height())
                {
                    if existing_votes[index].block_hash() == vote.block_hash() {
                        // Deduplicate votes for the same block
                        continue;
                    }
                    let existing_vote = existing_votes.remove(index);
                    double_signs
                        .entry(minter.clone())
                        .or_default()
                        .push(DoubleSign {
                            minter: minter.p2wsh(),
                            target_flame_height: height,
                            votes: vec![existing_vote, included_vote],
                        });
                    continue;
                }
            }

            if let Some(existing_vote) = self
                .consensus_storage
                .get_minter_vote_for_height(&minter.p2wsh(), height)
                .await
                .map_err(VoteValidationError::ConsensusStorage)?
            {
                if existing_vote.original.block_hash() == vote.block_hash() {
                    // This minter has already voted for this block.
                    continue;
                }
                double_signs
                    .entry(minter.clone())
                    .or_default()
                    .push(DoubleSign {
                        minter: minter.p2wsh(),
                        target_flame_height: height,
                        votes: vec![existing_vote.original.clone(), included_vote],
                    });
                removed_votes.push(existing_vote);
                continue;
            }

            valid_votes
                .entry(minter.clone())
                .or_default()
                .push(included_vote);
        }

        valid_votes.retain(|minter, _| !double_signs.contains_key(minter));
        pending_votes.retain(|vote| !double_signs.contains_key(vote.vote.auth().minter()));

        Ok(VoteValidationResult {
            valid_votes: valid_votes.into_values().flatten().collect(),
            double_signs: double_signs
                .into_values()
                .flat_map(|signs| signs.double_signs.into_values())
                .collect(),
            removed_votes,
            pending_votes,
        })
    }
}

struct VoteValidationResult {
    valid_votes: Vec<IncludedVote>,
    double_signs: Vec<DoubleSign>,
    removed_votes: Vec<WeightedVote>,
    pending_votes: Vec<IncludedVote>,
}

#[derive(Debug)]
enum VoteValidationError<C, H> {
    ConsensusStorage(C),
    ChainStorage(H),
}

#[derive(Default)]
struct MinterDoubleSigns {
    double_signs: HashMap<CoreFlameHeight, DoubleSign>,
}

impl MinterDoubleSigns {
    fn push(&mut self, sign: DoubleSign) {
        self.double_signs
            .entry(sign.target_flame_height)
            .and_modify(|existing| existing.votes.extend(sign.votes.iter().cloned()))
            .or_insert(sign);
    }
}

#[cfg(test)]
mod tests;
