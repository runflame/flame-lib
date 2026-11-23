use super::consensus_engine::ConsensusEngine;
use crate::block_processing::BtcBlockRecords;
use crate::consensus::{
    ConsensusOutcome, ConsensusStorage, IncludedEndorsement, MintingEngine, MintingEngineError,
    MintingOutcome, MintingProtocolParams, engine::consensus_evaluator::ConsensusEvaluator,
};
use flame_storage::{CanonicalStorage, ChainStorage};
use std::sync::Arc;

pub struct MintingEngineFactory<C, H, S> {
    pub protocol_params: Arc<MintingProtocolParams>,
    pub consensus_storage: Arc<C>,
    pub chain_storage: Arc<H>,
    pub canonical_storage: Arc<S>,
}

impl<C, H, S> ConsensusEngine for MintingEngineFactory<C, H, S>
where
    C: ConsensusStorage + Send + Sync + 'static,
    H: ChainStorage + Send + Sync + 'static,
    S: CanonicalStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H::Error: Send + 'static,
{
    type Error = MintingEngineError<C::Error, H::Error>;

    async fn get_rollback_plan(
        &self,
        input: crate::consensus::BitcoinRollbackInput,
    ) -> Result<crate::consensus::BitcoinRollbackPlan, Self::Error> {
        crate::consensus::rollback::bitcoin_rollback::calculate(
            self.consensus_storage.as_ref(),
            self.chain_storage.as_ref(),
            &self.protocol_params,
            input,
        )
        .await
    }

    async fn get_endorsements_outcome(
        &self,
        endorsements: Vec<IncludedEndorsement>,
    ) -> Result<ConsensusOutcome, Self::Error> {
        ConsensusEvaluator {
            protocol_params: &self.protocol_params,
            consensus_storage: self.consensus_storage.as_ref(),
            chain_storage: self.chain_storage.as_ref(),
        }
        .evaluate(&endorsements, Vec::new())
        .await
    }

    async fn get_minting_outcome(
        &self,
        block: BtcBlockRecords,
    ) -> Result<MintingOutcome, Self::Error> {
        MintingEngine::new(
            Arc::new(block),
            self.protocol_params.clone(),
            self.consensus_storage.clone(),
            self.chain_storage.clone(),
            self.canonical_storage.clone(),
        )
        .get_minting_outcome()
        .await
    }
}
