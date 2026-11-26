use super::*;
use crate::orchestrator::consensus::ConsensusLoop;
use crate::{
    block_processing::{
        BitcoinBlockProcessor,
        journal::{
            FlameTransition, RewardsChanges,
            consensus::{
                BitcoinJournal, ConsensusJournal, ConsensusOperation, FlameJournal,
                FlameJournalEntry, InMemoryBitcoinJournal, InMemoryConsensusJournal,
                InMemoryFlameJournal,
            },
        },
    },
    consensus::{ConsensusUndo, MintingOutcome},
};
use btc_integration::watcher::impls::{MemoryBtc, RecentBlocks};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Begin,
    Transition,
    Rewards,
    Advance(FlameJournalStatus),
}

#[derive(Clone)]
pub(super) struct FaultJournal {
    pub(super) owner: InMemoryConsensusJournal,
    inner: InMemoryFlameJournal,
    failure: Arc<StdMutex<Option<(Event, bool)>>>,
}

impl FaultJournal {
    pub(super) fn new(initial: BlockId) -> Self {
        let owner = InMemoryConsensusJournal::new(initial);
        Self {
            inner: owner.flame_journal(),
            owner,
            failure: Arc::new(StdMutex::new(None)),
        }
    }

    fn fail(&self, event: Event, after: bool) {
        *self.failure.lock().unwrap() = Some((event, after));
    }

    fn check(&self, event: Event, after: bool) -> Result<(), &'static str> {
        let mut failure = self.failure.lock().unwrap();
        if *failure == Some((event, after)) {
            failure.take();
            return Err("injected journal failure");
        }
        Ok(())
    }
}

impl FlameJournal for FaultJournal {
    type Error = &'static str;

    async fn begin_apply(
        &self,
        tip: BlockTip,
        btc_tip: BlockId,
        outcome: &ConsensusOutcome,
        undo: &ConsensusUndo,
    ) -> Result<OperationId, Self::Error> {
        self.check(Event::Begin, false)?;
        let id = self
            .inner
            .begin_apply(tip, btc_tip, outcome, undo)
            .await
            .unwrap();
        self.check(Event::Begin, true)?;
        Ok(id)
    }

    async fn pending_entry(&self) -> Result<Option<FlameJournalEntry>, Self::Error> {
        Ok(self.inner.pending_entry().await.unwrap())
    }

    async fn store_transition(
        &self,
        id: OperationId,
        transition: &FlameTransition,
    ) -> Result<(), Self::Error> {
        self.check(Event::Transition, false)?;
        self.inner.store_transition(id, transition).await.unwrap();
        self.check(Event::Transition, true)
    }

    async fn store_rewards(
        &self,
        id: OperationId,
        rewards: &RewardsChanges,
    ) -> Result<(), Self::Error> {
        self.check(Event::Rewards, false)?;
        self.inner.store_rewards(id, rewards).await.unwrap();
        self.check(Event::Rewards, true)
    }

    async fn complete_apply_stage(
        &self,
        id: OperationId,
        stage: FlameApplyStage,
    ) -> Result<(), Self::Error> {
        let event = Event::Advance(FlameJournalStatus::Applying(stage));
        self.check(event, false)?;
        self.inner.complete_apply_stage(id, stage).await.unwrap();
        self.check(event, true)
    }
}

impl FaultJournal {
    pub(super) async fn latest_applied(
        &self,
        tip: BlockId,
    ) -> Result<Option<FlameJournalEntry>, &'static str> {
        Ok(self
            .owner
            .latest_entry()
            .await
            .unwrap()
            .and_then(|entry| match entry.operation {
                ConsensusOperation::Flame(operation)
                    if operation.btc_tip == tip
                        && operation.status == FlameJournalStatus::Applied =>
                {
                    Some(FlameJournalEntry {
                        id: entry.id,
                        operation,
                    })
                }
                _ => None,
            }))
    }
}

fn restart(processor: Processor) -> Processor {
    FlameBlockProcessor::new(
        processor.engine,
        processor.consensus_storage,
        processor.chain,
        processor.canonical_storage,
        processor.rewards_storage,
        processor.cursor_storage,
        processor.rewards_engine,
        processor.journal,
    )
}

#[tokio::test]
async fn resumes_each_apply_stage_without_recomputing_consensus() {
    let events = [
        Event::Begin,
        Event::Transition,
        Event::Rewards,
        Event::Advance(FlameJournalStatus::Applying(FlameApplyStage::Consensus)),
        Event::Advance(FlameJournalStatus::Applying(
            FlameApplyStage::PrepareTransition,
        )),
        Event::Advance(FlameJournalStatus::Applying(
            FlameApplyStage::ApplyTransition,
        )),
        Event::Advance(FlameJournalStatus::Applying(
            FlameApplyStage::PrepareRewards,
        )),
        Event::Advance(FlameJournalStatus::Applying(FlameApplyStage::ApplyRewards)),
        Event::Advance(FlameJournalStatus::Applying(FlameApplyStage::RemovePending)),
    ];
    for event in events {
        for after in [false, true] {
            let fixture = Fixture::new().await;
            let endorsement = included(0x51, &fixture.arriving, 100);
            fixture.pending(&endorsement).await;
            let tip = fixture.arriving.header.block_tip();
            fixture.processor.journal.fail(event, after);
            assert!(
                fixture.processor.on_flame_block_arrival(tip).await.is_err(),
                "{event:?}/{after}"
            );
            let processor = restart(fixture.processor);
            processor.resume().await.unwrap();
            processor.on_flame_block_arrival(tip).await.unwrap();
            let expected_calls = if event == Event::Begin && !after {
                2
            } else {
                1
            };
            assert_eq!(
                processor.engine.calls.load(Ordering::SeqCst),
                expected_calls,
                "{event:?}/{after}"
            );
            assert_eq!(
                processor.canonical_storage.get_tip().await.unwrap(),
                Some(tip)
            );
            assert!(
                processor
                    .consensus_storage
                    .get_pending_endorsements_for_block(endorsement.endorsement.block_tip())
                    .await
                    .unwrap()
                    .is_empty()
            );
            let accepted = processor
                .consensus_storage
                .get_endorsements_for_block(endorsement.endorsement.block_tip())
                .await
                .unwrap();
            assert_eq!(accepted.len(), 1);
            assert_eq!(accepted[0].effective_minting_power, 8);
            assert!(processor.journal.pending_entry().await.unwrap().is_none());
        }
    }
}

pub(super) struct PendingEngine(Engine);

impl ConsensusEngine for PendingEngine {
    type Error = &'static str;

    async fn get_rollback_plan(
        &self,
        input: BitcoinRollbackInput,
    ) -> Result<BitcoinRollbackPlan, Self::Error> {
        self.0
            .get_rollback_plan(input)
            .await
            .map_err(|_| "rollback failed")
    }

    async fn get_endorsements_outcome(
        &self,
        _: Vec<IncludedEndorsement>,
    ) -> Result<ConsensusOutcome, Self::Error> {
        unreachable!()
    }

    async fn get_minting_outcome(
        &self,
        block: BtcBlockRecords,
    ) -> Result<MintingOutcome, Self::Error> {
        let mut consensus = ConsensusOutcome::default();
        for endorsement in block.endorsements {
            consensus
                .pending_endorsements
                .entry(endorsement.block_tip())
                .or_default()
                .push(IncludedEndorsement {
                    btc_block: block.id,
                    endorsement,
                });
        }
        Ok(MintingOutcome {
            consensus,
            next_btc_cursor: block.id,
        })
    }
}

pub(super) type BitcoinProcessor = BitcoinBlockProcessor<
    PendingEngine,
    InMemoryConsensusStorage,
    Chain,
    InMemoryBitcoinJournal,
    InMemoryCanonicalStorage,
    InMemoryRewardsStorage,
    Cursor,
    InMemoryChainStorage,
>;

pub(super) async fn bitcoin_processor(fixture: &Fixture) -> BitcoinProcessor {
    let processor = &fixture.processor;
    BitcoinBlockProcessor::new(
        PendingEngine(MintingEngineFactory {
            protocol_params: processor.engine.inner.protocol_params.clone(),
            consensus_storage: processor.engine.inner.consensus_storage.clone(),
            chain_storage: processor.engine.inner.chain_storage.clone(),
            canonical_storage: processor.engine.inner.canonical_storage.clone(),
        }),
        processor.consensus_storage.clone(),
        processor.chain.clone(),
        processor.journal.owner.bitcoin_journal(),
        processor.canonical_storage.clone(),
        processor.rewards_storage.clone(),
        processor.cursor_storage.clone(),
        RewardsEngine {
            chain_storage: processor.rewards_engine.chain_storage.clone(),
            consensus_storage: processor.rewards_engine.consensus_storage.clone(),
            protocol_params: processor.rewards_engine.protocol_params.clone(),
        },
    )
}

async fn apply_pending(processor: &BitcoinProcessor, block: &Block, minter: u8, id: BlockId) {
    processor
        .apply(BtcBlockRecords {
            id,
            burns: vec![],
            endorsements: vec![included(minter, block, id.height).endorsement],
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn btc_rollback_preserves_surviving_endorsements_without_replaying_arrivals() {
    let fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    fixture.add_acquisition(0x52).await;
    let bitcoin = bitcoin_processor(&fixture).await;
    apply_pending(&bitcoin, &fixture.arriving, 0x51, btc_tip(100)).await;
    apply_pending(&bitcoin, &fixture.arriving, 0x52, btc_tip(101)).await;
    let tip = fixture.arriving.header.block_tip();
    let core_tip = included(0x51, &fixture.arriving, 100)
        .endorsement
        .block_tip();
    let calls = fixture.processor.engine.calls.clone();
    let consensus = fixture.processor.consensus_storage.clone();
    let canonical = fixture.processor.canonical_storage.clone();
    let owner = fixture.processor.journal.owner.clone();
    let cursor = fixture.processor.cursor_storage.clone();
    let manager = Arc::new(ConsensusManager::new(
        bitcoin,
        fixture.processor,
        owner.clone(),
    ));
    manager.startup().await.unwrap();
    manager.new_flame_block(tip).unwrap();
    wait_until(|| async {
        owner.latest_entry().await.unwrap().is_some_and(|entry| {
            matches!(entry.operation, ConsensusOperation::Flame(_)) && entry.is_complete()
        })
    })
    .await;
    assert_eq!(
        consensus
            .get_endorsements_for_block(core_tip)
            .await
            .unwrap()
            .len(),
        2
    );
    manager.bitcoin_reorg(vec![btc_tip(101)], vec![]).unwrap();
    wait_until(|| async {
        cursor.get_cursor().await.unwrap() == btc_tip(100)
            && owner
                .latest_entry()
                .await
                .unwrap()
                .is_some_and(|entry| entry.is_complete())
    })
    .await;
    let remaining = consensus
        .get_endorsements_for_block(core_tip)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].original.btc_block, btc_tip(100));
    assert_eq!(canonical.get_tip().await.unwrap(), Some(tip));
    manager.bitcoin_reorg(vec![btc_tip(100)], vec![]).unwrap();
    wait_until(|| async {
        cursor.get_cursor().await.unwrap() == btc_tip(99)
            && owner
                .latest_entry()
                .await
                .unwrap()
                .is_some_and(|entry| entry.is_complete())
    })
    .await;
    manager.shutdown().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        consensus
            .get_endorsements_for_block(core_tip)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        consensus
            .get_pending_endorsements_for_block(core_tip)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        canonical.get_tip().await.unwrap(),
        Some(fixture.root.header.block_tip())
    );
}

#[tokio::test]
async fn invalid_btc_rollback_does_not_unwind_arrivals() {
    let fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    let bitcoin = bitcoin_processor(&fixture).await;
    apply_pending(&bitcoin, &fixture.arriving, 0x51, btc_tip(100)).await;
    let tip = fixture.arriving.header.block_tip();
    let canonical = fixture.processor.canonical_storage.clone();
    let journal = fixture.processor.journal.clone();
    let owner = fixture.processor.journal.owner.clone();
    let manager = Arc::new(ConsensusManager::new(
        bitcoin,
        fixture.processor,
        owner.clone(),
    ));
    manager.startup().await.unwrap();
    manager.new_flame_block(tip).unwrap();
    wait_until(|| async {
        owner.latest_entry().await.unwrap().is_some_and(|entry| {
            matches!(entry.operation, ConsensusOperation::Flame(_)) && entry.is_complete()
        })
    })
    .await;
    manager.bitcoin_reorg(vec![btc_tip(99)], vec![]).unwrap();
    assert_eq!(manager.stopped().await, crate::ConsensusStatus::Failed);
    assert!(manager.shutdown().await.is_err());
    assert!(journal.pending_entry().await.unwrap().is_none());
    assert!(
        journal
            .latest_applied(btc_tip(100))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(canonical.get_tip().await.unwrap(), Some(tip));
}

#[tokio::test]
async fn startup_resumes_an_arrival_before_accepting_the_next_bitcoin_block() {
    let fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    let bitcoin = bitcoin_processor(&fixture).await;
    apply_pending(&bitcoin, &fixture.arriving, 0x51, btc_tip(100)).await;
    let tip = fixture.arriving.header.block_tip();
    let journal = fixture.processor.journal.clone();
    let calls = fixture.processor.engine.calls.clone();
    let canonical = fixture.processor.canonical_storage.clone();
    let owner = fixture.processor.journal.owner.clone();
    let cursor = fixture.processor.cursor_storage.clone();
    let manager = Arc::new(ConsensusManager::new(
        bitcoin,
        fixture.processor,
        owner.clone(),
    ));
    manager.startup().await.unwrap();
    journal.fail(
        Event::Advance(FlameJournalStatus::Applying(FlameApplyStage::Consensus)),
        true,
    );
    manager.new_flame_block(tip).unwrap();
    assert_eq!(manager.stopped().await, crate::ConsensusStatus::Failed);
    assert!(manager.shutdown().await.is_err());
    let arrival = journal.owner.latest_entry().await.unwrap().unwrap();
    assert!(!arrival.is_complete());
    assert!(matches!(arrival.operation, ConsensusOperation::Flame(_)));
    let next = BtcBlockRecords {
        id: btc_tip(101),
        burns: vec![],
        endorsements: vec![],
    };
    assert_eq!(manager.startup().await.unwrap(), btc_tip(100));
    manager.new_bitcoin_block(next.clone()).unwrap();
    wait_until(|| async { cursor.get_cursor().await.unwrap() == btc_tip(101) }).await;
    manager.shutdown().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(canonical.get_tip().await.unwrap(), Some(tip));
    let latest = journal.owner.latest_entry().await.unwrap().unwrap();
    assert_eq!(latest.id, OperationId(arrival.id.0 + 1));
    assert!(latest.is_complete());
    let ConsensusOperation::Bitcoin(operation) = latest.operation else {
        panic!()
    };
    assert_eq!(operation.block, next);
    let original = journal
        .owner
        .bitcoin_journal()
        .get_entry(btc_tip(100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.operation.block.endorsements.len(), 1);
    assert!(journal.pending_entry().await.unwrap().is_none());
}

#[tokio::test]
async fn consensus_continues_a_reorg_from_the_cursor_recovered_after_a_rollback_failure() {
    // Consensus has applied the Bitcoin blocks 100 and 101 on top of 99.
    let btc = MemoryBtc::new();
    btc.mine_n(101);
    let [lowest, first, second] = [99, 100, 101].map(|height| btc.block_at(height).unwrap());
    let fixture = Fixture::at_btc_tip(lowest).await;
    fixture.add_acquisition(0x52).await;
    let bitcoin = bitcoin_processor(&fixture).await;
    apply_pending(&bitcoin, &fixture.arriving, 0x51, first).await;
    apply_pending(&bitcoin, &fixture.arriving, 0x52, second).await;
    let cursor = fixture.processor.cursor_storage.clone();
    let journal = fixture.processor.journal.owner.clone();
    let storage = fixture.processor.consensus_storage.clone();
    let canonical = fixture.processor.canonical_storage.clone();
    let calls = fixture.processor.engine.calls.clone();
    let chain_failure = fixture.processor.chain.fail.clone();
    let tip = fixture.arriving.header.block_tip();
    let core_tip = included(0x51, &fixture.arriving, 100)
        .endorsement
        .block_tip();
    let consensus = ConsensusLoop::new(
        ConsensusManager::new(bitcoin, fixture.processor, journal.clone()),
        lowest,
    );
    let handle = consensus.consensus_handle();
    consensus
        .startup(btc.source(), RecentBlocks::default())
        .await
        .unwrap();
    handle.on_flame_block_arrival(tip).await.unwrap();
    wait_until(|| async {
        journal.latest_entry().await.unwrap().is_some_and(|entry| {
            matches!(entry.operation, ConsensusOperation::Flame(_)) && entry.is_complete()
        })
    })
    .await;
    chain_failure.store(true, Ordering::SeqCst);
    // Bitcoin replaces the blocks 100 and 101 with one block. The rollback
    // of block 101 fails, and so does every attempt of the loop to start
    // consensus again.
    let target = btc.reorg(2, 1);
    wait_until(|| async {
        let pending = journal.bitcoin_journal().pending_entry().await;
        pending.unwrap().is_some()
    })
    .await;
    let interrupted = journal
        .bitcoin_journal()
        .pending_entry()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        interrupted.operation.entry.status,
        crate::block_processing::journal::JournalStatus::RollingBack(
            crate::block_processing::journal::RollbackStage::PrepareRewards
        )
    );
    assert!(interrupted.operation.rollback_plan.is_some());
    assert_eq!(cursor.get_cursor().await.unwrap(), second);
    // Nobody starts consensus again: once the failure is gone, the loop
    // does, and the watcher goes back to the block consensus is at.
    chain_failure.store(false, Ordering::SeqCst);
    wait_until(|| async { cursor.get_cursor().await.unwrap() == target }).await;
    consensus.shutdown().await.unwrap();
    assert!(
        journal
            .bitcoin_journal()
            .pending_entry()
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_endorsements_for_block(core_tip)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        canonical.get_tip().await.unwrap(),
        Some(fixture.root.header.block_tip())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
