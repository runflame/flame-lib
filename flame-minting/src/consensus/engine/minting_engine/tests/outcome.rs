use super::weighter::{add_acquisition, add_block, header, included, params};
use super::*;
use crate::consensus::{ConsensusOutcome, engine::weighter::WeighterError};
use std::collections::BTreeMap;

fn outcome_engine(
    storage: Storage,
    endorsements: Vec<AuthenticatedMintingEndorsement>,
    acquisitions: Vec<Burn>,
) -> MintingEngine<Storage, Storage, Storage> {
    let storage = Arc::new(storage);
    MintingEngine::new(
        Arc::new(BtcBlockRecords {
            id: btc_tip(100),
            burns: acquisitions,
            endorsements,
        }),
        Arc::new(params()),
        Arc::clone(&storage),
        storage,
        Arc::new(Storage::default()),
    )
}

#[tokio::test]
async fn produces_an_outcome_with_weighted_endorsements_acquisitions_pending_endorsements_and_cursor()
 {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 1);
    add_block(&mut storage, &child, 10, 0);
    add_acquisition(&mut storage, 1, 98, 40, 5);
    add_acquisition(&mut storage, 2, 98, 10, 5);
    add_acquisition(&mut storage, 3, 98, 10, 5);
    let accepted = included(1, &root, 100);
    let pending = endorsement(2, 4, 99);
    let another_pending = endorsement(3, 4, 99);
    let acquisition = acquisition(Some(5));
    let engine = outcome_engine(
        storage,
        vec![
            accepted.endorsement.clone(),
            accepted.endorsement.clone(),
            pending.clone(),
            another_pending.clone(),
        ],
        vec![acquisition.clone(), super::acquisition(Some(0))],
    );
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert_eq!(
        outcome.consensus.accepted_endorsements,
        vec![WeightedEndorsement {
            original: accepted,
            effective_minting_power: 8
        }]
    );
    assert_eq!(
        outcome.consensus.accepted_acquisitions,
        vec![IncludedAcquisition {
            btc_block: btc_tip(100),
            acquisition
        }]
    );
    assert_eq!(outcome.consensus.pending_endorsements.len(), 1);
    let pending_group = &outcome.consensus.pending_endorsements[&pending.block_tip()];
    assert_eq!(pending_group.len(), 2);
    for endorsement in [pending, another_pending] {
        assert!(pending_group.contains(&IncludedEndorsement {
            btc_block: btc_tip(100),
            endorsement
        }));
    }
    assert!(outcome.consensus.removed_endorsements.is_empty());
    assert!(outcome.consensus.double_signs.is_empty());
    assert_eq!(outcome.next_btc_cursor, btc_tip(100));
    assert_eq!(
        outcome.consensus.weighted_blocks[&root.block_tip()].effective_power,
        9
    );
    assert_eq!(
        outcome.consensus.weighted_blocks[&child.block_tip()].parent_weight,
        13
    );
    assert_eq!(
        engine.consensus_storage.cummulative_weights[&root.block_tip()].effective_power,
        1
    );
}

#[tokio::test]
async fn double_sign_removes_the_stored_endorsement_and_decreases_descendant_weights() {
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
    let mut engine = outcome_engine(storage, vec![included(1, &fork, 101).endorsement], vec![]);
    Arc::make_mut(&mut engine.new_block).id = btc_tip(101);
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert!(outcome.consensus.accepted_endorsements.is_empty());
    assert_eq!(outcome.consensus.removed_endorsements, vec![stored.clone()]);
    assert_eq!(outcome.consensus.double_signs.len(), 1);
    assert_eq!(
        outcome.consensus.double_signs[0].endorsements,
        vec![stored.original, included(1, &fork, 101)]
    );
    assert_eq!(outcome.consensus.weighted_blocks.len(), 2);
    assert_eq!(
        outcome.consensus.weighted_blocks[&root.block_tip()].effective_power,
        0
    );
    assert_eq!(
        outcome.consensus.weighted_blocks[&child.block_tip()].parent_weight,
        10
    );
    assert_eq!(outcome.next_btc_cursor, btc_tip(101));
}

#[tokio::test]
async fn counts_current_block_acquisitions_when_maturity_is_zero() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 0);
    let endorsement = included(1, &root, 100);
    let acquisition =
        acquisition_with_amount(endorsement.endorsement.auth().minter().p2wsh(), Some(5), 15);
    let mut engine = outcome_engine(
        storage,
        vec![endorsement.endorsement.clone()],
        vec![acquisition],
    );
    Arc::make_mut(&mut engine.protocol_params).acquisition_maturity = 0;
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert_eq!(
        outcome.consensus.accepted_endorsements,
        vec![WeightedEndorsement {
            original: endorsement,
            effective_minting_power: 3
        }]
    );
    assert_eq!(
        outcome.consensus.weighted_blocks[&root.block_tip()].effective_power,
        3
    );
    assert!(engine.consensus_storage.acquisitions.is_empty());
}

#[tokio::test]
async fn returns_empty_outcome_without_reading_storage() {
    let engine = outcome_engine(
        Storage {
            fail: true,
            ..Storage::default()
        },
        vec![],
        vec![],
    );
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert_eq!(
        outcome,
        MintingOutcome {
            consensus: ConsensusOutcome {
                accepted_endorsements: vec![],
                removed_endorsements: vec![],
                weighted_blocks: HashMap::new(),
                accepted_acquisitions: vec![],
                double_signs: vec![],
                pending_endorsements: BTreeMap::new(),
                removed_pending_endorsements: Default::default(),
            },
            next_btc_cursor: btc_tip(100),
        }
    );
}

#[tokio::test]
async fn propagates_storage_validation_and_weighter_errors() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let engine = outcome_engine(
        Storage {
            fail: true,
            blocks: HashMap::from([(root.id(), root.clone())]),
            ..Storage::default()
        },
        vec![included(1, &root, 100).endorsement],
        vec![],
    );
    assert_eq!(
        engine.get_minting_outcome().await,
        Err(MintingEngineError::ConsensusStorage("storage unavailable"))
    );
    let mut storage = Storage::default();
    storage.blocks.insert(root.id(), root.clone());
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let engine = outcome_engine(storage, vec![included(1, &root, 100).endorsement], vec![]);
    assert_eq!(
        engine.get_minting_outcome().await,
        Err(MintingEngineError::Weighter(WeighterError::MissingWeight(
            root.block_tip()
        )))
    );
    let mut failed_chain = outcome_engine(
        Storage::default(),
        engine.new_block.endorsements.clone(),
        vec![],
    );
    failed_chain.consensus_storage = Arc::clone(&engine.consensus_storage);
    failed_chain.chain_storage = Arc::new(Storage {
        fail_blocks: true,
        ..Storage::default()
    });
    assert_eq!(
        failed_chain.get_minting_outcome().await,
        Err(MintingEngineError::ChainStorage(
            "chain storage unavailable"
        ))
    );
}
