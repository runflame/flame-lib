use super::weighter::{add_acquisition, add_block, header, included, params};
use super::*;
use crate::consensus::engine::weighter::WeighterError;

fn outcome_engine(
    storage: Storage,
    votes: Vec<AuthenticatedMintingVote>,
    acquisitions: Vec<Acquisition>,
) -> MintingEngine<Storage, Storage, Storage> {
    let storage = Arc::new(storage);
    MintingEngine::new(
        Arc::new(IndexedBlock {
            btc_block_tip: btc_tip(100),
            acquisitions,
            votes,
        }),
        Arc::new(params()),
        Arc::clone(&storage),
        storage,
        Arc::new(Storage::default()),
    )
}

#[tokio::test]
async fn produces_an_outcome_with_weighted_votes_acquisitions_pending_votes_and_cursor() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 1);
    add_block(&mut storage, &child, 10, 0);
    add_acquisition(&mut storage, 1, 98, 40, 5);
    add_acquisition(&mut storage, 2, 98, 10, 5);
    add_acquisition(&mut storage, 3, 98, 10, 5);
    let accepted = included(1, &root, 100);
    let pending = vote(2, 4, 99);
    let another_pending = vote(3, 4, 99);
    let acquisition = acquisition(Some(5));
    let engine = outcome_engine(
        storage,
        vec![
            accepted.vote.clone(),
            accepted.vote.clone(),
            pending.clone(),
            another_pending.clone(),
        ],
        vec![acquisition.clone(), super::acquisition(Some(0))],
    );
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert_eq!(
        outcome.accepted_votes,
        vec![WeightedVote {
            original: accepted,
            effective_minting_power: 8
        }]
    );
    assert_eq!(
        outcome.accepted_acquisitions,
        vec![IncludedAcquisition {
            btc_block: btc_tip(100),
            acquisition
        }]
    );
    assert_eq!(outcome.pending_votes.len(), 1);
    let pending_group = &outcome.pending_votes[&pending.block_tip()];
    assert_eq!(pending_group.len(), 2);
    for vote in [pending, another_pending] {
        assert!(pending_group.contains(&IncludedVote {
            btc_block: btc_tip(100),
            vote
        }));
    }
    assert!(outcome.removed_votes.is_empty());
    assert!(outcome.double_signs.is_empty());
    assert_eq!(outcome.next_btc_cursor, btc_tip(100));
    assert_eq!(
        outcome.weighted_blocks[&root.block_tip()].effective_power,
        9
    );
    assert_eq!(
        outcome.weighted_blocks[&child.block_tip()].parent_weight,
        13
    );
    assert_eq!(
        engine.consensus_storage.cummulative_weights[&root.block_tip()].effective_power,
        1
    );
}

#[tokio::test]
async fn double_sign_removes_the_stored_vote_and_decreases_descendant_weights() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let fork = header(10, Some((3, 100)), BlockHash::new([0; 32]), 2);
    let child = header(11, None, root.id(), 3);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 8);
    add_block(&mut storage, &fork, 10, 0);
    add_block(&mut storage, &child, 13, 0);
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let stored = WeightedVote {
        original: included(1, &root, 100),
        effective_minting_power: 8,
    };
    storage.votes.push(stored.clone());
    let mut engine = outcome_engine(storage, vec![included(1, &fork, 101).vote], vec![]);
    Arc::make_mut(&mut engine.new_block).btc_block_tip = btc_tip(101);
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert!(outcome.accepted_votes.is_empty());
    assert_eq!(outcome.removed_votes, vec![stored.clone()]);
    assert_eq!(outcome.double_signs.len(), 1);
    assert_eq!(
        outcome.double_signs[0].votes,
        vec![stored.original, included(1, &fork, 101)]
    );
    assert_eq!(outcome.weighted_blocks.len(), 2);
    assert_eq!(
        outcome.weighted_blocks[&root.block_tip()].effective_power,
        0
    );
    assert_eq!(
        outcome.weighted_blocks[&child.block_tip()].parent_weight,
        10
    );
    assert_eq!(outcome.next_btc_cursor, btc_tip(101));
}

#[tokio::test]
async fn counts_current_block_acquisitions_when_maturity_is_zero() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 0);
    let vote = included(1, &root, 100);
    let acquisition = acquisition_with_amount(vote.vote.auth().minter().p2wsh(), Some(5), 15);
    let mut engine = outcome_engine(storage, vec![vote.vote.clone()], vec![acquisition]);
    Arc::make_mut(&mut engine.protocol_params).acquisition_maturity = 0;
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert_eq!(
        outcome.accepted_votes,
        vec![WeightedVote {
            original: vote,
            effective_minting_power: 3
        }]
    );
    assert_eq!(
        outcome.weighted_blocks[&root.block_tip()].effective_power,
        3
    );
    assert!(engine.consensus_storage.acquisitions.is_empty());
}

#[tokio::test]
async fn returns_empty_outcome_for_an_empty_block() {
    let engine = outcome_engine(Storage::default(), vec![], vec![]);
    let outcome = engine.get_minting_outcome().await.unwrap();
    assert_eq!(
        outcome,
        MintingOutcome {
            accepted_votes: vec![],
            removed_votes: vec![],
            weighted_blocks: HashMap::new(),
            accepted_acquisitions: vec![],
            double_signs: vec![],
            pending_votes: BTreeMap::new(),
            next_btc_cursor: btc_tip(100),
        }
    );
}

#[tokio::test]
async fn propagates_storage_validation_and_weighter_errors() {
    let engine = outcome_engine(
        Storage {
            fail: true,
            ..Storage::default()
        },
        vec![],
        vec![],
    );
    assert_eq!(
        engine.get_minting_outcome().await,
        Err(MintingEngineError::ConsensusStorage("storage unavailable"))
    );
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    storage.blocks.insert(root.id(), root.clone());
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let engine = outcome_engine(storage, vec![included(1, &root, 100).vote], vec![]);
    assert_eq!(
        engine.get_minting_outcome().await,
        Err(MintingEngineError::Weighter(WeighterError::MissingWeight(
            root.block_tip()
        )))
    );
    let mut failed_chain =
        outcome_engine(Storage::default(), engine.new_block.votes.clone(), vec![]);
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
