use std::{collections::BTreeMap, sync::Mutex};

use corepc_client::bitcoin::{BlockHash as BtcBlockHash, hashes::Hash};
use flamechain::{Block, Blockchain, ChainParams, CoreBlockHeader, CoreBlockTip};

use super::*;
use crate::consensus::engine::tests::{Storage, vote_for_tip};
use crate::consensus::{IncludedAcquisition, IncludedVote, MinterAcquisitions, WeightedVote};
use btc_integration::{Acquisition, AcquisitionData, MinterP2wsh};
use corepc_client::bitcoin::{Amount, Transaction, TxOut, absolute, transaction};
use flamevm::Predicate;

pub(crate) fn protocol_params() -> Arc<MintingProtocolParams> {
    Arc::new(MintingProtocolParams {
        acquisition_maturity: 1,
        default_acquisition_duration: 100.try_into().unwrap(),
        min_acquisition_duration: 1.try_into().unwrap(),
        max_vote_delay: 10,
    })
}

#[derive(Default)]
pub(crate) struct TestChainStorage {
    blocks: BTreeMap<BlockTip, BlockHeader>,
    reads: Mutex<Vec<BlockTip>>,
    fail: bool,
}

impl TestChainStorage {
    fn insert_chain(&mut self, core_height: u32, branch: u8) -> BlockHeader {
        let mut header = Blockchain::new(ChainParams::default())
            .unwrap()
            .build_block([branch; 32], vec![])
            .unwrap()
            .header;
        header.height = 0;
        for height in 1..=core_height {
            for is_core in [false, true] {
                header.parent = header.id();
                header.height += 1;
                header.core_block = is_core.then_some(CoreBlockHeader {
                    height: height.into(),
                    target_btc_height: height,
                });
                self.blocks.insert(header.block_tip(), header.clone());
            }
        }
        header
    }
}

impl ChainStorage for TestChainStorage {
    type Error = &'static str;

    async fn add_block(&self, _: &Block) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Block>, Self::Error> {
        self.reads.lock().unwrap().push(tip);
        if self.fail {
            return Err("chain storage unavailable");
        }
        Ok(self.blocks.get(&tip).map(|header| Block {
            header: header.clone(),
            transactions: vec![],
        }))
    }

    async fn get_block_header_by_block_tip(
        &self,
        tip: BlockTip,
    ) -> Result<Option<BlockHeader>, Self::Error> {
        self.reads.lock().unwrap().push(tip);
        if self.fail {
            return Err("chain storage unavailable");
        }
        Ok(self.blocks.get(&tip).cloned())
    }

    async fn get_core_block_header(
        &self,
        _: CoreBlockTip,
    ) -> Result<Option<CoreBlockHeader>, Self::Error> {
        unreachable!()
    }

    async fn get_block_header(&self, _: CoreBlockTip) -> Result<Option<BlockHeader>, Self::Error> {
        unreachable!()
    }

    async fn get_core_block_header_with_core_descendants(
        &self,
        _: CoreBlockTip,
    ) -> Result<Option<(BlockHeader, Vec<BlockHeader>)>, Self::Error> {
        unreachable!()
    }
}

fn btc_tip() -> BtcBlockTip {
    BtcBlockTip {
        hash: BtcBlockHash::from_byte_array([1; 32]),
        height: 1000,
    }
}

#[tokio::test]
async fn loads_the_previous_cycle_from_the_supplied_branch_in_core_height_order() {
    for height in [150, 250] {
        let mut storage = TestChainStorage::default();
        let header = storage.insert_chain(height, 1);
        storage.insert_chain(height, 2);
        let engine = RewardsEngine {
            consensus_storage: Arc::new(Storage::default()),
            protocol_params: protocol_params(),
            chain_storage: Arc::new(storage),
        };
        let range = height - 149..=height - 50;
        let headers = engine
            .load_cycle_headers(&header, range.clone())
            .await
            .unwrap();
        assert_eq!(
            headers
                .iter()
                .map(|header| header.core_block.as_ref().unwrap().height.as_u32())
                .collect::<Vec<_>>(),
            range.collect::<Vec<_>>()
        );
        assert!(
            headers
                .iter()
                .all(|header| header.core_block_hash == [1; 32])
        );
    }
}

#[tokio::test]
async fn skips_early_and_unscheduled_blocks_without_reading_storage() {
    let mut storage = TestChainStorage::default();
    let header = storage.insert_chain(150, 1);
    let engine = RewardsEngine {
        consensus_storage: Arc::new(Storage::default()),
        protocol_params: protocol_params(),
        chain_storage: Arc::new(storage),
    };
    for height in [None, Some(0), Some(49), Some(50), Some(149), Some(151)] {
        let mut header = header.clone();
        header.core_block = height.map(|height| CoreBlockHeader {
            height: height.into(),
            target_btc_height: height,
        });
        assert!(
            engine
                .calculate(&header, btc_tip())
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(engine.chain_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn calculates_an_empty_cycle_without_rewards() {
    let mut storage = TestChainStorage::default();
    let header = storage.insert_chain(150, 1);
    let engine = RewardsEngine {
        consensus_storage: Arc::new(Storage::default()),
        protocol_params: protocol_params(),
        chain_storage: Arc::new(storage),
    };
    let outcome = engine.calculate(&header, btc_tip()).await.unwrap().unwrap();
    assert_eq!(outcome.target_block_height.as_u32(), 200);
    assert!(outcome.rewards.is_empty());
    let reads = engine.chain_storage.reads.lock().unwrap();
    let last_header = &engine.chain_storage.blocks[reads.last().unwrap()];
    assert_eq!(last_header.core_block.as_ref().unwrap().height.as_u32(), 1);
}

#[tokio::test]
async fn missing_ancestors_and_storage_errors_are_reported() {
    let mut storage = TestChainStorage::default();
    let header = storage.insert_chain(150, 1);
    let parent = BlockTip {
        hash: header.parent,
        height: (header.height - 1).into(),
    };
    storage.blocks.remove(&parent);
    let engine = RewardsEngine {
        consensus_storage: Arc::new(Storage::default()),
        protocol_params: protocol_params(),
        chain_storage: Arc::new(storage),
    };
    assert!(matches!(
        engine.calculate(&header, btc_tip()).await,
        Err(RewardsEngineError::MissingBlock(tip)) if tip == parent
    ));
    let engine = RewardsEngine {
        consensus_storage: Arc::new(Storage::default()),
        protocol_params: protocol_params(),
        chain_storage: Arc::new(TestChainStorage {
            fail: true,
            ..TestChainStorage::default()
        }),
    };
    assert!(matches!(
        engine.calculate(&header, btc_tip()).await,
        Err(RewardsEngineError::ChainStorage(
            "chain storage unavailable"
        ))
    ));
}

type TestEngine = RewardsEngine<TestChainStorage, Storage>;

fn predicate(tag: u64) -> Predicate {
    let point = Predicate::unspendable_key().decompress().unwrap();
    Predicate::opaque((point * flamevm::Scalar::from(tag).to_dalek()).compress())
}

fn reward_header() -> BlockHeader {
    let mut storage = TestChainStorage::default();
    storage.insert_chain(100, 1)
}

fn included_vote(minter: u8, header: &BlockHeader, delay: u64) -> WeightedVote {
    WeightedVote {
        original: IncludedVote {
            btc_block: BtcBlockTip {
                height: u64::from(header.core_block.as_ref().unwrap().target_btc_height) + delay,
                ..btc_tip()
            },
            vote: vote_for_tip(
                minter,
                CoreBlockTip {
                    hash: header.id(),
                    height: header.core_block.as_ref().unwrap().height,
                },
            ),
        },
        effective_minting_power: 0,
    }
}

fn acquisition(
    minter: MinterP2wsh,
    access: u64,
    amount: u64,
    duration: Option<u16>,
) -> IncludedAcquisition {
    let mut data = AcquisitionData::new(
        minter,
        predicate(access),
        ed25519_dalek::SigningKey::from_bytes(&[1; 32]).verifying_key(),
    );
    data.duration = duration;
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![TxOut {
            value: Amount::from_sat(amount),
            script_pubkey: data.to_script(),
        }],
    };
    IncludedAcquisition {
        btc_block: BtcBlockTip {
            height: 90,
            ..btc_tip()
        },
        acquisition: Acquisition::from_tx(&transaction).pop().unwrap(),
    }
}

fn test_engine(storage: Storage) -> TestEngine {
    RewardsEngine {
        chain_storage: Arc::new(TestChainStorage::default()),
        consensus_storage: Arc::new(storage),
        protocol_params: protocol_params(),
    }
}

fn calculator<'a>(
    engine: &'a TestEngine,
    header: &'a BlockHeader,
    btc_tip: BtcBlockTip,
) -> BlockWeightCalculator<'a, Storage> {
    BlockWeightCalculator {
        header,
        btc_tip,
        consensus_storage: engine.consensus_storage.as_ref(),
        protocol_params: &engine.protocol_params,
    }
}

fn pair(access: u64) -> PredicatePair {
    (
        Predicate::unspendable_key().to_bytes(),
        predicate(access).to_point().to_bytes(),
    )
}

#[tokio::test]
async fn groups_predicates_and_recalculates_fractional_power_with_duration_and_delay() {
    let header = reward_header();
    let first = included_vote(1, &header, 0);
    let second = included_vote(2, &header, 1);
    let first_minter = first.original.vote.auth().minter().p2wsh();
    let second_minter = second.original.vote.auth().minter().p2wsh();
    let mut storage = Storage::default();
    storage.votes = vec![first, second];
    storage.acquisitions.insert(
        first_minter,
        MinterAcquisitions {
            is_double_signed: false,
            acquisitions: vec![
                acquisition(first_minter, 1, 60, Some(100)),
                acquisition(first_minter, 1, 20, Some(200)),
                acquisition(first_minter, 2, 60, Some(200)),
            ],
        },
    );
    storage.acquisitions.insert(
        second_minter,
        MinterAcquisitions {
            is_double_signed: false,
            acquisitions: vec![acquisition(second_minter, 1, 200, None)],
        },
    );
    let mut engine = test_engine(storage);
    let weights = calculator(&engine, &header, btc_tip())
        .calculate()
        .await
        .unwrap();
    assert_eq!(weights.len(), 2);
    let total: f64 = weights.values().map(|entry| entry.weight).sum();
    assert!((weights[&pair(1)].weight - 1.7).abs() < 1e-12);
    assert!((weights[&pair(2)].weight - 0.3).abs() < 1e-12);
    assert!((weights[&pair(1)].weight / total - 0.85).abs() < 1e-12);
    assert!((weights[&pair(2)].weight / total - 0.15).abs() < 1e-12);
    assert!(
        engine
            .consensus_storage
            .active_acquisition_reads
            .lock()
            .unwrap()
            .iter()
            .all(|(_, height)| *height == 100)
    );

    let storage = Arc::get_mut(&mut engine.consensus_storage).unwrap();
    storage.votes.reverse();
    for minter in storage.acquisitions.values_mut() {
        minter.acquisitions.reverse();
    }
    let reordered = calculator(&engine, &header, btc_tip())
        .calculate()
        .await
        .unwrap();
    for (key, entry) in weights {
        assert_eq!(entry.weight.to_bits(), reordered[&key].weight.to_bits());
    }
    let mut rewards = CycleRewards::new();
    engine
        .calculate_block(&header, btc_tip(), &mut rewards)
        .await
        .unwrap();
    assert_eq!(rewards.len(), 2);
    assert_eq!(rewards[&pair(1)].amount, 4_250_000_000);
    assert_eq!(rewards[&pair(2)].amount, 750_000_000);
}

#[tokio::test]
async fn excludes_inactive_disqualified_late_future_and_other_block_contributions() {
    let header = reward_header();
    let mut other = header.clone();
    other.core_block_hash = [2; 32];
    let mut storage = Storage::default();
    for (minter, delay, target) in [
        (1, 10, &header),
        (2, 11, &header),
        (3, 0, &header),
        (4, 0, &other),
    ] {
        let vote = included_vote(minter, target, delay);
        let identity = vote.original.vote.auth().minter().p2wsh();
        storage.votes.push(vote);
        storage.acquisitions.insert(
            identity,
            MinterAcquisitions {
                is_double_signed: minter == 3,
                acquisitions: vec![acquisition(identity, u64::from(minter), 100, Some(100))],
            },
        );
    }
    let first = storage.votes[0].original.vote.auth().minter().p2wsh();
    let mut immature = acquisition(first, 5, 100, Some(100));
    immature.btc_block.height = 100;
    let expired = acquisition(first, 6, 100, Some(2));
    storage
        .acquisitions
        .get_mut(&first)
        .unwrap()
        .acquisitions
        .extend([immature, expired]);
    let engine = test_engine(storage);
    let weights = calculator(&engine, &header, btc_tip())
        .calculate()
        .await
        .unwrap();
    assert_eq!(weights.len(), 1);
    assert_eq!(weights[&pair(1)].weight, 1.0 / 1024.0);
    let earlier = BtcBlockTip {
        height: 109,
        ..btc_tip()
    };
    assert!(
        calculator(&engine, &header, earlier)
            .calculate()
            .await
            .unwrap()
            .is_empty()
    );
    let mut rewards = CycleRewards::new();
    engine
        .calculate_block(&header, earlier, &mut rewards)
        .await
        .unwrap();
    assert!(rewards.is_empty());
}

#[tokio::test]
async fn propagates_vote_and_acquisition_storage_errors() {
    let header = reward_header();
    let mut storage = Storage::default();
    storage.fail_vote_reads = true;
    let mut engine = test_engine(storage);
    assert!(matches!(
        calculator(&engine, &header, btc_tip()).calculate().await,
        Err(BlockWeightCalculatorError::ConsensusStorage(
            "storage unavailable"
        ))
    ));
    let storage = Arc::get_mut(&mut engine.consensus_storage).unwrap();
    storage.fail_vote_reads = false;
    storage.votes.push(included_vote(1, &header, 0));
    storage.fail_active_acquisition_reads = true;
    assert!(matches!(
        calculator(&engine, &header, btc_tip()).calculate().await,
        Err(BlockWeightCalculatorError::ConsensusStorage(
            "storage unavailable"
        ))
    ));
    assert_eq!(
        engine
            .consensus_storage
            .active_acquisition_reads
            .lock()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn accumulates_rewards_for_the_same_predicates_and_checks_overflow() {
    let mut rewards = CycleRewards::new();
    let reward = |amount| PendingReward {
        flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
        access_predicate: predicate(1),
        amount,
    };
    TestEngine::accumulate_reward(&mut rewards, pair(1), reward(3)).unwrap();
    TestEngine::accumulate_reward(&mut rewards, pair(1), reward(7)).unwrap();
    assert_eq!(rewards.len(), 1);
    assert_eq!(rewards[&pair(1)].amount, 10);
    assert_eq!(
        rewards[&pair(1)].access_predicate.to_point(),
        predicate(1).to_point()
    );
    assert_eq!(
        TestEngine::accumulate_reward(&mut rewards, pair(1), reward(u64::MAX)),
        Err(RewardsEngineError::RewardOverflow)
    );
    assert_eq!(rewards[&pair(1)].amount, 10);
}

#[tokio::test]
async fn calculates_cycle_rewards_and_combines_matching_predicates_across_blocks() {
    let mut chain = TestChainStorage::default();
    let tip = chain.insert_chain(150, 1);
    let mut storage = Storage::default();
    for height in [1, 100] {
        let header = chain
            .blocks
            .values()
            .find(|header| {
                header
                    .core_block
                    .as_ref()
                    .is_some_and(|core| core.height.as_u32() == height)
            })
            .unwrap();
        storage.votes.push(included_vote(1, header, 0));
    }
    let minter = storage.votes[0].original.vote.auth().minter().p2wsh();
    let mut acquisition = acquisition(minter, 1, 100, Some(200));
    acquisition.btc_block.height = 0;
    storage.acquisitions.insert(
        minter,
        MinterAcquisitions {
            is_double_signed: false,
            acquisitions: vec![acquisition],
        },
    );
    let engine = RewardsEngine {
        chain_storage: Arc::new(chain),
        consensus_storage: Arc::new(storage),
        protocol_params: protocol_params(),
    };
    let outcome = engine.calculate(&tip, btc_tip()).await.unwrap().unwrap();
    assert_eq!(outcome.target_block_height.as_u32(), 200);
    let rewards = &outcome.rewards;
    assert_eq!(rewards.len(), 1);
    assert_eq!(rewards[0].amount, 10_000_000_000);
    assert_eq!(rewards[0].flame_predicate.to_point().to_bytes(), pair(1).0);
    assert_eq!(rewards[0].access_predicate.to_point().to_bytes(), pair(1).1);

    struct RecordingStorage(Mutex<Vec<(CoreFlameHeight, u64)>>);

    impl crate::rewards::RewardsStorage for RecordingStorage {
        type Error = std::convert::Infallible;

        async fn store_pending_reward(
            &self,
            target_block_height: CoreFlameHeight,
            reward: &PendingReward,
        ) -> Result<(), Self::Error> {
            self.0
                .lock()
                .unwrap()
                .push((target_block_height, reward.amount));
            Ok(())
        }
    }

    let applier = crate::rewards::RewardsApplier {
        rewards_storage: RecordingStorage(Mutex::new(Vec::new())),
    };
    applier.apply(&outcome).await.unwrap();
    assert_eq!(
        *applier.rewards_storage.0.lock().unwrap(),
        vec![(200.into(), 10_000_000_000)]
    );
}

#[tokio::test]
async fn rejects_target_height_overflow_before_loading_history() {
    let mut header = reward_header();
    header.core_block.as_mut().unwrap().height = (u32::MAX - 45).into();
    let engine = test_engine(Storage::default());
    assert!(matches!(
        engine.calculate(&header, btc_tip()).await,
        Err(RewardsEngineError::TargetBlockHeightOverflow)
    ));
    assert!(engine.chain_storage.reads.lock().unwrap().is_empty());
}
