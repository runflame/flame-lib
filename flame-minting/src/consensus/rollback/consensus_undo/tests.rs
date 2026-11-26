use crate::consensus::ConsensusUndoManager;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use bitcoin::{
    Amount, BlockHash as BtcBlockHash, Transaction, TxOut, absolute, hashes::Hash, transaction,
};
use btc_integration::{Burn, BurnData, MinterP2wsh, watcher::BlockId};
use flamechain::{BlockHash, Blockchain, ChainParams, CoreBlockHeader};
use flamevm::Predicate;

use super::*;
use crate::consensus::{
    ConsensusOutcome, ConsensusStorage, engine::tests::endorsement_for_tip,
    storage::InMemoryConsensusStorage,
};

fn btc_tip() -> BlockId {
    BlockId {
        hash: BtcBlockHash::from_byte_array([1; 32]),
        height: 100,
    }
}

fn empty_outcome() -> ConsensusOutcome {
    ConsensusOutcome {
        accepted_endorsements: Vec::new(),
        removed_endorsements: Vec::new(),
        weighted_blocks: HashMap::new(),
        accepted_acquisitions: Vec::new(),
        double_signs: Vec::new(),
        pending_endorsements: BTreeMap::new(),
        removed_pending_endorsements: BTreeSet::new(),
    }
}

fn included(minter: u8, height: u32, hash: u8) -> IncludedEndorsement {
    IncludedEndorsement {
        btc_block: btc_tip(),
        endorsement: endorsement_for_tip(
            minter,
            CoreBlockTip {
                hash: BlockHash::from([hash; 32]),
                height: height.into(),
            },
        ),
    }
}

fn acquisition(amount: u64) -> IncludedAcquisition {
    let data = BurnData::new(
        [1; 32].into(),
        Predicate::opaque(Predicate::unspendable_key()),
    );
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: Vec::new(),
        output: vec![TxOut {
            value: Amount::from_sat(amount),
            script_pubkey: data.to_script(),
        }],
    };
    IncludedAcquisition {
        btc_block: btc_tip(),
        acquisition: Burn::from_tx(&transaction).pop().unwrap(),
    }
}

fn weighted_header(seed: u8, power: u64) -> WeightedBlockHeader {
    let mut chain = Blockchain::new(ChainParams::default()).unwrap();
    let mut header = chain.build_block([seed; 32], Vec::new()).unwrap().header;
    header.core_block = Some(CoreBlockHeader {
        height: 1.into(),
        target_btc_height: 100,
    });
    WeightedBlockHeader {
        header,
        parent_weight: 10,
        effective_power: power,
    }
}

#[tokio::test]
async fn restores_endorsement_weights_and_minter_status_after_double_sign() {
    let storage = InMemoryConsensusStorage::new();
    let manager = ConsensusUndoManager {
        consensus_storage: &storage,
    };
    let original = weighted_header(1, 16);
    let competing = weighted_header(2, 8);
    let mut descendant = weighted_header(3, 2);
    descendant.header.parent = original.block_tip().hash;
    descendant.header.height = 2;
    descendant.header.core_block.as_mut().unwrap().height = 2.into();
    descendant.parent_weight = 14;
    let old = WeightedEndorsement {
        original: IncludedEndorsement {
            btc_block: btc_tip(),
            endorsement: endorsement_for_tip(
                0x51,
                CoreBlockTip {
                    hash: original.block_tip().hash,
                    height: 1.into(),
                },
            ),
        },
        effective_minting_power: 16,
    };
    let minter = old.original.endorsement.auth().minter().p2wsh();
    let sign = DoubleSign {
        minter,
        target_flame_height: 1.into(),
        endorsements: vec![old.original.clone(), included(0x51, 1, 9)],
    };
    storage
        .store_endorsement(old.original.btc_block, &old)
        .await
        .unwrap();
    for weighted in [&original, &competing, &descendant] {
        storage
            .store_cumulative_weight(weighted.block_tip(), weighted)
            .await
            .unwrap();
    }
    let mut changed_original = original.clone();
    changed_original.effective_power = 0;
    let mut changed_descendant = descendant.clone();
    changed_descendant.parent_weight = 10;
    let outcome = ConsensusOutcome {
        removed_endorsements: vec![old.clone()],
        weighted_blocks: HashMap::from([
            (changed_original.block_tip(), changed_original),
            (changed_descendant.block_tip(), changed_descendant),
        ]),
        double_signs: vec![sign.clone()],
        ..empty_outcome()
    };
    let undo = manager.prepare(&outcome).await.unwrap();
    assert_eq!(undo.endorsements[&(minter, 1.into())], Some(old.clone()));
    assert_eq!(undo.double_signs[&(minter, 1.into())], None);
    assert_eq!(
        storage.get_block_tip_with_most_weight().await.unwrap(),
        Some(descendant.block_tip())
    );
    assert!(!storage.is_minter_double_signed(&minter).await.unwrap());

    storage.remove_endorsement(&old).await.unwrap();
    storage.add_double_sign(&sign).await.unwrap();
    for (&tip, weighted) in &outcome.weighted_blocks {
        storage
            .store_cumulative_weight(tip, weighted)
            .await
            .unwrap();
    }
    assert!(storage.is_minter_double_signed(&minter).await.unwrap());
    assert_eq!(
        storage
            .get_minter_endorsement_for_height(&minter, 1.into())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        storage.get_block_tip_with_most_weight().await.unwrap(),
        Some(competing.block_tip())
    );

    for _ in 0..2 {
        manager.restore(&undo).await.unwrap();
        assert_eq!(
            storage
                .get_minter_endorsement_for_height(&minter, 1.into())
                .await
                .unwrap(),
            Some(old.clone())
        );
        assert!(!storage.is_minter_double_signed(&minter).await.unwrap());
        assert_eq!(
            storage.get_block_tip_with_most_weight().await.unwrap(),
            Some(descendant.block_tip())
        );
        for weighted in [&original, &competing, &descendant] {
            assert_eq!(
                storage
                    .get_cumulative_weight(weighted.block_tip())
                    .await
                    .unwrap(),
                Some(weighted.clone())
            );
        }
        assert_eq!(manager.prepare(&outcome).await.unwrap(), undo);
    }
}

#[tokio::test]
async fn restores_existing_records_and_removes_new_records() {
    let storage = InMemoryConsensusStorage::new();
    let manager = ConsensusUndoManager {
        consensus_storage: &storage,
    };
    let existing_acquisition = acquisition(10);
    let new_acquisition = acquisition(20);
    let existing_pending = included(0x51, 2, 1);
    let new_pending = included(0x51, 2, 2);
    let accepted = WeightedEndorsement {
        original: included(0x52, 3, 3),
        effective_minting_power: 8,
    };
    let old_sign = DoubleSign {
        minter: existing_pending.endorsement.auth().minter().p2wsh(),
        target_flame_height: 1.into(),
        endorsements: vec![included(0x51, 1, 1), included(0x51, 1, 2)],
    };
    let mut new_sign = old_sign.clone();
    new_sign.endorsements.push(included(0x51, 1, 3));
    let new_weight = weighted_header(4, 8);
    storage
        .store_acquisition(btc_tip(), &existing_acquisition)
        .await
        .unwrap();
    storage
        .store_pending_endorsement(existing_pending.endorsement.block_tip(), &existing_pending)
        .await
        .unwrap();
    storage.add_double_sign(&old_sign).await.unwrap();
    let outcome = ConsensusOutcome {
        accepted_acquisitions: vec![existing_acquisition.clone(), new_acquisition.clone()],
        accepted_endorsements: vec![accepted.clone()],
        pending_endorsements: BTreeMap::from([
            (
                existing_pending.endorsement.block_tip(),
                vec![existing_pending.clone()],
            ),
            (
                new_pending.endorsement.block_tip(),
                vec![new_pending.clone()],
            ),
        ]),
        weighted_blocks: HashMap::from([(new_weight.block_tip(), new_weight.clone())]),
        double_signs: vec![new_sign.clone()],
        ..empty_outcome()
    };
    let undo = manager.prepare(&outcome).await.unwrap();
    assert_eq!(
        undo.acquisitions[&AcquisitionIndex::from(&existing_acquisition.acquisition)],
        Some(existing_acquisition.clone())
    );
    assert_eq!(
        undo.acquisitions[&AcquisitionIndex::from(&new_acquisition.acquisition)],
        None
    );
    assert_eq!(
        undo.double_signs[&(old_sign.minter, old_sign.target_flame_height)],
        Some(old_sign.clone())
    );

    storage
        .store_acquisition(btc_tip(), &new_acquisition)
        .await
        .unwrap();
    storage
        .store_endorsement(btc_tip(), &accepted)
        .await
        .unwrap();
    storage
        .store_pending_endorsement(new_pending.endorsement.block_tip(), &new_pending)
        .await
        .unwrap();
    storage
        .store_cumulative_weight(new_weight.block_tip(), &new_weight)
        .await
        .unwrap();
    storage.add_double_sign(&new_sign).await.unwrap();
    for _ in 0..2 {
        manager.restore(&undo).await.unwrap();
        assert_eq!(manager.prepare(&outcome).await.unwrap(), undo);
        assert_eq!(
            storage
                .get_acquisition(AcquisitionIndex::from(&new_acquisition.acquisition))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_minter_endorsement_for_height(
                    &accepted.original.endorsement.auth().minter().p2wsh(),
                    accepted.original.block_height()
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_pending_endorsement(
                    new_pending.endorsement.block_tip(),
                    EndorsementIndex::from(&new_pending.endorsement)
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_cumulative_weight(new_weight.block_tip())
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_double_sign(&old_sign.minter, old_sign.target_flame_height)
                .await
                .unwrap(),
            Some(old_sign.clone())
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ReadKey {
    Burn(AcquisitionIndex),
    Endorsement(MinterP2wsh, CoreFlameHeight),
    Pending(CoreBlockTip, EndorsementIndex),
    Weight(BlockTip),
    DoubleSign(MinterP2wsh, CoreFlameHeight),
}

#[tokio::test]
async fn restores_removed_pending_endorsements_and_preserves_previously_absent_keys() {
    let storage = InMemoryConsensusStorage::new();
    let removed = included(0x51, 1, 1);
    let absent = included(0x52, 1, 1);
    let untouched = included(0x53, 1, 1);
    let tip = removed.endorsement.block_tip();
    for endorsement in [&removed, &untouched] {
        storage
            .store_pending_endorsement(tip, endorsement)
            .await
            .unwrap();
    }
    let removed_key = (tip, EndorsementIndex::from(&removed.endorsement));
    let absent_key = (tip, EndorsementIndex::from(&absent.endorsement));
    let outcome = ConsensusOutcome {
        removed_pending_endorsements: BTreeSet::from([removed_key, absent_key]),
        ..ConsensusOutcome::default()
    };
    let manager = ConsensusUndoManager {
        consensus_storage: &storage,
    };
    let undo = manager.prepare(&outcome).await.unwrap();
    assert_eq!(
        undo.pending_endorsements,
        BTreeMap::from([(removed_key, Some(removed.clone())), (absent_key, None),])
    );
    for &(tip, index) in &outcome.removed_pending_endorsements {
        storage
            .restore_pending_endorsement(tip, index, None)
            .await
            .unwrap();
    }
    assert_eq!(
        storage
            .get_pending_endorsements_for_block(tip)
            .await
            .unwrap(),
        vec![untouched.clone()]
    );
    for _ in 0..2 {
        manager.restore(&undo).await.unwrap();
        assert_eq!(
            storage
                .get_pending_endorsement(removed_key.0, removed_key.1)
                .await
                .unwrap(),
            Some(removed.clone())
        );
        assert_eq!(
            storage
                .get_pending_endorsement(absent_key.0, absent_key.1)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_pending_endorsement(tip, EndorsementIndex::from(&untouched.endorsement))
                .await
                .unwrap(),
            Some(untouched.clone())
        );
    }
}

#[derive(Default)]
struct ReadOnceStorage {
    reads: Mutex<BTreeSet<ReadKey>>,
}

impl ReadOnceStorage {
    fn read<T>(&self, key: ReadKey) -> Result<Option<T>, &'static str> {
        if !self.reads.lock().unwrap().insert(key) {
            return Err("key read more than once");
        }
        Ok(None)
    }
}

impl ConsensusStorage for ReadOnceStorage {
    type Error = &'static str;

    async fn get_acquisition(
        &self,
        index: AcquisitionIndex,
    ) -> Result<Option<IncludedAcquisition>, Self::Error> {
        self.read(ReadKey::Burn(index))
    }

    async fn get_minter_endorsement_for_height(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<WeightedEndorsement>, Self::Error> {
        self.read(ReadKey::Endorsement(*minter, height))
    }

    async fn get_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
    ) -> Result<Option<IncludedEndorsement>, Self::Error> {
        self.read(ReadKey::Pending(tip, index))
    }

    async fn get_cumulative_weight(
        &self,
        tip: BlockTip,
    ) -> Result<Option<WeightedBlockHeader>, Self::Error> {
        self.read(ReadKey::Weight(tip))
    }

    async fn get_double_sign(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<DoubleSign>, Self::Error> {
        self.read(ReadKey::DoubleSign(*minter, height))
    }
}

#[tokio::test]
async fn captures_each_key_once_including_removed_and_accepted_endorsements() {
    let storage = ReadOnceStorage::default();
    let manager = ConsensusUndoManager {
        consensus_storage: &storage,
    };
    let acquisition = acquisition(10);
    let first = included(0x51, 1, 1);
    let second = included(0x51, 1, 2);
    let minter = first.endorsement.auth().minter().p2wsh();
    let sign = DoubleSign {
        minter,
        target_flame_height: 1.into(),
        endorsements: vec![first.clone(), second.clone()],
    };
    let header = weighted_header(1, 8);
    let accepted = WeightedEndorsement {
        original: first.clone(),
        effective_minting_power: 8,
    };
    let removed = WeightedEndorsement {
        original: second,
        effective_minting_power: 16,
    };
    let outcome = ConsensusOutcome {
        accepted_acquisitions: vec![acquisition.clone(), acquisition.clone()],
        accepted_endorsements: vec![accepted.clone(), accepted],
        removed_endorsements: vec![removed.clone(), removed],
        pending_endorsements: BTreeMap::from([(
            first.endorsement.block_tip(),
            vec![first.clone(), first.clone()],
        )]),
        removed_pending_endorsements: BTreeSet::from([(
            first.endorsement.block_tip(),
            EndorsementIndex::from(&first.endorsement),
        )]),
        weighted_blocks: HashMap::from([(header.block_tip(), header.clone())]),
        double_signs: vec![sign.clone(), sign],
        ..empty_outcome()
    };
    let undo = manager.prepare(&outcome).await.unwrap();
    assert_eq!(undo.acquisitions.len(), 1);
    assert_eq!(undo.endorsements.len(), 1);
    assert_eq!(undo.pending_endorsements.len(), 1);
    assert_eq!(undo.weighted_blocks.len(), 1);
    assert_eq!(undo.double_signs.len(), 1);
    assert_eq!(
        *storage.reads.lock().unwrap(),
        BTreeSet::from([
            ReadKey::Burn(AcquisitionIndex::from(&acquisition.acquisition)),
            ReadKey::Endorsement(minter, 1.into()),
            ReadKey::Pending(
                first.endorsement.block_tip(),
                EndorsementIndex::from(&first.endorsement)
            ),
            ReadKey::Weight(header.block_tip()),
            ReadKey::DoubleSign(minter, 1.into()),
        ])
    );
}

struct UnavailableStorage;

impl ConsensusStorage for UnavailableStorage {
    type Error = &'static str;

    async fn get_acquisition(
        &self,
        _: AcquisitionIndex,
    ) -> Result<Option<IncludedAcquisition>, Self::Error> {
        Err("storage unavailable")
    }
}

#[tokio::test]
async fn preparation_propagates_read_failure_without_attempting_writes() {
    let manager = ConsensusUndoManager {
        consensus_storage: &UnavailableStorage,
    };
    let outcome = ConsensusOutcome {
        accepted_acquisitions: vec![acquisition(10)],
        ..empty_outcome()
    };
    assert_eq!(manager.prepare(&outcome).await, Err("storage unavailable"));
}

#[tokio::test]
async fn empty_outcome_needs_no_storage_operations() {
    let manager = ConsensusUndoManager {
        consensus_storage: &UnavailableStorage,
    };
    let undo = manager.prepare(&empty_outcome()).await.unwrap();
    assert_eq!(undo, ConsensusUndo::default());
    manager.restore(&undo).await.unwrap();
}

struct InterruptedStorage {
    inner: InMemoryConsensusStorage,
    fail_once: AtomicBool,
}

impl ConsensusStorage for InterruptedStorage {
    type Error = &'static str;

    async fn restore_acquisition(
        &self,
        index: AcquisitionIndex,
        previous: Option<&IncludedAcquisition>,
    ) -> Result<(), Self::Error> {
        self.inner
            .restore_acquisition(index, previous)
            .await
            .map_err(|_| "acquisition restore failed")
    }

    async fn restore_endorsement(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
        previous: Option<&WeightedEndorsement>,
    ) -> Result<(), Self::Error> {
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err("endorsement restore interrupted");
        }
        self.inner
            .restore_endorsement(minter, height, previous)
            .await
            .map_err(|_| "endorsement restore failed")
    }
}

#[tokio::test]
async fn retry_finishes_a_partially_restored_undo() {
    let storage = InterruptedStorage {
        inner: InMemoryConsensusStorage::new(),
        fail_once: AtomicBool::new(true),
    };
    let snapshot_manager = ConsensusUndoManager {
        consensus_storage: &storage.inner,
    };
    let manager = ConsensusUndoManager {
        consensus_storage: &storage,
    };
    let acquisition = acquisition(10);
    let accepted = WeightedEndorsement {
        original: included(0x51, 1, 1),
        effective_minting_power: 8,
    };
    let index = AcquisitionIndex::from(&acquisition.acquisition);
    let minter = accepted.original.endorsement.auth().minter().p2wsh();
    let outcome = ConsensusOutcome {
        accepted_acquisitions: vec![acquisition.clone()],
        accepted_endorsements: vec![accepted.clone()],
        ..empty_outcome()
    };
    let undo = snapshot_manager.prepare(&outcome).await.unwrap();
    storage
        .inner
        .store_acquisition(btc_tip(), &acquisition)
        .await
        .unwrap();
    storage
        .inner
        .store_endorsement(btc_tip(), &accepted)
        .await
        .unwrap();
    assert_eq!(
        manager.restore(&undo).await,
        Err("endorsement restore interrupted")
    );
    assert_eq!(storage.inner.get_acquisition(index).await.unwrap(), None);
    assert_eq!(
        storage
            .inner
            .get_minter_endorsement_for_height(&minter, 1.into())
            .await
            .unwrap(),
        Some(accepted)
    );
    manager.restore(&undo).await.unwrap();
    assert_eq!(
        storage
            .inner
            .get_minter_endorsement_for_height(&minter, 1.into())
            .await
            .unwrap(),
        None
    );
    assert_eq!(snapshot_manager.prepare(&outcome).await.unwrap(), undo);
}
