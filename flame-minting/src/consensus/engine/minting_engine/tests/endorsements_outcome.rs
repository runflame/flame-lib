use super::weighter::{add_acquisition, add_block, header, included, params};
use super::*;
use crate::consensus::{ConsensusEngine, ConsensusOutcome, EndorsementIndex, MintingEngineFactory};
use std::collections::BTreeSet;

fn factory(storage: Storage) -> MintingEngineFactory<Storage, Storage, Storage> {
    let storage = Arc::new(storage);
    MintingEngineFactory {
        protocol_params: Arc::new(params()),
        consensus_storage: Arc::clone(&storage),
        chain_storage: storage,
        canonical_storage: Arc::new(Storage::default()),
    }
}

#[tokio::test]
async fn preserves_inclusion_blocks_and_weights_endorsements_by_their_individual_delays() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 0);
    add_block(&mut storage, &child, 10, 0);
    for minter in [1, 2] {
        add_acquisition(&mut storage, minter, 98, 40, 5);
    }
    let first = included(1, &root, 100);
    let delayed = included(2, &root, 102);
    let pending = IncludedEndorsement {
        btc_block: btc_tip(103),
        endorsement: endorsement(3, 4, 99),
    };
    let engine = factory(storage);
    let outcome = engine
        .get_endorsements_outcome(vec![first.clone(), delayed.clone(), pending.clone()])
        .await
        .unwrap();

    assert_eq!(outcome.accepted_endorsements.len(), 2);
    assert_eq!(
        outcome.removed_pending_endorsements,
        BTreeSet::from([
            (
                first.endorsement.block_tip(),
                EndorsementIndex::from(&first.endorsement)
            ),
            (
                delayed.endorsement.block_tip(),
                EndorsementIndex::from(&delayed.endorsement)
            ),
        ])
    );
    for (original, effective_minting_power) in [(first, 8), (delayed, 2)] {
        assert!(
            outcome
                .accepted_endorsements
                .contains(&WeightedEndorsement {
                    original,
                    effective_minting_power,
                })
        );
    }
    assert_eq!(outcome.pending_endorsements.len(), 1);
    assert_eq!(
        outcome.pending_endorsements[&pending.endorsement.block_tip()],
        vec![pending]
    );
    assert_eq!(
        outcome.weighted_blocks[&root.block_tip()].effective_power,
        10
    );
    assert_eq!(
        outcome.weighted_blocks[&child.block_tip()].parent_weight,
        13
    );
    assert!(outcome.accepted_acquisitions.is_empty());
    assert!(outcome.removed_endorsements.is_empty());
    assert!(outcome.double_signs.is_empty());
    assert_eq!(
        engine.consensus_storage.cummulative_weights[&root.block_tip()].effective_power,
        0
    );
    assert!(engine.consensus_storage.endorsements.is_empty());
}

#[tokio::test]
async fn revalidates_inclusion_height_acquisition_activity_and_current_disqualification() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 0);
    for minter in [1, 2, 3] {
        add_acquisition(&mut storage, minter, 98, 40, 5);
    }
    add_acquisition(&mut storage, 4, 100, 40, 5);
    let disqualified = included(2, &root, 100);
    storage.double_signs.push(DoubleSign {
        minter: disqualified.endorsement.auth().minter().p2wsh(),
        target_flame_height: 2.into(),
        endorsements: vec![],
    });
    let accepted = included(3, &root, 110);
    let engine = factory(storage);
    let endorsements = vec![
        included(1, &root, 99),
        disqualified,
        included(4, &root, 100),
        included(5, &root, 100),
        accepted.clone(),
    ];
    let removed_pending: BTreeSet<_> = endorsements
        .iter()
        .map(|included| {
            (
                included.endorsement.block_tip(),
                EndorsementIndex::from(&included.endorsement),
            )
        })
        .collect();
    let outcome = engine.get_endorsements_outcome(endorsements).await.unwrap();

    assert_eq!(outcome.removed_pending_endorsements, removed_pending);
    assert_eq!(
        outcome.accepted_endorsements,
        vec![WeightedEndorsement {
            original: accepted,
            effective_minting_power: 0,
        }]
    );
    assert!(outcome.pending_endorsements.is_empty());
    assert!(outcome.double_signs.is_empty());
    assert!(outcome.accepted_acquisitions.is_empty());
}

#[tokio::test]
async fn detects_conflicts_with_stored_endorsements_and_removes_their_weight() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let fork = header(10, Some((3, 100)), BlockHash::new([0; 32]), 2);
    let child = header(11, None, root.id(), 3);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 8);
    add_block(&mut storage, &fork, 10, 0);
    add_block(&mut storage, &child, 13, 0);
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let stored = WeightedEndorsement {
        original: included(1, &root, 100),
        effective_minting_power: 8,
    };
    storage.endorsements.push(stored.clone());
    let conflicting = included(1, &fork, 101);
    let outcome = factory(storage)
        .get_endorsements_outcome(vec![conflicting.clone()])
        .await
        .unwrap();

    assert!(outcome.accepted_endorsements.is_empty());
    assert_eq!(outcome.removed_endorsements, vec![stored.clone()]);
    assert_eq!(
        outcome.double_signs,
        vec![DoubleSign {
            minter: conflicting.endorsement.auth().minter().p2wsh(),
            target_flame_height: conflicting.block_height(),
            endorsements: vec![stored.original, conflicting],
        }]
    );
    assert_eq!(
        outcome.weighted_blocks[&root.block_tip()].effective_power,
        0
    );
    assert_eq!(
        outcome.weighted_blocks[&child.block_tip()].parent_weight,
        10
    );
}

#[tokio::test]
async fn detects_double_signs_across_inclusion_blocks_in_the_same_batch() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let fork = header(10, Some((3, 100)), BlockHash::new([0; 32]), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 0);
    add_block(&mut storage, &fork, 10, 0);
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let first = included(1, &root, 100);
    let second = included(1, &fork, 102);
    let outcome = factory(storage)
        .get_endorsements_outcome(vec![first.clone(), second.clone()])
        .await
        .unwrap();

    assert_eq!(
        outcome.removed_pending_endorsements,
        BTreeSet::from([
            (
                first.endorsement.block_tip(),
                EndorsementIndex::from(&first.endorsement)
            ),
            (
                second.endorsement.block_tip(),
                EndorsementIndex::from(&second.endorsement)
            ),
        ])
    );
    assert_eq!(
        outcome.double_signs,
        vec![DoubleSign {
            minter: first.endorsement.auth().minter().p2wsh(),
            target_flame_height: first.block_height(),
            endorsements: vec![first, second],
        }]
    );
    assert!(outcome.accepted_endorsements.is_empty());
    assert!(outcome.removed_endorsements.is_empty());
    assert!(outcome.weighted_blocks.is_empty());
}

#[tokio::test]
async fn ignores_stored_duplicates_and_counts_new_duplicates_once() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 8);
    for minter in [1, 2] {
        add_acquisition(&mut storage, minter, 98, 40, 5);
    }
    let stored = included(1, &root, 100);
    storage.endorsements.push(WeightedEndorsement {
        original: stored.clone(),
        effective_minting_power: 8,
    });
    let new = included(2, &root, 101);
    let removed_pending = BTreeSet::from([
        (
            stored.endorsement.block_tip(),
            EndorsementIndex::from(&stored.endorsement),
        ),
        (
            new.endorsement.block_tip(),
            EndorsementIndex::from(&new.endorsement),
        ),
    ]);
    let outcome = factory(storage)
        .get_endorsements_outcome(vec![stored, new.clone(), new.clone()])
        .await
        .unwrap();

    assert_eq!(outcome.removed_pending_endorsements, removed_pending);
    assert_eq!(
        outcome.accepted_endorsements,
        vec![WeightedEndorsement {
            original: new,
            effective_minting_power: 4,
        }]
    );
    assert_eq!(
        outcome.weighted_blocks[&root.block_tip()].effective_power,
        12
    );
    assert!(outcome.double_signs.is_empty());
}

#[tokio::test]
async fn bitcoin_and_endorsement_entry_points_produce_the_same_consensus_outcome() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 0);
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let endorsement = included(1, &root, 101);
    let engine = factory(storage);
    let minting = engine
        .get_minting_outcome(BtcBlockRecords {
            id: endorsement.btc_block,
            burns: vec![],
            endorsements: vec![endorsement.endorsement.clone()],
        })
        .await
        .unwrap();
    let consensus = engine
        .get_endorsements_outcome(vec![endorsement.clone()])
        .await
        .unwrap();

    assert_eq!(minting.consensus, consensus);
    assert_eq!(minting.next_btc_cursor, endorsement.btc_block);
}

#[tokio::test]
async fn empty_endorsements_do_not_read_storage() {
    let engine = factory(Storage {
        fail: true,
        fail_blocks: true,
        ..Storage::default()
    });
    assert_eq!(
        engine.get_endorsements_outcome(vec![]).await.unwrap(),
        ConsensusOutcome::default()
    );
}

#[tokio::test]
async fn propagates_validation_and_weight_errors() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    for (fail, fail_blocks, expected) in [
        (
            true,
            false,
            MintingEngineError::ConsensusStorage("storage unavailable"),
        ),
        (
            false,
            true,
            MintingEngineError::ChainStorage("chain storage unavailable"),
        ),
        (
            false,
            false,
            MintingEngineError::Weighter(WeighterError::MissingWeight(root.block_tip())),
        ),
    ] {
        let mut storage = Storage {
            fail,
            fail_blocks,
            blocks: HashMap::from([(root.id(), root.clone())]),
            ..Storage::default()
        };
        add_acquisition(&mut storage, 1, 98, 40, 5);
        assert_eq!(
            factory(storage)
                .get_endorsements_outcome(vec![included(1, &root, 100)])
                .await,
            Err(expected)
        );
    }
}
