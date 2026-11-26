use super::*;
use crate::block_processing::journal::BitcoinJournal;
use crate::block_processing::{BitcoinBlockProcessor, journal::consensus::InMemoryBitcoinJournal};
use crate::consensus::{AcquisitionIndex, BitcoinRollbackInput};
use std::collections::BTreeMap;

type BitcoinProcessor = BitcoinBlockProcessor<
    Engine,
    InMemoryConsensusStorage,
    Chain,
    InMemoryBitcoinJournal,
    InMemoryCanonicalStorage,
    InMemoryRewardsStorage,
    Cursor,
    InMemoryChainStorage,
>;

fn bitcoin_processor(fixture: &Fixture) -> BitcoinProcessor {
    let processor = &fixture.processor;
    BitcoinBlockProcessor::new(
        MintingEngineFactory {
            protocol_params: processor.engine.inner.protocol_params.clone(),
            consensus_storage: processor.engine.inner.consensus_storage.clone(),
            chain_storage: processor.engine.inner.chain_storage.clone(),
            canonical_storage: processor.engine.inner.canonical_storage.clone(),
        },
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

fn bitcoin_block(height: u64, endorsements: Vec<IncludedEndorsement>) -> BtcBlockRecords {
    BtcBlockRecords {
        id: btc_tip(height),
        burns: vec![],
        endorsements: endorsements
            .into_iter()
            .map(|entry| entry.endorsement)
            .collect(),
    }
}

#[tokio::test]
async fn restores_acquisitions_separately_and_preserves_surviving_acquisitions() {
    let fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    let bitcoin = bitcoin_processor(&fixture);
    let data = BurnData::new(
        included(0x52, &fixture.arriving, 100)
            .endorsement
            .auth()
            .minter()
            .p2wsh(),
        Predicate::opaque(Predicate::unspendable_key()),
    );
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![TxOut {
            value: Amount::from_sat(4800),
            script_pubkey: data.to_script(),
        }],
    };
    let acquisition = Burn::from_tx(&transaction).pop().unwrap();
    let index = AcquisitionIndex::from(&acquisition);
    let storage = &fixture.processor.consensus_storage;
    let before = storage.get_acquisitions_by_minters(100).await.unwrap();
    let mut block = bitcoin_block(100, vec![included(0x51, &fixture.arriving, 100)]);
    block.burns.push(acquisition);
    bitcoin.apply(block).await.unwrap();
    assert!(storage.get_acquisition(index).await.unwrap().is_some());
    bitcoin.rollback(btc_tip(100)).await.unwrap();
    assert!(storage.get_acquisition(index).await.unwrap().is_none());
    assert_eq!(
        storage.get_acquisitions_by_minters(100).await.unwrap(),
        before
    );
    assert_eq!(
        storage
            .get_cumulative_weight(fixture.arriving.header.block_tip())
            .await
            .unwrap()
            .unwrap()
            .effective_power,
        0
    );
}

#[tokio::test]
async fn resumes_the_saved_rollback_plan_after_inclusions_have_already_been_removed() {
    let fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    fixture.add_acquisition(0x52).await;
    let mut bitcoin = bitcoin_processor(&fixture);
    let first = included(0x51, &fixture.arriving, 100);
    let removed = included(0x52, &fixture.arriving, 101);
    bitcoin
        .apply(bitcoin_block(100, vec![first.clone()]))
        .await
        .unwrap();
    bitcoin
        .apply(bitcoin_block(101, vec![removed.clone()]))
        .await
        .unwrap();
    let storage = &fixture.processor.consensus_storage;
    let minter = removed.endorsement.auth().minter().p2wsh();
    let plan = bitcoin
        .engine
        .get_rollback_plan(BitcoinRollbackInput {
            included_endorsements: vec![removed.clone()],
            minters: BTreeMap::from([(
                minter,
                storage.get_minter_endorsement_state(&minter).await.unwrap(),
            )]),
            ..BitcoinRollbackInput::default()
        })
        .await
        .unwrap();
    fixture
        .processor
        .journal
        .owner
        .bitcoin_journal()
        .begin_rollback(btc_tip(101), &plan)
        .await
        .unwrap();
    storage
        .remove_endorsement_inclusion(&removed)
        .await
        .unwrap();
    bitcoin.engine.consensus_storage = Arc::new(InMemoryConsensusStorage::new());
    assert_eq!(bitcoin.resume().await.unwrap(), btc_tip(100));
    assert_eq!(
        storage
            .get_cumulative_weight(fixture.arriving.header.block_tip())
            .await
            .unwrap()
            .unwrap()
            .effective_power,
        8
    );
    assert_eq!(
        storage
            .get_endorsements_for_block(first.endorsement.block_tip())
            .await
            .unwrap(),
        vec![WeightedEndorsement {
            original: first,
            effective_minting_power: 8
        }]
    );
    assert!(
        fixture
            .processor
            .journal
            .owner
            .latest_entry()
            .await
            .unwrap()
            .unwrap()
            .is_complete()
    );
}

#[tokio::test]
async fn removes_only_the_rolled_back_pending_inclusion() {
    let fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    let bitcoin = bitcoin_processor(&fixture);
    let unknown = CoreBlockTip {
        hash: BlockHash::from([7; 32]),
        height: 7.into(),
    };
    let first = IncludedEndorsement {
        btc_block: btc_tip(100),
        endorsement: endorsement_for_tip(0x51, unknown),
    };
    let second = IncludedEndorsement {
        btc_block: btc_tip(101),
        endorsement: endorsement_for_tip(0x52, unknown),
    };
    bitcoin
        .apply(bitcoin_block(100, vec![first.clone()]))
        .await
        .unwrap();
    bitcoin
        .apply(bitcoin_block(101, vec![second]))
        .await
        .unwrap();
    bitcoin.rollback(btc_tip(101)).await.unwrap();
    assert_eq!(
        fixture
            .processor
            .consensus_storage
            .get_pending_endorsements_for_block(unknown)
            .await
            .unwrap(),
        vec![first]
    );
    bitcoin.rollback(btc_tip(100)).await.unwrap();
    assert!(
        fixture
            .processor
            .consensus_storage
            .get_pending_endorsements_for_block(unknown)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.processor.engine.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn restores_endorsements_excluded_at_other_heights_after_the_last_conflict_is_removed() {
    let mut fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    let mut branch = fixture.root_state.clone();
    branch.connect(&fixture.arriving).unwrap();
    let later = core_block(&mut branch, 2, 100, 2);
    let mut fork = fixture.root_state.clone();
    let conflict = core_block(&mut fork, 1, 100, 3);
    fixture.register(conflict.clone(), 0, 0).await;
    fixture.add_acquisition(0x52).await;
    let bitcoin = bitcoin_processor(&fixture);
    let first = included(0x51, &fixture.arriving, 100);
    let excluded = included(0x51, &later, 100);
    let foreign = included(0x52, &fixture.arriving, 100);
    bitcoin
        .apply(bitcoin_block(
            100,
            vec![first.clone(), excluded.clone(), foreign.clone()],
        ))
        .await
        .unwrap();
    bitcoin
        .apply(bitcoin_block(101, vec![included(0x51, &conflict, 101)]))
        .await
        .unwrap();
    fixture.register(later.clone(), 3, 0).await;
    fixture
        .processor
        .on_flame_block_arrival(later.header.block_tip())
        .await
        .unwrap();
    let storage = &fixture.processor.consensus_storage;
    assert!(
        storage
            .get_pending_endorsements_for_block(excluded.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        storage
            .get_endorsements_for_block(excluded.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    let calls = fixture.processor.engine.calls.load(Ordering::SeqCst);
    bitcoin.rollback(btc_tip(101)).await.unwrap();
    assert!(
        !storage
            .is_minter_double_signed(&first.endorsement.auth().minter().p2wsh())
            .await
            .unwrap()
    );
    let counted = storage
        .get_endorsements_for_block(first.endorsement.block_tip())
        .await
        .unwrap();
    assert_eq!(counted.len(), 2);
    assert!(
        counted
            .iter()
            .any(|entry| entry.original == foreign && entry.effective_minting_power == 8)
    );
    assert!(
        counted
            .iter()
            .any(|entry| entry.original == first && entry.effective_minting_power == 8)
    );
    assert_eq!(
        storage
            .get_endorsements_for_block(excluded.endorsement.block_tip())
            .await
            .unwrap(),
        vec![WeightedEndorsement {
            original: excluded,
            effective_minting_power: 8
        }]
    );
    let weight = storage
        .get_cumulative_weight(later.header.block_tip())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((weight.parent_weight, weight.effective_power), (4, 8));
    assert_eq!(
        fixture.processor.canonical_storage.get_tip().await.unwrap(),
        Some(later.header.block_tip())
    );
    assert_eq!(fixture.processor.engine.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn keeps_remaining_conflicts_and_unknown_pending_without_counting_disqualified_power() {
    let mut fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    let mut fork = fixture.root_state.clone();
    let conflict = core_block(&mut fork, 1, 100, 3);
    fixture.register(conflict.clone(), 0, 0).await;
    let bitcoin = bitcoin_processor(&fixture);
    let first = included(0x51, &fixture.arriving, 100);
    let unknown = CoreBlockTip {
        hash: BlockHash::from([8; 32]),
        height: 8.into(),
    };
    let pending = IncludedEndorsement {
        btc_block: btc_tip(100),
        endorsement: endorsement_for_tip(0x51, unknown),
    };
    bitcoin
        .apply(bitcoin_block(100, vec![first.clone(), pending.clone()]))
        .await
        .unwrap();
    bitcoin
        .apply(bitcoin_block(101, vec![included(0x51, &conflict, 101)]))
        .await
        .unwrap();
    bitcoin
        .apply(bitcoin_block(102, vec![included(0x51, &conflict, 102)]))
        .await
        .unwrap();
    bitcoin.rollback(btc_tip(102)).await.unwrap();
    let storage = &fixture.processor.consensus_storage;
    let minter = first.endorsement.auth().minter().p2wsh();
    assert!(storage.is_minter_double_signed(&minter).await.unwrap());
    assert_eq!(
        storage
            .get_double_sign(&minter, 1.into())
            .await
            .unwrap()
            .unwrap()
            .endorsements
            .len(),
        2
    );
    assert!(
        storage
            .get_endorsements_for_block(first.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        storage
            .get_pending_endorsements_for_block(unknown)
            .await
            .unwrap(),
        vec![pending.clone()]
    );
    bitcoin.rollback(btc_tip(101)).await.unwrap();
    assert!(!storage.is_minter_double_signed(&minter).await.unwrap());
    assert_eq!(
        storage
            .get_endorsements_for_block(first.endorsement.block_tip())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        storage
            .get_pending_endorsements_for_block(unknown)
            .await
            .unwrap(),
        vec![pending]
    );
}

#[tokio::test]
async fn recalculates_rewards_without_changing_the_selected_flame_branch() {
    let mut fixture = Fixture::at_btc_tip(btc_tip(99)).await;
    fixture.add_acquisition(0x52).await;
    let mut branch = fixture.root_state.clone();
    branch.connect(&fixture.arriving).unwrap();
    let mut last = fixture.arriving.clone();
    for height in 2..=150 {
        last = core_block(&mut branch, height, 100, height as u8);
        fixture.register(last.clone(), 0, 0).await;
    }
    let bitcoin = recovery::bitcoin_processor(&fixture).await;
    bitcoin
        .apply(bitcoin_block(
            100,
            vec![
                included(0x51, &fixture.arriving, 100),
                included(0x51, &last, 100),
            ],
        ))
        .await
        .unwrap();
    bitcoin
        .apply(bitcoin_block(
            101,
            vec![included(0x52, &fixture.arriving, 101)],
        ))
        .await
        .unwrap();
    fixture
        .processor
        .on_flame_block_arrival(fixture.arriving.header.block_tip())
        .await
        .unwrap();
    fixture
        .processor
        .on_flame_block_arrival(last.header.block_tip())
        .await
        .unwrap();
    let rewards = &fixture.processor.rewards_storage;
    let before = rewards.get_pending_rewards(200.into()).await.unwrap();
    assert_eq!(before.len(), 2);
    assert_eq!(
        fixture.processor.canonical_storage.get_tip().await.unwrap(),
        Some(last.header.block_tip())
    );
    let calls = fixture.processor.engine.calls.load(Ordering::SeqCst);
    bitcoin.rollback(btc_tip(101)).await.unwrap();
    let after = rewards.get_pending_rewards(200.into()).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].amount, 50 * flamechain::SPARKS_PER_FLAME);
    assert!(before.iter().all(|entry| entry.amount < after[0].amount));
    assert_eq!(
        fixture.processor.canonical_storage.get_tip().await.unwrap(),
        Some(last.header.block_tip())
    );
    assert_eq!(fixture.processor.engine.calls.load(Ordering::SeqCst), calls);
}
