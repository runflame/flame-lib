use crate::block_processing::bitcoin::{
    consensus_loop::ConsensusLoop, in_memory_cursor_storage::InMemoryCursorStorage,
};
use crate::{
    block_processing::{
        BitcoinBlockProcessor, ConsensusManager, FlameBlockProcessor,
        journal::{
            ConsensusJournal, InMemoryBitcoinJournal, InMemoryConsensusJournal,
            InMemoryFlameJournal,
        },
    },
    consensus::{MintingEngineFactory, MintingProtocolParams, storage::InMemoryConsensusStorage},
    rewards::{RewardsEngine, storage::InMemoryRewardsStorage},
};
use btc_integration::watcher::BlockId;
use flame_chain_service::ChainAccess;
use flame_storage::{chain::InMemoryChainStorage, state::canonical::InMemoryCanonicalStorage};
use std::{num::NonZeroU16, sync::Arc};

type Engine =
    MintingEngineFactory<InMemoryConsensusStorage, InMemoryChainStorage, InMemoryCanonicalStorage>;

pub type DefaultConsensusLoop<H> = ConsensusLoop<
    BitcoinBlockProcessor<
        Engine,
        InMemoryConsensusStorage,
        H,
        InMemoryBitcoinJournal,
        InMemoryCanonicalStorage,
        InMemoryRewardsStorage,
        InMemoryCursorStorage,
        InMemoryChainStorage,
    >,
    FlameBlockProcessor<
        Engine,
        InMemoryConsensusStorage,
        H,
        InMemoryCanonicalStorage,
        InMemoryRewardsStorage,
        InMemoryCursorStorage,
        InMemoryChainStorage,
        InMemoryFlameJournal,
    >,
    InMemoryConsensusJournal,
>;

pub(in crate::orchestrator) fn consensus_loop<H>(
    chain: H,
    initial_cursor: BlockId,
) -> DefaultConsensusLoop<H>
where
    H: ChainAccess + Clone + Send + Sync + 'static,
    H::Error: Send + 'static,
{
    let protocol_params = Arc::new(MintingProtocolParams {
        acquisition_maturity: 1,
        default_acquisition_duration: NonZeroU16::new(100).unwrap(),
        min_acquisition_duration: NonZeroU16::MIN,
        max_endorsement_delay: 10,
    });
    let consensus_storage = InMemoryConsensusStorage::new();
    let chain_storage = Arc::new(InMemoryChainStorage::new());
    let canonical_storage = InMemoryCanonicalStorage::new();
    let rewards_storage = InMemoryRewardsStorage::new();
    let cursor_storage = InMemoryCursorStorage::new(initial_cursor);
    let journal = InMemoryConsensusJournal::new(initial_cursor);
    let bitcoin_processor = BitcoinBlockProcessor::new(
        MintingEngineFactory {
            protocol_params: protocol_params.clone(),
            consensus_storage: Arc::new(consensus_storage.clone()),
            chain_storage: chain_storage.clone(),
            canonical_storage: Arc::new(canonical_storage.clone()),
        },
        consensus_storage.clone(),
        chain.clone(),
        journal.bitcoin_journal(),
        canonical_storage.clone(),
        rewards_storage.clone(),
        cursor_storage.clone(),
        RewardsEngine {
            chain_storage: chain_storage.clone(),
            consensus_storage: Arc::new(consensus_storage.clone()),
            protocol_params: protocol_params.clone(),
        },
    );
    let flame_processor = FlameBlockProcessor::new(
        Engine {
            protocol_params: protocol_params.clone(),
            consensus_storage: Arc::new(consensus_storage.clone()),
            chain_storage: chain_storage.clone(),
            canonical_storage: Arc::new(canonical_storage.clone()),
        },
        consensus_storage.clone(),
        chain,
        canonical_storage,
        rewards_storage,
        cursor_storage,
        RewardsEngine {
            chain_storage,
            consensus_storage: Arc::new(consensus_storage),
            protocol_params,
        },
        journal.flame_journal(),
    );
    ConsensusLoop::new(
        ConsensusManager::new(bitcoin_processor, flame_processor, journal),
        initial_cursor,
    )
}
