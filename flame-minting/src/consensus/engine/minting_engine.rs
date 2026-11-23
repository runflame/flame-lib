use super::MintingEngineError;
use super::consensus_evaluator::ConsensusEvaluator;
use crate::block_processing::BtcBlockRecords;
use crate::consensus::{
    ConsensusStorage, IncludedAcquisition, IncludedEndorsement, MintingOutcome,
    MintingProtocolParams,
};
use flame_storage::{CanonicalStorage, ChainStorage};
use std::sync::Arc;

pub struct MintingEngine<C: ConsensusStorage, H: ChainStorage, S: CanonicalStorage> {
    pub new_block: Arc<BtcBlockRecords>,
    pub protocol_params: Arc<MintingProtocolParams>,
    pub consensus_storage: Arc<C>,
    pub chain_storage: Arc<H>,
    pub canonical_storage: Arc<S>,
}

impl<C: ConsensusStorage, H: ChainStorage, S: CanonicalStorage> MintingEngine<C, H, S> {
    pub fn new(
        new_block: Arc<BtcBlockRecords>,
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
        }
    }

    pub async fn get_minting_outcome(
        &self,
    ) -> Result<MintingOutcome, MintingEngineError<C::Error, H::Error>> {
        let endorsements: Vec<_> = self
            .new_block
            .endorsements
            .iter()
            .cloned()
            .map(|endorsement| IncludedEndorsement {
                btc_block: self.new_block.id,
                endorsement,
            })
            .collect();
        let consensus = ConsensusEvaluator {
            protocol_params: &self.protocol_params,
            consensus_storage: self.consensus_storage.as_ref(),
            chain_storage: self.chain_storage.as_ref(),
        }
        .evaluate(&endorsements, self.validate_acquisitions())
        .await?;
        Ok(MintingOutcome {
            consensus,
            next_btc_cursor: self.new_block.id,
        })
    }

    fn validate_acquisitions(&self) -> Vec<IncludedAcquisition> {
        self.new_block
            .burns
            .iter()
            .filter(|acquisition| {
                let duration = acquisition
                    .data()
                    .duration
                    .unwrap_or(self.protocol_params.default_acquisition_duration.get());
                duration >= self.protocol_params.min_acquisition_duration.get()
            })
            .map(|acquisition| IncludedAcquisition {
                btc_block: self.new_block.id,
                acquisition: acquisition.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests;
