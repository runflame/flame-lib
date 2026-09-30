use std::{num::NonZeroU16, sync::Arc};

use btc_integration::BtcBlockTip;
use flame_chain_service::ChainAccess;
use flame_storage::{chain::InMemoryChainStorage, state::canonical::InMemoryCanonicalStorage};

use crate::consensus::{
    MintingOutcomeApplier, MintingProtocolParams,
    applier::journal::in_memory::InMemoryMintingJournal, storage::InMemoryConsensusStorage,
};
use crate::rewards::{RewardsApplier, RewardsEngine, storage::in_memory::InMemoryRewardsStorage};

use super::{
    consensus::ConsensusLoop, cursor_storage::in_memory::InMemoryCursorStorage,
    ports::MintingEngineFactory,
};

pub type DefaultConsensusLoop<H> = ConsensusLoop<
    MintingEngineFactory<InMemoryConsensusStorage, InMemoryChainStorage, InMemoryCanonicalStorage>,
    MintingOutcomeApplier<
        InMemoryConsensusStorage,
        H,
        InMemoryMintingJournal,
        InMemoryCanonicalStorage,
    >,
    InMemoryCursorStorage,
    InMemoryRewardsStorage,
    InMemoryChainStorage,
    InMemoryConsensusStorage,
>;

pub(super) fn consensus_loop<H>(chain: H, initial_cursor: BtcBlockTip) -> DefaultConsensusLoop<H>
where
    H: ChainAccess + Send + Sync + 'static,
    H::Error: Send + 'static,
{
    let protocol_params = Arc::new(MintingProtocolParams {
        acquisition_maturity: 1,
        default_acquisition_duration: NonZeroU16::new(100).unwrap(),
        min_acquisition_duration: NonZeroU16::MIN,
        max_vote_delay: 10,
    });
    let consensus_storage = InMemoryConsensusStorage::new();
    let chain_storage = Arc::new(InMemoryChainStorage::new());
    let canonical_storage = InMemoryCanonicalStorage::new();

    ConsensusLoop::new(
        MintingEngineFactory {
            protocol_params: protocol_params.clone(),
            consensus_storage: Arc::new(consensus_storage.clone()),
            chain_storage: chain_storage.clone(),
            canonical_storage: Arc::new(canonical_storage.clone()),
        },
        MintingOutcomeApplier {
            consensus_storage: consensus_storage.clone(),
            chain,
            journal: InMemoryMintingJournal::new(),
            canonical_storage,
        },
        InMemoryCursorStorage::new(initial_cursor),
        RewardsEngine {
            chain_storage,
            consensus_storage: Arc::new(consensus_storage),
            protocol_params,
        },
        RewardsApplier {
            rewards_storage: InMemoryRewardsStorage::new(),
        },
    )
}
