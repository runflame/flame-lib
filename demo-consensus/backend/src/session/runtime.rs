use std::{convert::Infallible, num::NonZeroU16, sync::Arc};

use anyhow::Result;
use btc_integration::{
    BitcoinConfig, BitcoinFacade, BtcBlockTip, IdentityConfig, IdentityManager, IndexedBlock,
    ProtocolIndexerV31, btc::rpc::Core31RpcApi, identity::storage::InMemorySecretStorage,
};
use flame_chain_service::InMemoryChain;
use flame_minting::{
    MintingOrchestrator,
    consensus::{
        MintingOutcome, MintingOutcomeApplier, MintingProtocolParams,
        applier::journal::InMemoryMintingJournal, storage::InMemoryConsensusStorage,
    },
    core_block_notifier::CoreBlockNotifier,
    minter::{MinterManager, VotePolicy, journal::in_memory::InMemoryMinterJournal},
    orchestrator::{
        consensus::ConsensusLoop,
        cursor_storage::in_memory::InMemoryCursorStorage,
        ports::{ConsensusEngine, MintingEngineFactory},
        sender::MintingSender,
    },
    rewards::{RewardsApplier, RewardsEngine, storage::in_memory::InMemoryRewardsStorage},
};
use flame_storage::{chain::InMemoryChainStorage, state::canonical::InMemoryCanonicalStorage};
use tokio::sync::Mutex;

type Engine =
    MintingEngineFactory<InMemoryConsensusStorage, InMemoryChainStorage, InMemoryCanonicalStorage>;
type Applier = MintingOutcomeApplier<
    InMemoryConsensusStorage,
    InMemoryChain,
    InMemoryMintingJournal,
    InMemoryCanonicalStorage,
>;
type DemoLoop = ConsensusLoop<
    RecordingEngine,
    Applier,
    InMemoryCursorStorage,
    InMemoryRewardsStorage,
    InMemoryChainStorage,
    InMemoryConsensusStorage,
>;
type Orchestrator = MintingOrchestrator<
    Arc<MintingSender>,
    Arc<ProtocolIndexerV31>,
    DemoLoop,
    MinterManager<InMemoryChain, MintingSender, InMemoryMinterJournal>,
    InMemoryChain,
    ManualCoreBlockNotifier,
>;

#[derive(Clone)]
pub(super) struct RecordedBlock {
    pub block: IndexedBlock,
    pub outcome: MintingOutcome,
}

pub(super) struct RecordingEngine {
    inner: Engine,
    history: Arc<Mutex<Vec<RecordedBlock>>>,
}

impl ConsensusEngine for RecordingEngine {
    type Error = <Engine as ConsensusEngine>::Error;

    async fn get_minting_outcome(
        &self,
        block: IndexedBlock,
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
        cursor: BtcBlockTip,
        identity: IdentityConfig,
    ) -> Result<Self> {
        let parameters = Arc::new(MintingProtocolParams {
            acquisition_maturity: 1,
            default_acquisition_duration: NonZeroU16::new(100).unwrap(),
            min_acquisition_duration: NonZeroU16::MIN,
            max_vote_delay: 10,
        });
        let blocks = InMemoryChainStorage::new();
        let consensus = InMemoryConsensusStorage::new();
        let canonical = InMemoryCanonicalStorage::new();
        let cursor = InMemoryCursorStorage::new(cursor);
        let history = Arc::new(Mutex::new(Vec::new()));
        let identity_manager = Arc::new(IdentityManager::new(
            Arc::new(InMemorySecretStorage::default()),
            identity,
        ));
        let btc_indexer = Arc::new(ProtocolIndexerV31::new(Arc::new(BitcoinFacade::new(
            Arc::new(Core31RpcApi::new(
                &bitcoin.node_rpc_url,
                bitcoin.auth.clone(),
            )?),
        ))));
        let btc_sender = Arc::new(MintingSender::new(bitcoin, identity_manager.clone()));
        let consensus_manager = ConsensusLoop::new(
            RecordingEngine {
                inner: Engine {
                    protocol_params: parameters.clone(),
                    consensus_storage: Arc::new(consensus.clone()),
                    chain_storage: Arc::new(blocks.clone()),
                    canonical_storage: Arc::new(canonical.clone()),
                },
                history: history.clone(),
            },
            MintingOutcomeApplier {
                consensus_storage: consensus.clone(),
                chain: chain.clone(),
                journal: InMemoryMintingJournal::new(),
                canonical_storage: canonical.clone(),
            },
            cursor.clone(),
            RewardsEngine {
                chain_storage: Arc::new(blocks.clone()),
                consensus_storage: Arc::new(consensus.clone()),
                protocol_params: parameters.clone(),
            },
            RewardsApplier {
                rewards_storage: InMemoryRewardsStorage::new(),
            },
        );
        let minter_manager = MinterManager::new(
            Arc::new(chain.clone()),
            btc_sender.clone(),
            Arc::new(InMemoryMinterJournal::new()),
            VotePolicy::Manual,
        );
        let orchestrator = MintingOrchestrator {
            identity_manager,
            btc_sender,
            btc_indexer,
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
