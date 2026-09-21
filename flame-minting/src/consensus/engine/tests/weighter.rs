use super::*;
use crate::consensus::engine::weighter::{Weighter, WeighterError, WeighterResult};

pub(super) fn params() -> MintingProtocolParams {
    MintingProtocolParams {
        acquisition_maturity: 2,
        default_acquisition_duration: 5.try_into().unwrap(),
        min_acquisition_duration: 1.try_into().unwrap(),
        max_vote_delay: 10,
    }
}

pub(super) fn header(
    height: u64,
    core: Option<(u32, u32)>,
    parent: BlockHash,
    tag: u8,
) -> BlockHeader {
    let mut header = Blockchain::new(ChainParams::default())
        .unwrap()
        .build_block([tag; 32], vec![])
        .unwrap()
        .header;
    header.height = height;
    header.parent = parent;
    header.core_block = core.map(|(height, target_btc_height)| CoreBlockHeader {
        height: height.into(),
        target_btc_height,
    });
    header
}

fn tip(header: &BlockHeader) -> BlockTip {
    header.block_tip()
}

pub(super) fn included(minter: u8, header: &BlockHeader, btc_height: u64) -> IncludedVote {
    IncludedVote {
        btc_block: btc_tip(btc_height),
        vote: vote_for_tip(
            minter,
            CoreBlockTip {
                hash: header.id(),
                height: header.core_block.as_ref().unwrap().height,
            },
        ),
    }
}

pub(super) fn add_acquisition(
    storage: &mut Storage,
    minter: u8,
    btc_height: u64,
    amount: u64,
    duration: u16,
) {
    let minter = vote(minter, 1, 1).auth().minter().p2wsh();
    storage
        .acquisitions
        .entry(minter)
        .or_insert_with(|| MinterAcquisitions {
            is_double_signed: false,
            acquisitions: vec![],
        })
        .acquisitions
        .push(IncludedAcquisition {
            btc_block: btc_tip(btc_height),
            acquisition: acquisition_with_amount(minter, Some(duration), amount),
        });
}

pub(super) fn add_block(
    storage: &mut Storage,
    header: &BlockHeader,
    parent_weight: u64,
    effective_power: u64,
) {
    storage.blocks.insert(header.id(), header.clone());
    storage.cummulative_weights.insert(
        tip(header),
        WeightedBlockHeader {
            header: header.clone(),
            parent_weight,
            effective_power,
        },
    );
}

async fn weigh(
    storage: &Storage,
    votes: &[IncludedVote],
) -> Result<HashMap<BlockTip, WeightedBlockHeader>, WeighterError<&'static str, &'static str>> {
    weigh_changes(storage, votes, &[])
        .await
        .map(|result| result.weighted_blocks)
}

async fn weigh_changes(
    storage: &Storage,
    votes: &[IncludedVote],
    removed_votes: &[WeightedVote],
) -> Result<WeighterResult, WeighterError<&'static str, &'static str>> {
    let params = params();
    let acquisitions = AcquisitionProvider {
        storage,
        new_acquisitions: &[],
        params: &params,
    };
    Weighter {
        votes,
        removed_votes,
        acquisitions: &acquisitions,
        consensus_storage: storage,
        chain_storage: storage,
        protocol_params: &params,
    }
    .weigh()
    .await
}

#[tokio::test]
async fn returns_updated_headers_using_each_blocks_own_target_and_stored_power() {
    let first = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let second = header(10, Some((3, 103)), BlockHash::new([0; 32]), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &first, 20, 9);
    add_block(&mut storage, &second, 30, 12);
    add_acquisition(&mut storage, 1, 98, 24, 3); // Power 8 at 100, expired at 103.
    add_acquisition(&mut storage, 1, 101, 40, 5); // Power 8 at 103, not acquired at 100.
    add_acquisition(&mut storage, 2, 98, 18, 3); // Power 6 at 100.
    let votes = [
        included(1, &first, 101),
        included(1, &second, 104),
        included(2, &first, 101),
    ];
    let result = weigh(&storage, &votes).await.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(
        result[&tip(&first)],
        WeightedBlockHeader {
            header: first.clone(),
            parent_weight: 20,
            effective_power: 9 + 4 + 3,
        }
    );
    assert_eq!(
        result[&tip(&second)],
        WeightedBlockHeader {
            header: second.clone(),
            parent_weight: 30,
            effective_power: 12 + 4,
        }
    );
    // Deref exposes the full header, including its ordinary Flame height.
    assert_eq!(storage.cummulative_weights[&tip(&first)].height, 10);
    let mut reads = storage.acquisition_reads.lock().unwrap().clone();
    reads.sort();
    assert_eq!(reads, vec![100, 103]);
    assert_eq!(storage.cummulative_weights[&tip(&first)].effective_power, 9);
}

#[tokio::test]
async fn includes_all_descendants_and_forks_without_double_counting_overlapping_roots() {
    let height = u64::from(u32::MAX);
    let root = header(height, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(height + 1, None, root.id(), 2);
    let grandchild = header(height + 2, Some((4, 101)), child.id(), 3);
    let fork = header(height + 2, Some((4, 102)), child.id(), 4);
    let leaf = header(height + 3, None, grandchild.id(), 6);
    let unrelated = header(height, Some((3, 100)), BlockHash::new([0; 32]), 5);
    let mut storage = Storage::default();
    for (header, parent_weight, power) in [
        (&root, 10, 2),
        (&child, 11, 0),
        (&grandchild, 11, 4),
        (&fork, 11, 8),
        (&leaf, 13, 0),
        (&unrelated, 7, 8),
    ] {
        add_block(&mut storage, header, parent_weight, power);
    }
    add_acquisition(&mut storage, 1, 98, 40, 5);
    let votes = [included(1, &grandchild, 101), included(1, &root, 101)];
    let result = weigh(&storage, &votes).await.unwrap();
    let reversed = weigh(&storage, &[votes[1].clone(), votes[0].clone()])
        .await
        .unwrap();
    assert_eq!(result, reversed);
    assert_eq!(result.len(), 5);
    for (header, parent_weight, effective_power) in [
        (&root, 10, 6),
        (&child, 12, 0),
        (&grandchild, 12, 12),
        (&fork, 12, 8),
        (&leaf, 15, 0), // Receives deltas from both voted ancestors.
    ] {
        assert_eq!(
            result[&tip(header)],
            WeightedBlockHeader {
                header: header.clone(),
                parent_weight,
                effective_power,
            }
        );
    }
    assert!(!result.contains_key(&tip(&unrelated)));
    let mut reads = storage.acquisition_reads.lock().unwrap().clone();
    reads.sort();
    assert_eq!(reads, vec![100, 100, 101, 101]);
    let mut descendants_reads = storage.descendants_reads.lock().unwrap().clone();
    descendants_reads.sort();
    let mut expected_reads: Vec<_> = votes
        .iter()
        .flat_map(|vote| [vote.vote.block_tip(); 2])
        .collect();
    expected_reads.sort();
    assert_eq!(descendants_reads, expected_reads);
    let mut header_reads = storage.header_reads.lock().unwrap().clone();
    header_reads.sort();
    assert_eq!(header_reads, expected_reads);
    let mut weight_reads = storage.weight_reads.lock().unwrap().clone();
    weight_reads.sort();
    let mut expected_weight_reads: Vec<_> = [&root, &child, &grandchild, &fork, &leaf]
        .into_iter()
        .flat_map(|header| [tip(header); 2])
        .collect();
    expected_weight_reads.sort();
    assert_eq!(weight_reads, expected_weight_reads);
}

#[tokio::test]
async fn empty_input_does_not_read_storage() {
    let storage = Storage {
        fail: true,
        fail_blocks: true,
        ..Storage::default()
    };
    assert!(weigh(&storage, &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn includes_unchanged_descendants_when_power_changes_without_changing_weight() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let leaf = header(12, Some((4, 101)), child.id(), 3);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 4);
    add_block(&mut storage, &child, 12, 0);
    add_block(&mut storage, &leaf, 12, 2);
    add_acquisition(&mut storage, 1, 98, 15, 5);
    let added = included(1, &root, 100);

    let result = weigh_changes(&storage, &[added.clone()], &[])
        .await
        .unwrap();

    assert_eq!(result.weighted_blocks.len(), 3);
    assert_eq!(result.weighted_blocks[&tip(&root)].effective_power, 7);
    assert_eq!(result.weighted_blocks[&tip(&root)].parent_weight, 10);
    for descendant in [&child, &leaf] {
        assert_eq!(
            result.weighted_blocks[&tip(descendant)],
            storage.cummulative_weights[&tip(descendant)],
        );
    }
    assert_eq!(
        result.weighted_votes,
        vec![WeightedVote {
            original: added,
            effective_minting_power: 3,
        }],
    );
    assert_eq!(storage.cummulative_weights[&tip(&root)].effective_power, 4);
}

#[tokio::test]
async fn reports_missing_blocks_and_weight_records_and_storage_errors() {
    let header = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let votes = [included(1, &header, 100)];
    let mut storage = Storage::default();
    assert_eq!(
        weigh(&storage, &votes).await,
        Err(WeighterError::MissingVotedBlock(votes[0].vote.block_tip()))
    );
    storage.blocks.insert(header.id(), header.clone());
    assert_eq!(
        weigh(&storage, &votes).await,
        Err(WeighterError::MissingWeight(tip(&header)))
    );
    storage.fail = true;
    assert_eq!(
        weigh(&storage, &votes).await,
        Err(WeighterError::ConsensusStorage("storage unavailable"))
    );
    storage.fail_blocks = true;
    assert_eq!(
        weigh(&storage, &votes).await,
        Err(WeighterError::ChainStorage("chain storage unavailable"))
    );
}

#[tokio::test]
async fn rejects_votes_without_a_matching_core_header() {
    let core = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let non_core = header(11, None, core.id(), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &core, 10, 4);
    add_block(&mut storage, &non_core, 12, 0);

    for block in [&core, &non_core] {
        let core_tip = CoreBlockTip {
            hash: block.id(),
            height: (block.height as u32).into(),
        };
        let vote = IncludedVote {
            btc_block: btc_tip(100),
            vote: vote_for_tip(1, core_tip),
        };
        assert_eq!(
            weigh(&storage, &[vote]).await,
            Err(WeighterError::MissingVotedBlock(core_tip)),
        );
    }
    assert!(storage.weight_reads.lock().unwrap().is_empty());
    assert!(storage.acquisition_reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn checks_effective_power_overflow_and_vote_errors() {
    let header = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let mut storage = Storage::default();
    add_block(&mut storage, &header, 0, u64::MAX);
    add_acquisition(&mut storage, 1, 98, 5, 5);
    assert_eq!(
        weigh(&storage, &[included(1, &header, 100)]).await,
        Err(WeighterError::EffectivePowerOverflow(tip(&header)))
    );
    assert_eq!(
        weigh(&storage, &[included(1, &header, 99)]).await,
        Err(WeighterError::VoteWeight(
            crate::consensus::engine::weighter::VoteWeightError::VoteBeforeTarget {
                inclusion_height: 99,
                target_height: 100
            }
        ))
    );
}

#[tokio::test]
async fn preserves_raw_weight_fields_and_checks_parent_weight_overflow() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let votes = [included(1, &root, 100)];
    let mut storage = Storage::default();
    for power in [0, 1, 2] {
        add_block(&mut storage, &root, u64::MAX, power);
        assert_eq!(
            weigh(&storage, &votes).await.unwrap()[&tip(&root)],
            WeightedBlockHeader {
                header: root.clone(),
                parent_weight: u64::MAX,
                effective_power: power,
            }
        );
    }
    let child = header(11, None, root.id(), 2);
    add_block(&mut storage, &root, u64::MAX, 0);
    add_block(&mut storage, &child, u64::MAX, 0);
    add_acquisition(&mut storage, 1, 98, 10, 5); // Power 2 increases parent weight by 1.
    assert_eq!(
        weigh(&storage, &votes).await,
        Err(WeighterError::CumulativeWeightOverflow(tip(&child)))
    );
}

#[tokio::test]
async fn removes_stored_power_without_recalculating_expired_acquisitions() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let leaf = header(12, Some((4, 101)), child.id(), 3);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, 8);
    add_block(&mut storage, &child, 13, 0);
    add_block(&mut storage, &leaf, 13, 2);
    let removed = WeightedVote {
        original: included(1, &root, 100),
        effective_minting_power: 8,
    };
    let result = weigh_changes(&storage, &[], &[removed]).await.unwrap();
    assert!(result.weighted_votes.is_empty());
    assert_eq!(result.weighted_blocks[&tip(&root)].effective_power, 0);
    assert_eq!(result.weighted_blocks[&tip(&child)].parent_weight, 10);
    assert_eq!(result.weighted_blocks[&tip(&leaf)].parent_weight, 10);
    assert_eq!(result.weighted_blocks[&tip(&leaf)].effective_power, 2);
    assert!(storage.acquisition_reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn combines_removals_and_additions_on_the_same_block_and_returns_weighted_votes() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 10, u64::MAX);
    add_block(&mut storage, &child, 73, 0);
    add_acquisition(&mut storage, 2, 98, 15, 5);
    let removed = WeightedVote {
        original: included(1, &root, 100),
        effective_minting_power: u64::MAX,
    };
    let added = included(2, &root, 100);
    let result = weigh_changes(&storage, &[added.clone()], &[removed])
        .await
        .unwrap();
    assert_eq!(
        result.weighted_votes,
        vec![WeightedVote {
            original: added,
            effective_minting_power: 3
        }]
    );
    assert_eq!(result.weighted_blocks[&tip(&root)].effective_power, 3);
    assert_eq!(result.weighted_blocks[&tip(&child)].parent_weight, 11);
}

#[tokio::test]
async fn combines_opposite_ancestor_deltas_before_updating_a_shared_descendant() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, Some((4, 100)), root.id(), 2);
    let leaf = header(12, None, child.id(), 3);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, u64::MAX - 1, 2);
    add_block(&mut storage, &child, u64::MAX, 1);
    add_block(&mut storage, &leaf, u64::MAX, 0);
    add_acquisition(&mut storage, 2, 98, 5, 5);
    let removed = WeightedVote {
        original: included(1, &root, 100),
        effective_minting_power: 1,
    };
    let result = weigh_changes(&storage, &[included(2, &child, 100)], &[removed])
        .await
        .unwrap();
    assert_eq!(result.weighted_blocks[&tip(&root)].effective_power, 1);
    assert_eq!(
        result.weighted_blocks[&tip(&child)].parent_weight,
        u64::MAX - 1
    );
    assert_eq!(result.weighted_blocks[&tip(&child)].effective_power, 2);
    assert_eq!(result.weighted_blocks[&tip(&leaf)].parent_weight, u64::MAX);
}

#[tokio::test]
async fn reports_power_and_parent_weight_underflow() {
    let root = header(10, Some((3, 100)), BlockHash::new([0; 32]), 1);
    let child = header(11, None, root.id(), 2);
    let mut storage = Storage::default();
    add_block(&mut storage, &root, 0, 1);
    let removed = WeightedVote {
        original: included(1, &root, 100),
        effective_minting_power: 2,
    };
    assert_eq!(
        weigh_changes(&storage, &[], &[removed.clone()]).await,
        Err(WeighterError::EffectivePowerUnderflow(tip(&root)))
    );
    add_block(&mut storage, &root, 0, 2);
    add_block(&mut storage, &child, 0, 0); // Inconsistent stored parent weight.
    assert_eq!(
        weigh_changes(&storage, &[], &[removed]).await,
        Err(WeighterError::CumulativeWeightUnderflow(tip(&child)))
    );
}
