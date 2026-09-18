use super::{
    ConsensusStorage, IncludedAcquisition, MinterAcquisitions, MintingOutcome,
    MintingProtocolParams,
};
use crate::rewards::RewardsEngine;
use btc_integration::{IndexedBlock, MinterP2wsh};
use flame_storage::{CanonicalStorage, ChainStorage};
use std::collections::HashMap;
use std::sync::Arc;
use vote_validation::{VoteValidationError, VoteValidationResult, VoteValidator};

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

#[cfg(test)]
mod tests;
mod vote_validation;
