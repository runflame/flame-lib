use crate::rewards::RewardsEngine;
use btc_integration::{IndexedBlock, MinterIdentity};
use flame_storage::{CanonicalStorage, ChainStorage};
use flamechain::CoreFlameHeight;
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    ConsensusStorage, DoubleSign, IncludedAcquisition, IncludedVote, MintingOutcome,
    MintingProtocolParams, WeightedVote,
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

    async fn validate_votes(&self) -> Result<VoteValidationResult, C::Error> {
        let mut valid_votes: HashMap<MinterIdentity, Vec<IncludedVote>> = HashMap::new();
        let mut double_signs: HashMap<MinterIdentity, MinterDoubleSigns> = HashMap::new();
        let mut removed_votes = Vec::new();

        // We need to process votes sequentially to search for double votes
        for vote in &self.new_block.votes {
            let minter = vote.auth().minter();
            let height = CoreFlameHeight::from(vote.block_height());
            if self
                .consensus_storage
                .get_double_sign(&minter.p2wsh(), height)
                .await?
                .is_some()
            {
                continue;
            }

            let included_vote = IncludedVote {
                btc_block: self.new_block.btc_block_tip,
                vote: vote.clone(),
            };

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
                .await?
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

        Ok(VoteValidationResult {
            valid_votes: valid_votes.into_values().flatten().collect(),
            double_signs: double_signs
                .into_values()
                .flat_map(|signs| signs.double_signs.into_values())
                .collect(),
            removed_votes,
        })
    }
}

struct VoteValidationResult {
    valid_votes: Vec<IncludedVote>,
    double_signs: Vec<DoubleSign>,
    removed_votes: Vec<WeightedVote>,
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
