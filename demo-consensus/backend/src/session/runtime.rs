use std::{convert::Infallible, num::NonZeroU16, sync::Arc};

use anyhow::Result;
use btc_integration::{
    BitcoinConfig, IdentityConfig,
    identity::impls::LocalKey,
    watcher::{BlockId, impls::RpcSource},
};
use flame_chain_service::InMemoryChain;
use flame_minting::BtcBlockRecords;
use flame_minting::{
    MintingOrchestrator,
    block_processing::{
        BitcoinBlockProcessor, ConsensusManager, FlameBlockProcessor,
        journal::consensus::{
            ConsensusJournal, InMemoryBitcoinJournal, InMemoryConsensusJournal,
            InMemoryFlameJournal,
        },
    },
    consensus::{
        ConsensusEngine, ConsensusOutcome, IncludedEndorsement, MintingEngineFactory,
        MintingOutcome, MintingProtocolParams, storage::InMemoryConsensusStorage,
    },
    core_block_notifier::CoreBlockNotifier,
    minter::{EndorsementPolicy, MinterManager, journal::in_memory::InMemoryMinterJournal},
    orchestrator::{
        consensus::ConsensusLoop, cursor_storage::in_memory::InMemoryCursorStorage,
        sender::MintingSender,
    },
    rewards::{RewardsEngine, storage::in_memory::InMemoryRewardsStorage},
};
use flame_storage::{chain::InMemoryChainStorage, state::canonical::InMemoryCanonicalStorage};
use tokio::sync::Mutex;

type Engine =
    MintingEngineFactory<InMemoryConsensusStorage, InMemoryChainStorage, InMemoryCanonicalStorage>;
type DemoLoop = ConsensusLoop<
    BitcoinBlockProcessor<
        RecordingEngine,
        InMemoryConsensusStorage,
        InMemoryChain,
        InMemoryBitcoinJournal,
        InMemoryCanonicalStorage,
        InMemoryRewardsStorage,
        InMemoryCursorStorage,
        InMemoryChainStorage,
    >,
    FlameBlockProcessor<
        Engine,
        InMemoryConsensusStorage,
        InMemoryChain,
        InMemoryCanonicalStorage,
        InMemoryRewardsStorage,
        InMemoryCursorStorage,
        InMemoryChainStorage,
        InMemoryFlameJournal,
    >,
    InMemoryConsensusJournal,
>;
type Orchestrator = MintingOrchestrator<
    Arc<MintingSender>,
    Arc<RpcSource>,
    DemoLoop,
    MinterManager<InMemoryChain, MintingSender, InMemoryMinterJournal>,
    InMemoryChain,
    ManualCoreBlockNotifier,
>;

#[derive(Clone)]
pub(super) struct RecordedBlock {
    pub block: BtcBlockRecords,
    pub outcome: MintingOutcome,
}

pub(super) struct RecordingEngine {
    inner: Engine,
    history: Arc<Mutex<Vec<RecordedBlock>>>,
}

impl ConsensusEngine for RecordingEngine {
    type Error = <Engine as ConsensusEngine>::Error;

    async fn get_rollback_plan(
        &self,
        input: flame_minting::consensus::BitcoinRollbackInput,
    ) -> Result<flame_minting::consensus::BitcoinRollbackPlan, Self::Error> {
        self.inner.get_rollback_plan(input).await
    }

    async fn get_endorsements_outcome(
        &self,
        endorsements: Vec<IncludedEndorsement>,
    ) -> Result<ConsensusOutcome, Self::Error> {
        self.inner.get_endorsements_outcome(endorsements).await
    }

    async fn get_minting_outcome(
        &self,
        block: BtcBlockRecords,
    ) -> Result<MintingOutcome, Self::Error> {
        let outcome = self.inner.get_minting_outcome(block.clone()).await?;
        self.history.lock().await.push(RecordedBlock {
            block,
            outcome: outcome.clone(),
        });
        Ok(outcome)
    }
}

pub(super) struct ManualCoreBlockNotifier;

impl CoreBlockNotifier for ManualCoreBlockNotifier {
    type Error = Infallible;

    async fn notify_core_block_needed(&mut self, _: u64) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(super) struct DemoRuntime {
    pub orchestrator: Orchestrator,
    pub blocks: InMemoryChainStorage,
    pub consensus: InMemoryConsensusStorage,
    pub canonical: InMemoryCanonicalStorage,
    pub cursor: InMemoryCursorStorage,
    pub parameters: Arc<MintingProtocolParams>,
    pub history: Arc<Mutex<Vec<RecordedBlock>>>,
}

impl DemoRuntime {
    pub fn new(
        chain: InMemoryChain,
        bitcoin: BitcoinConfig,
        cursor: BlockId,
        identity: &IdentityConfig,
        identity_key: Arc<LocalKey>,
    ) -> Result<Self> {
        let parameters = Arc::new(MintingProtocolParams {
            acquisition_maturity: 1,
            default_acquisition_duration: NonZeroU16::new(100).unwrap(),
            min_acquisition_duration: NonZeroU16::MIN,
            max_endorsement_delay: 10,
        });
        let blocks = InMemoryChainStorage::new();
        let consensus = InMemoryConsensusStorage::new();
        let canonical = InMemoryCanonicalStorage::new();
        let journal = InMemoryConsensusJournal::new(cursor);
        let rewards = InMemoryRewardsStorage::new();
        let lowest_block = cursor;
        let cursor = InMemoryCursorStorage::new(cursor);
        let history = Arc::new(Mutex::new(Vec::new()));
        let identity = identity.identity(&identity_key);
        let btc_source = Arc::new(RpcSource::new(&bitcoin)?);
        let btc_sender = Arc::new(MintingSender::new(bitcoin, identity.clone(), identity_key));
        let bitcoin_processor = BitcoinBlockProcessor::new(
            RecordingEngine {
                inner: Engine {
                    protocol_params: parameters.clone(),
                    consensus_storage: Arc::new(consensus.clone()),
                    chain_storage: Arc::new(blocks.clone()),
                    canonical_storage: Arc::new(canonical.clone()),
                },
                history: history.clone(),
            },
            consensus.clone(),
            chain.clone(),
            journal.bitcoin_journal(),
            canonical.clone(),
            rewards.clone(),
            cursor.clone(),
            RewardsEngine {
                chain_storage: Arc::new(blocks.clone()),
                consensus_storage: Arc::new(consensus.clone()),
                protocol_params: parameters.clone(),
            },
        );
        let flame_processor = FlameBlockProcessor::new(
            Engine {
                protocol_params: parameters.clone(),
                consensus_storage: Arc::new(consensus.clone()),
                chain_storage: Arc::new(blocks.clone()),
                canonical_storage: Arc::new(canonical.clone()),
            },
            consensus.clone(),
            chain.clone(),
            canonical.clone(),
            rewards,
            cursor.clone(),
            RewardsEngine {
                chain_storage: Arc::new(blocks.clone()),
                consensus_storage: Arc::new(consensus.clone()),
                protocol_params: parameters.clone(),
            },
            journal.flame_journal(),
        );
        let consensus_manager = ConsensusLoop::new(
            ConsensusManager::new(bitcoin_processor, flame_processor, journal),
            lowest_block,
        );
        let minter_manager = MinterManager::new(
            Arc::new(chain.clone()),
            btc_sender.clone(),
            Arc::new(InMemoryMinterJournal::new()),
            EndorsementPolicy::Manual,
        );
        let orchestrator = MintingOrchestrator {
            identity,
            btc_sender,
            btc_source,
            consensus_manager,
            minter_manager,
            chain_manager: chain,
            core_block_notifier: ManualCoreBlockNotifier,
        };
        Ok(Self {
            orchestrator,
            blocks,
            consensus,
            canonical,
            cursor,
            parameters,
            history,
        })
    }
}
