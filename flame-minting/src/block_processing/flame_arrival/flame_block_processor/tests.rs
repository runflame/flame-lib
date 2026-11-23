use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::block_processing::BtcBlockRecords;
use bitcoin::{
    Amount, BlockHash as BitcoinBlockHash, Transaction, TxOut, absolute, hashes::Hash, transaction,
};
use btc_integration::{Burn, BurnData, watcher::BlockId};
use flame_chain_service::{ChainPath, InMemoryChain};
use flame_storage::{chain::InMemoryChainStorage, state::canonical::InMemoryCanonicalStorage};
use flamechain::{Block, BlockHash, Blockchain, ChainParams, CoreBlockHeader};
use flamevm::Predicate;

use super::*;
use crate::block_processing::journal::{ConsensusJournal, FlameJournal};
use crate::{
    block_processing::{BlockProcessor, ConsensusManager, tests::support::wait_until},
    consensus::{
        BitcoinRollbackInput, BitcoinRollbackPlan, IncludedAcquisition, IncludedEndorsement,
        MintingEngineFactory, MintingProtocolParams, WeightedBlockHeader, WeightedEndorsement,
        engine::tests::endorsement_for_tip, storage::InMemoryConsensusStorage,
    },
    rewards::storage::InMemoryRewardsStorage,
};

type Engine =
    MintingEngineFactory<InMemoryConsensusStorage, InMemoryChainStorage, InMemoryCanonicalStorage>;
type Processor = FlameBlockProcessor<
    RecordingEngine,
    InMemoryConsensusStorage,
    Chain,
    InMemoryCanonicalStorage,
    InMemoryRewardsStorage,
    Cursor,
    InMemoryChainStorage,
    recovery::FaultJournal,
>;

struct RecordingEngine {
    inner: Engine,
    calls: Arc<AtomicUsize>,
    fail: bool,
}

impl ConsensusEngine for RecordingEngine {
    type Error = &'static str;

    async fn get_rollback_plan(
        &self,
        input: BitcoinRollbackInput,
    ) -> Result<BitcoinRollbackPlan, Self::Error> {
        self.inner
            .get_rollback_plan(input)
            .await
            .map_err(|_| "engine failed")
    }

    async fn get_endorsements_outcome(
        &self,
        endorsements: Vec<IncludedEndorsement>,
    ) -> Result<ConsensusOutcome, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err("engine failed");
        }
        self.inner
            .get_endorsements_outcome(endorsements)
            .await
            .map_err(|_| "engine failed")
    }
}

#[derive(Clone)]
struct Cursor {
    tip: Arc<StdMutex<BlockId>>,
    reads: Arc<AtomicUsize>,
    fail: bool,
}

impl CursorStorage for Cursor {
    type Error = &'static str;
    async fn store_cursor(&self, tip: BlockId) -> Result<(), Self::Error> {
        *self.tip.lock().unwrap() = tip;
        Ok(())
    }

    async fn get_cursor(&self) -> Result<BlockId, Self::Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err("cursor failed");
        }
        Ok(*self.tip.lock().unwrap())
    }
}

#[derive(Clone, Default)]
struct Chain {
    inner: InMemoryChain,
    wrong_block: Option<Arc<Block>>,
    fail: Arc<AtomicBool>,
}

impl ChainAccess for Chain {
    type Error = &'static str;

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Arc<Block>>, Self::Error> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("chain failed");
        }
        if let Some(block) = &self.wrong_block {
            return Ok(Some(block.clone()));
        }
        self.inner.get_block(tip).await.map_err(|_| "chain failed")
    }

    async fn get_chain_path(&self, from: BlockTip, to: BlockTip) -> Result<ChainPath, Self::Error> {
        self.inner
            .get_chain_path(from, to)
            .await
            .map_err(|_| "chain failed")
    }
}

struct Fixture {
    processor: Processor,
    root: Arc<Block>,
    root_state: Blockchain,
    arriving: Arc<Block>,
}

impl Fixture {
    async fn new() -> Self {
        Self::at_btc_tip(btc_tip(300)).await
    }

    async fn at_btc_tip(initial_btc_tip: BlockId) -> Self {
        let consensus = InMemoryConsensusStorage::new();
        let blocks = Arc::new(InMemoryChainStorage::new());
        let canonical = InMemoryCanonicalStorage::new();
        let params = Arc::new(MintingProtocolParams {
            acquisition_maturity: 2,
            default_acquisition_duration: 300.try_into().unwrap(),
            min_acquisition_duration: 1.try_into().unwrap(),
            max_endorsement_delay: 10,
        });
        let processor = FlameBlockProcessor::new(
            RecordingEngine {
                inner: MintingEngineFactory {
                    protocol_params: params.clone(),
                    consensus_storage: Arc::new(consensus.clone()),
                    chain_storage: blocks.clone(),
                    canonical_storage: Arc::new(canonical.clone()),
                },
                calls: Arc::new(AtomicUsize::new(0)),
                fail: false,
            },
            consensus.clone(),
            Chain::default(),
            canonical.clone(),
            InMemoryRewardsStorage::new(),
            Cursor {
                tip: Arc::new(StdMutex::new(initial_btc_tip)),
                reads: Arc::new(AtomicUsize::new(0)),
                fail: false,
            },
            RewardsEngine {
                chain_storage: blocks,
                consensus_storage: Arc::new(consensus),
                protocol_params: params,
            },
            recovery::FaultJournal::new(initial_btc_tip),
        );
        let mut root_state = Blockchain::new(ChainParams::default()).unwrap();
        let root = Arc::new(root_state.build_block([0; 32], vec![]).unwrap());
        root_state.connect(&root).unwrap();
        canonical.commit_state(&root_state).await.unwrap();
        let mut branch = root_state.clone();
        let arriving = core_block(&mut branch, 1, 100, 1);
        let mut fixture = Self {
            processor,
            root,
            root_state,
            arriving,
        };
        fixture.register(fixture.root.clone(), 0, 0).await;
        fixture.register(fixture.arriving.clone(), 0, 0).await;
        fixture.add_acquisition(0x51).await;
        fixture
    }

    async fn register(&mut self, block: Arc<Block>, parent_weight: u64, effective_power: u64) {
        self.processor
            .rewards_engine
            .chain_storage
            .add_block(block.clone())
            .await
            .unwrap();
        self.processor
            .consensus_storage
            .store_cumulative_weight(
                block.header.block_tip(),
                &WeightedBlockHeader {
                    header: block.header.clone(),
                    parent_weight,
                    effective_power,
                },
            )
            .await
            .unwrap();
        self.processor
            .chain
            .inner
            .import_block(block)
            .await
            .unwrap();
    }

    async fn add_acquisition(&self, minter: u8) {
        let endorsement = included(minter, &self.arriving, 100);
        let data = BurnData::new(
            endorsement.endorsement.auth().minter().p2wsh(),
            Predicate::opaque(
                (Predicate::unspendable_key().decompress().unwrap()
                    * flamevm::Scalar::from(u64::from(minter)).to_dalek())
                .compress(),
            ),
        );
        let transaction = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(2400),
                script_pubkey: data.to_script(),
            }],
        };
        let acquisition = IncludedAcquisition {
            btc_block: btc_tip(98),
            acquisition: Burn::from_tx(&transaction).pop().unwrap(),
        };
        self.processor
            .consensus_storage
            .store_acquisition(acquisition.btc_block, &acquisition)
            .await
            .unwrap();
    }

    async fn pending(&self, endorsement: &IncludedEndorsement) {
        self.processor
            .consensus_storage
            .store_pending_endorsement(endorsement.endorsement.block_tip(), endorsement)
            .await
            .unwrap();
    }
}

fn btc_tip(height: u64) -> BlockId {
    BlockId {
        hash: BitcoinBlockHash::from_byte_array([height as u8; 32]),
        height,
    }
}

fn core_block(state: &mut Blockchain, height: u32, target: u32, tag: u8) -> Arc<Block> {
    let mut block = state.build_block([tag; 32], vec![]).unwrap();
    block.header.core_block = Some(CoreBlockHeader {
        height: height.into(),
        target_btc_height: target,
    });
    state.connect(&block).unwrap();
    Arc::new(block)
}

fn included(minter: u8, block: &Block, btc_height: u64) -> IncludedEndorsement {
    IncludedEndorsement {
        btc_block: btc_tip(btc_height),
        endorsement: endorsement_for_tip(
            minter,
            CoreBlockTip {
                hash: block.header.id(),
                height: block.header.core_block.as_ref().unwrap().height,
            },
        ),
    }
}

#[tokio::test]
async fn skips_non_core_blocks_and_core_blocks_without_pending() {
    let fixture = Fixture::new().await;
    let processor = &fixture.processor;
    for block in [&fixture.root, &fixture.arriving] {
        processor
            .on_flame_block_arrival(block.header.block_tip())
            .await
            .unwrap();
    }
    assert_eq!(processor.engine.calls.load(Ordering::SeqCst), 0);
    assert_eq!(processor.cursor_storage.reads.load(Ordering::SeqCst), 0);
    assert!(processor.journal.pending_entry().await.unwrap().is_none());
    assert!(
        processor
            .journal
            .latest_applied(btc_tip(300))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        processor.canonical_storage.get_tip().await.unwrap(),
        Some(fixture.root.header.block_tip())
    );
}

#[tokio::test]
async fn applies_pending_endorsements_and_switches_to_the_heavier_branch() {
    let mut fixture = Fixture::new().await;
    let mut old_state = fixture.root_state.clone();
    let old = core_block(&mut old_state, 1, 100, 2);
    fixture.register(old.clone(), 0, 4).await;
    fixture
        .processor
        .canonical_storage
        .commit_state(&old_state)
        .await
        .unwrap();
    let accepted = included(0x51, &fixture.arriving, 100);
    let rejected = included(0x52, &fixture.arriving, 100);
    let unrelated = included(0x53, &old, 100);
    for endorsement in [&accepted, &rejected, &unrelated] {
        fixture.pending(endorsement).await;
    }
    let processor = &fixture.processor;
    processor
        .on_flame_block_arrival(fixture.arriving.header.block_tip())
        .await
        .unwrap();

    assert_eq!(
        processor
            .consensus_storage
            .get_endorsements_for_block(accepted.endorsement.block_tip())
            .await
            .unwrap(),
        vec![WeightedEndorsement {
            original: accepted.clone(),
            effective_minting_power: 8,
        }]
    );
    assert!(
        processor
            .consensus_storage
            .get_pending_endorsements_for_block(accepted.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        processor
            .consensus_storage
            .get_pending_endorsements_for_block(unrelated.endorsement.block_tip())
            .await
            .unwrap(),
        vec![unrelated]
    );
    assert_eq!(
        processor.canonical_storage.get_tip().await.unwrap(),
        Some(fixture.arriving.header.block_tip())
    );
    assert_eq!(
        processor.cursor_storage.get_cursor().await.unwrap(),
        btc_tip(300)
    );
    assert_eq!(processor.engine.calls.load(Ordering::SeqCst), 1);
    let tip = fixture.arriving.header.block_tip();
    let weight = processor
        .consensus_storage
        .get_cumulative_weight(tip)
        .await
        .unwrap();
    let entry = processor
        .journal
        .latest_applied(btc_tip(300))
        .await
        .unwrap()
        .unwrap();
    processor.on_flame_block_arrival(tip).await.unwrap();
    assert_eq!(processor.engine.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        processor
            .consensus_storage
            .get_cumulative_weight(tip)
            .await
            .unwrap(),
        weight
    );
    assert_eq!(
        processor.canonical_storage.get_tip().await.unwrap(),
        Some(tip)
    );
    assert_eq!(
        processor
            .journal
            .latest_applied(btc_tip(300))
            .await
            .unwrap()
            .unwrap()
            .id,
        entry.id
    );
    assert_eq!(
        processor
            .consensus_storage
            .get_endorsements_for_block(accepted.endorsement.block_tip())
            .await
            .unwrap(),
        vec![WeightedEndorsement {
            original: accepted,
            effective_minting_power: 8,
        }]
    );
}

#[tokio::test]
async fn removes_rejected_pending_even_when_no_endorsement_is_accepted() {
    let fixture = Fixture::new().await;
    let rejected = included(0x51, &fixture.arriving, 99);
    fixture.pending(&rejected).await;
    let processor = &fixture.processor;
    processor
        .on_flame_block_arrival(fixture.arriving.header.block_tip())
        .await
        .unwrap();
    assert!(
        processor
            .consensus_storage
            .get_pending_endorsements_for_block(rejected.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        processor
            .consensus_storage
            .get_endorsements_for_block(rejected.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn applies_double_signs_and_removes_previously_counted_power() {
    let mut fixture = Fixture::new().await;
    let mut old_state = fixture.root_state.clone();
    let old = core_block(&mut old_state, 1, 100, 2);
    fixture.register(old.clone(), 0, 8).await;
    fixture
        .processor
        .canonical_storage
        .commit_state(&old_state)
        .await
        .unwrap();
    let previous = WeightedEndorsement {
        original: included(0x51, &old, 100),
        effective_minting_power: 8,
    };
    fixture
        .processor
        .consensus_storage
        .store_endorsement(previous.original.btc_block, &previous)
        .await
        .unwrap();
    let conflicting = included(0x51, &fixture.arriving, 101);
    fixture.pending(&conflicting).await;
    let processor = &fixture.processor;
    processor
        .on_flame_block_arrival(fixture.arriving.header.block_tip())
        .await
        .unwrap();
    assert!(
        processor
            .consensus_storage
            .get_endorsements_for_block(previous.original.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        processor
            .consensus_storage
            .get_cumulative_weight(old.header.block_tip())
            .await
            .unwrap()
            .unwrap()
            .effective_power,
        0
    );
    assert!(
        processor
            .consensus_storage
            .is_minter_double_signed(&conflicting.endorsement.auth().minter().p2wsh())
            .await
            .unwrap()
    );
    assert!(
        processor
            .consensus_storage
            .get_pending_endorsements_for_block(conflicting.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn manager_serializes_arrivals_and_evaluates_the_pending_batch_once() {
    let fixture = Fixture::new().await;
    fixture
        .pending(&included(0x51, &fixture.arriving, 100))
        .await;
    let tip = fixture.arriving.header.block_tip();
    let calls = fixture.processor.engine.calls.clone();
    let journal = fixture.processor.journal.owner.clone();
    let manager = Arc::new(ConsensusManager::new(
        UnusedBitcoinProcessor,
        fixture.processor,
        journal.clone(),
    ));
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    let (first, second) = tokio::join!(
        handle.on_flame_block_arrival(tip),
        handle.on_flame_block_arrival(tip)
    );
    first.unwrap();
    second.unwrap();
    wait_until(|| async {
        journal
            .latest_entry()
            .await
            .unwrap()
            .is_some_and(|entry| entry.is_complete())
    })
    .await;
    manager.shutdown().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_mismatched_and_unreadable_blocks_fail_before_evaluation() {
    let mut fixture = Fixture::new().await;
    let missing = BlockTip {
        hash: BlockHash::from([9; 32]),
        height: 999.into(),
    };
    assert!(
        matches!(fixture.processor.on_flame_block_arrival(missing).await, Err(FlameBlockProcessingError::MissingBlock(tip)) if tip == missing)
    );
    fixture.processor.chain.wrong_block = Some(fixture.root.clone());
    let tip = fixture.arriving.header.block_tip();
    assert!(
        matches!(fixture.processor.on_flame_block_arrival(tip).await, Err(FlameBlockProcessingError::BlockTipMismatch { requested, .. }) if requested == tip)
    );
    fixture.processor.chain.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        fixture.processor.on_flame_block_arrival(tip).await,
        Err(FlameBlockProcessingError::Chain("chain failed"))
    ));
    assert_eq!(fixture.processor.engine.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn engine_and_cursor_errors_preserve_pending_and_consensus_state() {
    for fail_engine in [true, false] {
        let mut fixture = Fixture::new().await;
        let endorsement = included(0x51, &fixture.arriving, 100);
        fixture.pending(&endorsement).await;
        fixture.processor.engine.fail = fail_engine;
        fixture.processor.cursor_storage.fail = !fail_engine;
        let processor = &fixture.processor;
        let result = processor
            .on_flame_block_arrival(fixture.arriving.header.block_tip())
            .await;
        if fail_engine {
            assert!(matches!(
                result,
                Err(FlameBlockProcessingError::Engine("engine failed"))
            ));
        } else {
            assert!(matches!(
                result,
                Err(FlameBlockProcessingError::Cursor("cursor failed"))
            ));
        }
        assert!(
            processor
                .consensus_storage
                .get_endorsements_for_block(endorsement.endorsement.block_tip())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            processor
                .consensus_storage
                .get_pending_endorsements_for_block(endorsement.endorsement.block_tip())
                .await
                .unwrap(),
            vec![endorsement]
        );
        assert_eq!(
            processor.canonical_storage.get_tip().await.unwrap(),
            Some(fixture.root.header.block_tip())
        );
    }
}

#[tokio::test]
async fn resumes_after_transition_failure_without_recomputing_consensus() {
    let mut fixture = Fixture::new().await;
    let endorsement = included(0x51, &fixture.arriving, 100);
    fixture.pending(&endorsement).await;
    fixture.processor.canonical_storage = InMemoryCanonicalStorage::new();
    let processor = &mut fixture.processor;
    assert!(matches!(
        processor
            .on_flame_block_arrival(fixture.arriving.header.block_tip())
            .await,
        Err(FlameBlockProcessingError::PrepareFlame(
            flame::PrepareFlameError::MissingCanonicalTip
        ))
    ));
    assert_eq!(
        processor
            .consensus_storage
            .get_pending_endorsements_for_block(endorsement.endorsement.block_tip())
            .await
            .unwrap(),
        vec![endorsement.clone()]
    );
    let accepted = vec![WeightedEndorsement {
        original: endorsement.clone(),
        effective_minting_power: 8,
    }];
    assert_eq!(
        processor
            .consensus_storage
            .get_endorsements_for_block(endorsement.endorsement.block_tip())
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        processor
            .journal
            .pending_entry()
            .await
            .unwrap()
            .unwrap()
            .operation
            .status,
        FlameJournalStatus::Applying(FlameApplyStage::PrepareTransition)
    );
    processor
        .canonical_storage
        .commit_state(&fixture.root_state)
        .await
        .unwrap();
    processor.engine.fail = true;
    processor.resume().await.unwrap();
    assert_eq!(processor.engine.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        processor.canonical_storage.get_tip().await.unwrap(),
        Some(fixture.arriving.header.block_tip())
    );
    assert_eq!(
        processor
            .consensus_storage
            .get_endorsements_for_block(endorsement.endorsement.block_tip())
            .await
            .unwrap(),
        accepted
    );
    assert!(
        processor
            .consensus_storage
            .get_pending_endorsements_for_block(endorsement.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(processor.journal.pending_entry().await.unwrap().is_none());
    assert_eq!(
        processor.cursor_storage.get_cursor().await.unwrap(),
        btc_tip(300)
    );
}

struct UnusedBitcoinProcessor;

impl BlockProcessor for UnusedBitcoinProcessor {
    type Error = std::convert::Infallible;

    async fn apply(&self, _: BtcBlockRecords) -> Result<BlockId, Self::Error> {
        panic!("unexpected Bitcoin block")
    }
    async fn resume(&self) -> Result<BlockId, Self::Error> {
        Ok(btc_tip(300))
    }
    async fn rollback(&self, _: BlockId) -> Result<BlockId, Self::Error> {
        panic!("unexpected rollback")
    }
}

#[tokio::test]
async fn handles_arrivals_through_the_consensus_manager() {
    let fixture = Fixture::new().await;
    fixture
        .pending(&included(0x51, &fixture.arriving, 100))
        .await;
    let canonical = fixture.processor.canonical_storage.clone();
    let journal = fixture.processor.journal.owner.clone();
    let manager = Arc::new(ConsensusManager::new(
        UnusedBitcoinProcessor,
        fixture.processor,
        journal.clone(),
    ));
    manager.startup().await.unwrap();
    manager
        .consensus_handle()
        .on_flame_block_arrival(fixture.arriving.header.block_tip())
        .await
        .unwrap();
    wait_until(|| async {
        journal
            .latest_entry()
            .await
            .unwrap()
            .is_some_and(|entry| entry.is_complete())
    })
    .await;
    manager.shutdown().await.unwrap();
    assert_eq!(
        canonical.get_tip().await.unwrap(),
        Some(fixture.arriving.header.block_tip())
    );
}

#[tokio::test]
async fn calculates_rewards_for_core_blocks_attached_by_an_arrival() {
    let mut fixture = Fixture::at_btc_tip(btc_tip(299)).await;
    let bitcoin = recovery::bitcoin_processor(&fixture).await;
    bitcoin
        .apply(BtcBlockRecords {
            id: btc_tip(300),
            burns: vec![],
            endorsements: vec![],
        })
        .await
        .unwrap();
    fixture
        .pending(&included(0x51, &fixture.arriving, 100))
        .await;
    fixture
        .processor
        .on_flame_block_arrival(fixture.arriving.header.block_tip())
        .await
        .unwrap();
    let mut state = fixture.root_state.clone();
    state.connect(&fixture.arriving).unwrap();
    let mut last = fixture.arriving.clone();
    for height in 2..=150 {
        last = core_block(&mut state, height, 99 + height, height as u8);
        fixture.register(last.clone(), 3, 0).await;
    }
    fixture.pending(&included(0x51, &last, 249)).await;
    fixture
        .processor
        .on_flame_block_arrival(last.header.block_tip())
        .await
        .unwrap();
    let rewards = fixture
        .processor
        .rewards_storage
        .get_pending_rewards(200.into())
        .await
        .unwrap();
    assert_eq!(rewards.len(), 1);
    assert_eq!(rewards[0].amount, 50 * flamechain::SPARKS_PER_FLAME);
    assert_eq!(
        fixture.processor.canonical_storage.get_tip().await.unwrap(),
        Some(last.header.block_tip())
    );
    assert_eq!(
        fixture.processor.cursor_storage.get_cursor().await.unwrap(),
        btc_tip(300)
    );
    let calls = fixture.processor.engine.calls.load(Ordering::SeqCst);
    bitcoin.rollback(btc_tip(300)).await.unwrap();
    assert_eq!(fixture.processor.engine.calls.load(Ordering::SeqCst), calls);
    let restored = fixture
        .processor
        .rewards_storage
        .get_pending_rewards(200.into())
        .await
        .unwrap();
    assert_eq!(restored.len(), rewards.len());
    for (actual, expected) in restored.iter().zip(&rewards) {
        assert_eq!(actual.amount, expected.amount);
        assert_eq!(
            actual.flame_predicate.to_point(),
            expected.flame_predicate.to_point()
        );
        assert_eq!(
            actual.access_predicate.to_point(),
            expected.access_predicate.to_point()
        );
    }
    assert_eq!(
        fixture.processor.canonical_storage.get_tip().await.unwrap(),
        Some(last.header.block_tip())
    );
}

mod recovery;

mod rollback;
