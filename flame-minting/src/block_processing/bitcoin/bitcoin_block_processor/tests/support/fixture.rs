use super::*;

impl Fixture {
    pub(in super::super) async fn new() -> Self {
        let ancestor = Blockchain::new(ChainParams::default()).unwrap();
        let mut old = ancestor.clone();
        let old_first = old.build_block([1; 32], Vec::new()).unwrap();
        old.connect(&old_first).unwrap();
        let old_intermediate = old.clone();
        let old_second = old.build_block([2; 32], Vec::new()).unwrap();
        old.connect(&old_second).unwrap();
        let mut new = ancestor.clone();
        let mut new_first = new.build_block([3; 32], Vec::new()).unwrap();
        new_first.header.core_block = Some(CoreBlockHeader {
            height: 1.into(),
            target_btc_height: 101,
        });
        new.connect(&new_first).unwrap();
        let new_intermediate = new.clone();
        let new_second = new.build_block([4; 32], Vec::new()).unwrap();
        new.connect(&new_second).unwrap();
        let transition = FlameTransition {
            previous_tip: tip(&old),
            next_tip: tip(&new),
            path: ChainPath {
                detach: vec![old_second.header.block_tip(), old_first.header.block_tip()],
                attach: vec![new_first.header.block_tip(), new_second.header.block_tip()],
            },
            attached_core_headers: vec![new_first.header.clone()],
        };
        let inner = InMemoryConsensusStorage::new();
        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        let mut header = chain.build_block([1; 32], Vec::new()).unwrap().header;
        header.core_block = Some(CoreBlockHeader {
            height: 1.into(),
            target_btc_height: 100,
        });
        let mut weighted = WeightedBlockHeader {
            header,
            parent_weight: 10,
            effective_power: 16,
        };
        inner
            .store_cumulative_weight(weighted.block_tip(), &weighted)
            .await
            .unwrap();
        weighted.effective_power = 0;
        let old_endorsement = WeightedEndorsement {
            original: included(0x51, 1),
            effective_minting_power: 16,
        };
        inner
            .store_endorsement(old_endorsement.original.btc_block, &old_endorsement)
            .await
            .unwrap();
        let pending = included(0x53, 3);
        let resolved_pending = included(0x54, 3);
        inner
            .store_pending_endorsement(resolved_pending.endorsement.block_tip(), &resolved_pending)
            .await
            .unwrap();
        let outcome = MintingOutcome {
            consensus: ConsensusOutcome {
                removed_endorsements: vec![old_endorsement.clone()],
                accepted_endorsements: vec![WeightedEndorsement {
                    original: included(0x52, 2),
                    effective_minting_power: 8,
                }],
                accepted_acquisitions: vec![acquisition(10), acquisition(20)],
                weighted_blocks: HashMap::from([(weighted.block_tip(), weighted)]),
                double_signs: vec![DoubleSign {
                    minter: old_endorsement.original.endorsement.auth().minter().p2wsh(),
                    target_flame_height: 1.into(),
                    endorsements: vec![old_endorsement.original, included(0x51, 9)],
                }],
                pending_endorsements: BTreeMap::from([(
                    pending.endorsement.block_tip(),
                    vec![pending],
                )]),
                removed_pending_endorsements: BTreeSet::from([(
                    resolved_pending.endorsement.block_tip(),
                    EndorsementIndex::from(&resolved_pending.endorsement),
                )]),
            },
            next_btc_cursor: btc_tip(101),
        };
        let undo = ConsensusUndoManager {
            consensus_storage: &inner,
        }
        .prepare(&outcome.consensus)
        .await
        .unwrap();
        let owner = InMemoryConsensusJournal::new(btc_tip(100));
        let journal = owner.bitcoin_journal();
        let consensus = Consensus(Arc::new(ConsensusData {
            inner,
            journal: journal.clone(),
            outcome: outcome.clone(),
            undo: undo.clone(),
            previous_winner: tip(&old),
            winner: tip(&new),
            selections: AtomicUsize::new(0),
            reject_selection: AtomicBool::new(false),
            reject_reads: AtomicBool::new(false),
            writes: Mutex::new(Vec::new()),
            fail_after_write: Mutex::new(None),
        }));
        let processor = BitcoinBlockProcessor::new(
            Engine {
                rollback: BitcoinRollbackPlan {
                    endorsements: undo.endorsements.clone(),
                    pending_endorsements: undo.pending_endorsements.clone(),
                    double_signs: undo.double_signs.clone(),
                    weighted_blocks: undo
                        .weighted_blocks
                        .into_iter()
                        .filter_map(|(tip, value)| value.map(|value| (tip, value)))
                        .collect(),
                    ..BitcoinRollbackPlan::default()
                },
                rollback_calls: AtomicUsize::new(0),
                outcome,
                calls: AtomicUsize::new(0),
            },
            consensus.clone(),
            Chain {
                blocks: [old_first, old_second, new_first, new_second]
                    .into_iter()
                    .map(|block| (block.header.block_tip(), Arc::new(block)))
                    .collect(),
                transition: transition.clone(),
                plans: AtomicUsize::new(0),
                reject_plans: AtomicBool::new(false),
            },
            Journal {
                inner: journal.clone(),
                fault: Mutex::new(None),
            },
            Storage {
                state: Mutex::new(StoredState {
                    chain: old.clone(),
                    commits: Vec::new(),
                    failure: None,
                }),
                journal: journal.clone(),
                transition,
            },
            Rewards {
                inner: InMemoryRewardsStorage::new(),
                journal: journal.clone(),
                fail_after_write: AtomicBool::new(false),
            },
            Cursor {
                inner: InMemoryCursorStorage::new(btc_tip(100)),
                journal,
                fault: Mutex::new(None),
            },
            RewardsEngine {
                chain_storage: Arc::new(TestChainStorage::default()),
                consensus_storage: Arc::new(consensus),
                protocol_params: protocol_params(),
            },
        );
        Self {
            processor,
            journal: owner,
            states: vec![old, old_intermediate, ancestor, new_intermediate, new],
        }
    }
    pub(in super::super) fn block(&self) -> BtcBlockRecords {
        BtcBlockRecords {
            id: btc_tip(101),
            burns: vec![],
            endorsements: vec![],
        }
    }
    pub(in super::super) async fn entry(&self) -> BitcoinBlockJournalEntry {
        self.processor
            .journal
            .get_entry(btc_tip(101))
            .await
            .unwrap()
            .unwrap()
            .operation
            .entry
    }
    pub(in super::super) fn allow_planning(&self) {
        self.processor
            .consensus_storage
            .reject_reads
            .store(false, Ordering::SeqCst);
        self.processor
            .consensus_storage
            .reject_selection
            .store(false, Ordering::SeqCst);
        self.processor
            .chain
            .reject_plans
            .store(false, Ordering::SeqCst);
    }
    pub(in super::super) async fn no_replanning(&self) {
        self.processor
            .consensus_storage
            .reject_reads
            .store(true, Ordering::SeqCst);
        let planned = self.entry().await.flame_transition.is_some();
        self.processor
            .consensus_storage
            .reject_selection
            .store(planned, Ordering::SeqCst);
        self.processor
            .chain
            .reject_plans
            .store(planned, Ordering::SeqCst);
    }
    pub(in super::super) async fn snapshot(&self) -> ConsensusUndo {
        ConsensusUndoManager {
            consensus_storage: &self.processor.consensus_storage.inner,
        }
        .prepare(&self.processor.engine.outcome.consensus)
        .await
        .unwrap()
    }
    pub(in super::super) async fn assert_applied(&self) {
        let entry = self.entry().await;
        assert_eq!(entry.status, JournalStatus::Applied);
        assert_eq!(entry.consensus_undo, self.processor.consensus_storage.undo);
        assert_eq!(
            entry.flame_transition,
            Some(self.processor.chain.transition.clone())
        );
        assert!(entry.rewards_changes.is_some());
        for &(tip, index) in &entry.minting_outcome.consensus.removed_pending_endorsements {
            assert_eq!(
                self.processor
                    .consensus_storage
                    .inner
                    .get_pending_endorsement(tip, index)
                    .await
                    .unwrap(),
                None
            );
        }
        assert!(
            self.processor
                .journal
                .pending_entry()
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            self.processor.cursor_storage.get_cursor().await.unwrap(),
            btc_tip(101)
        );
        let state = self.processor.canonical_storage.state.lock().unwrap();
        assert_eq!(tip(&state.chain), tip(&self.states[4]));
        assert_eq!(
            state.chain.state_commitment(),
            self.states[4].state_commitment()
        );
    }
    pub(in super::super) async fn assert_restored(&self) {
        assert_eq!(self.entry().await.status, JournalStatus::RolledBack);
        assert_eq!(self.snapshot().await, self.processor.consensus_storage.undo);
        assert_eq!(
            self.processor.cursor_storage.get_cursor().await.unwrap(),
            btc_tip(100)
        );
        let state = self.processor.canonical_storage.state.lock().unwrap();
        assert_eq!(tip(&state.chain), tip(&self.states[0]));
        assert_eq!(
            state.chain.state_commitment(),
            self.states[0].state_commitment()
        );
    }
}

pub(in super::super) struct Fixture {
    pub(in super::super) processor: Processor,
    pub(in super::super) journal: InMemoryConsensusJournal,
    pub(in super::super) states: Vec<Blockchain>,
}
