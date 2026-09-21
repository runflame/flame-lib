use super::{
    ConsensusStorage, IncludedAcquisition, MinterAcquisitions, MintingOutcome,
    MintingProtocolParams,
};
use crate::rewards::RewardsEngine;
use acquisition_provider::AcquisitionProvider;
use btc_integration::{IndexedBlock, MinterP2wsh};
use flame_storage::{CanonicalStorage, ChainStorage};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use vote_validation::{VoteValidationError, VoteValidationResult, VoteValidator};
use weighter::{Weighter, WeighterError};

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

    pub async fn get_minting_outcome(
        &self,
    ) -> Result<MintingOutcome, MintingEngineError<C::Error, H::Error>> {
        let accepted_acquisitions = self.validate_acquisitions();
        let acquisition_provider = AcquisitionProvider {
            storage: self.consensus_storage.as_ref(),
            new_acquisitions: &accepted_acquisitions,
            params: &self.protocol_params,
        };
        let acquisitions = acquisition_provider
            .load_through(self.new_block.btc_block_tip.height)
            .await
            .map_err(MintingEngineError::ConsensusStorage)?;
        let validated = self
            .validate_votes(&acquisitions)
            .await
            .map_err(|error| match error {
                VoteValidationError::ConsensusStorage(error) => {
                    MintingEngineError::ConsensusStorage(error)
                }
                VoteValidationError::ChainStorage(error) => MintingEngineError::ChainStorage(error),
            })?;
        let weighted = Weighter {
            votes: &validated.valid_votes,
            removed_votes: &validated.removed_votes,
            acquisitions: &acquisition_provider,
            consensus_storage: self.consensus_storage.as_ref(),
            chain_storage: self.chain_storage.as_ref(),
            protocol_params: &self.protocol_params,
        }
        .weigh()
        .await
        .map_err(MintingEngineError::Weighter)?;
        let mut pending_votes = BTreeMap::new();
        for vote in validated.pending_votes {
            pending_votes
                .entry(vote.vote.block_tip())
                .or_insert_with(Vec::new)
                .push(vote);
        }
        Ok(MintingOutcome {
            accepted_votes: weighted.weighted_votes,
            removed_votes: validated.removed_votes,
            weighted_blocks: weighted.weighted_blocks,
            accepted_acquisitions,
            double_signs: validated.double_signs,
            pending_votes,
            next_btc_cursor: self.new_block.btc_block_tip,
        })
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

    async fn validate_votes(
        &self,
        acquisitions: &HashMap<MinterP2wsh, MinterAcquisitions>,
    ) -> Result<VoteValidationResult, VoteValidationError<C::Error, H::Error>> {
        VoteValidator {
            new_block: &self.new_block,
            protocol_params: &self.protocol_params,
            consensus_storage: self.consensus_storage.as_ref(),
            chain_storage: self.chain_storage.as_ref(),
            acquisitions,
        }
        .validate()
        .await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MintingEngineError<C, H> {
    ConsensusStorage(C),
    ChainStorage(H),
    Weighter(WeighterError<C, H>),
}

pub mod acquisition_provider;
#[cfg(test)]
mod tests;
mod vote_validation;
mod vote_weight;
pub mod weighter;
