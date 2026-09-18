use super::*;
use crate::consensus::MinterAcquisitions;
use btc_integration::{
    Acquisition, AcquisitionData, AuthenticatedMintingVote, BtcBlockTip, MinterP2wsh,
    MintingVoteValidator, UncheckedMintingVote, protocol::minter_witness_script,
};
use corepc_client::bitcoin::{
    Amount, Script, ScriptBuf, Transaction, TxIn, TxOut, Witness, absolute, hashes::Hash,
    transaction,
};
use flamechain::{Block, BlockHash};
use flamevm::Predicate;
use std::sync::Mutex;

#[derive(Default)]
struct Storage {
    votes: Vec<WeightedVote>,
    double_signs: Vec<DoubleSign>,
    fail: bool,
    fail_double_sign: bool,
    reads: Mutex<Vec<(MinterP2wsh, CoreFlameHeight)>>,
}

impl ConsensusStorage for Storage {
    type Error = &'static str;

    async fn store_acquisition(
        &self,
        _: BtcBlockTip,
        _: &crate::consensus::IncludedAcquisition,
    ) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_active_acquisitions_by_minters(
        &self,
        _: u64,
    ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, Self::Error> {
        unreachable!()
    }

    async fn store_vote(&self, _: BtcBlockTip, _: &WeightedVote) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn add_double_sign(&self, _: &DoubleSign) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_double_sign(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<DoubleSign>, Self::Error> {
        if self.fail_double_sign {
            return Err("double sign storage unavailable");
        }
        Ok(self
            .double_signs
            .iter()
            .find(|sign| sign.minter == *minter && sign.target_flame_height == height)
            .cloned())
    }

    async fn is_minter_double_signed(&self, minter: &MinterP2wsh) -> Result<bool, Self::Error> {
        if self.fail_double_sign {
            return Err("double sign storage unavailable");
        }
        Ok(self.double_signs.iter().any(|sign| sign.minter == *minter))
    }

    async fn get_minter_vote_for_height(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<WeightedVote>, Self::Error> {
        self.reads.lock().unwrap().push((*minter, height));
        if self.fail {
            return Err("storage unavailable");
        }
        Ok(self
            .votes
            .iter()
            .find(|vote| {
                vote.original.vote.auth().minter().p2wsh() == *minter
                    && vote.original.block_height() == height.as_u32()
            })
            .cloned())
    }
}

impl ChainStorage for Storage {
    type Error = &'static str;

    async fn add_block(&self, _: &Block) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_block(&self, _: BlockHash) -> Result<Option<Block>, Self::Error> {
        unreachable!()
    }
}

impl CanonicalStorage for Storage {
    type State = ();
    type Error = &'static str;

    async fn get_state(&self) -> Result<Option<(BlockHash, ())>, Self::Error> {
        unreachable!()
    }

    async fn commit_state(&self, _: BlockHash, _: &()) -> Result<(), Self::Error> {
        unreachable!()
    }
}

fn btc_tip(height: u64) -> BtcBlockTip {
    BtcBlockTip {
        hash: corepc_client::bitcoin::BlockHash::from_byte_array([height as u8; 32]),
        height,
    }
}

fn vote(minter: u8, height: u32, hash: u8) -> AuthenticatedMintingVote {
    let witness_script = minter_witness_script::build_with_authorization(
        &Predicate::opaque(Predicate::unspendable_key()),
        Script::from_bytes(&[minter]),
    );
    let mut payload = b"FLMB".to_vec();
    payload.push(1);
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(&[hash; 32]);
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            witness: Witness::from_slice(&[witness_script.as_bytes()]),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                <&corepc_client::bitcoin::script::PushBytes>::try_from(payload.as_slice()).unwrap(),
            ),
        }],
    };
    MintingVoteValidator::validate(
        UncheckedMintingVote::from_tx(&tx).pop().unwrap(),
        &TxOut {
            value: Amount::from_sat(1),
            script_pubkey: witness_script.to_p2wsh(),
        },
    )
    .unwrap()
}

fn engine(
    votes: Vec<AuthenticatedMintingVote>,
    storage: Storage,
) -> MintingEngine<Storage, Storage, Storage> {
    MintingEngine::new(
        Arc::new(IndexedBlock {
            btc_block_tip: btc_tip(100),
            acquisitions: vec![],
            votes,
        }),
        Arc::new(MintingProtocolParams {
            acquisition_maturity: 1,
            default_acquisition_duration: 1.try_into().unwrap(),
            min_acquisition_duration: 1.try_into().unwrap(),
            max_vote_delay: 10,
        }),
        Arc::new(storage),
        Arc::new(Storage::default()),
        Arc::new(Storage::default()),
    )
}

fn acquisition(duration: Option<u16>) -> Acquisition {
    let mut data = AcquisitionData::new(
        [0x11; 32].into(),
        Predicate::opaque(Predicate::unspendable_key()),
        ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    );
    data.duration = duration;
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![TxOut {
            value: Amount::from_sat(1),
            script_pubkey: data.to_script(),
        }],
    };
    Acquisition::from_tx(&transaction).pop().unwrap()
}

#[test]
fn validates_acquisition_duration_boundaries() {
    let mut engine = engine(vec![], Storage::default());
    Arc::make_mut(&mut engine.protocol_params).min_acquisition_duration = 10.try_into().unwrap();
    let acquisitions: Vec<_> = [0, 9, 10, 11, u16::MAX]
        .into_iter()
        .map(|duration| acquisition(Some(duration)))
        .collect();
    Arc::make_mut(&mut engine.new_block).acquisitions = acquisitions.clone();

    assert_eq!(
        engine.validate_acquisitions(),
        acquisitions[2..]
            .iter()
            .cloned()
            .map(|acquisition| IncludedAcquisition {
                btc_block: btc_tip(100),
                acquisition
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn validates_default_duration_when_acquisition_duration_is_omitted() {
    let acquisition = acquisition(None);
    for default_duration in [9, 10, 11] {
        let mut engine = engine(vec![], Storage::default());
        let params = Arc::make_mut(&mut engine.protocol_params);
        params.min_acquisition_duration = 10.try_into().unwrap();
        params.default_acquisition_duration = default_duration.try_into().unwrap();
        Arc::make_mut(&mut engine.new_block).acquisitions = vec![acquisition.clone()];

        let expected = if default_duration >= 10 {
            vec![IncludedAcquisition {
                btc_block: btc_tip(100),
                acquisition: acquisition.clone(),
            }]
        } else {
            vec![]
        };
        assert_eq!(engine.validate_acquisitions(), expected);
    }
}

#[tokio::test]
async fn accepts_votes_and_deduplicates_the_same_block() {
    let first = vote(0x51, 42, 1);
    let engine = engine(vec![first.clone(), first.clone()], Storage::default());
    let result = engine.validate_votes().await.unwrap();
    assert_eq!(
        result.valid_votes,
        vec![IncludedVote {
            btc_block: btc_tip(100),
            vote: first
        }]
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert_eq!(engine.consensus_storage.reads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn groups_conflicts_by_minter_and_height_and_appends_subsequent_votes() {
    let first = vote(0x51, 42, 1);
    let second = vote(0x51, 42, 2);
    let third = vote(0x51, 42, 3);
    let other_height = vote(0x51, 43, 1);
    let other_minter = vote(0x52, 42, 2);
    let engine = engine(
        vec![
            first.clone(),
            other_height.clone(),
            other_minter.clone(),
            second.clone(),
            third.clone(),
            first.clone(),
        ],
        Storage::default(),
    );
    let result = engine.validate_votes().await.unwrap();
    assert_eq!(result.valid_votes.len(), 2);
    assert!(result.valid_votes.iter().any(|v| v.vote == other_height));
    assert!(result.valid_votes.iter().any(|v| v.vote == other_minter));
    assert_eq!(
        result.double_signs,
        vec![DoubleSign {
            minter: first.auth().minter().p2wsh(),
            target_flame_height: 42.into(),
            votes: [first.clone(), second, third, first]
                .into_iter()
                .map(|vote| IncludedVote {
                    btc_block: btc_tip(100),
                    vote
                })
                .collect(),
        }]
    );
    assert!(result.removed_votes.is_empty());
    assert_eq!(engine.consensus_storage.reads.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn removes_a_conflicting_stored_vote_once_and_preserves_its_inclusion() {
    let stored = WeightedVote {
        original: IncludedVote {
            btc_block: btc_tip(99),
            vote: vote(0x51, 42, 1),
        },
        effective_minting_power: 7,
    };
    let second = vote(0x51, 42, 2);
    let third = vote(0x51, 42, 3);
    let engine = engine(
        vec![second.clone(), third.clone()],
        Storage {
            votes: vec![stored.clone()],
            ..Storage::default()
        },
    );
    let result = engine.validate_votes().await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert_eq!(result.removed_votes, vec![stored.clone()]);
    assert_eq!(
        result.double_signs,
        vec![DoubleSign {
            minter: second.auth().minter().p2wsh(),
            target_flame_height: 42.into(),
            votes: vec![
                stored.original,
                IncludedVote {
                    btc_block: btc_tip(100),
                    vote: second
                },
                IncludedVote {
                    btc_block: btc_tip(100),
                    vote: third
                }
            ],
        }]
    );
    assert_eq!(
        engine.consensus_storage.reads.lock().unwrap().as_slice(),
        &[(result.double_signs[0].minter, 42.into())]
    );
}

#[tokio::test]
async fn deduplicates_votes_when_storage_has_the_same_hash() {
    for power in [0, 7] {
        let incoming = vote(0x51, 42, 1);
        let engine = engine(
            vec![incoming.clone(), incoming],
            Storage {
                votes: vec![WeightedVote {
                    original: IncludedVote {
                        btc_block: btc_tip(99),
                        vote: vote(0x51, 42, 1),
                    },
                    effective_minting_power: power,
                }],
                ..Storage::default()
            },
        );
        let result = engine.validate_votes().await.unwrap();
        assert!(result.valid_votes.is_empty());
        assert!(result.double_signs.is_empty());
        assert!(result.removed_votes.is_empty());
    }
}

#[tokio::test]
async fn detects_a_conflicting_stored_vote_with_zero_power() {
    let stored = WeightedVote {
        original: IncludedVote {
            btc_block: btc_tip(99),
            vote: vote(0x51, 42, 1),
        },
        effective_minting_power: 0,
    };
    let incoming = vote(0x51, 42, 2);
    let engine = engine(
        vec![incoming.clone()],
        Storage {
            votes: vec![stored.clone()],
            ..Storage::default()
        },
    );
    let result = engine.validate_votes().await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert_eq!(result.removed_votes, vec![stored.clone()]);
    assert_eq!(
        result.double_signs,
        vec![DoubleSign {
            minter: incoming.auth().minter().p2wsh(),
            target_flame_height: 42.into(),
            votes: vec![
                stored.original,
                IncludedVote {
                    btc_block: btc_tip(100),
                    vote: incoming,
                }
            ],
        }]
    );
}

#[tokio::test]
async fn removes_stored_vote_regardless_of_current_vote_order_and_power() {
    let first = vote(0x51, 42, 1);
    let second = vote(0x51, 42, 2);
    let third = vote(0x51, 42, 3);
    for power in [0, 7] {
        let stored = WeightedVote {
            original: IncludedVote {
                btc_block: btc_tip(99),
                vote: first.clone(),
            },
            effective_minting_power: power,
        };
        for (case, votes) in [
            vec![first.clone(), second.clone(), third.clone()],
            vec![second.clone(), first.clone(), third.clone()],
            vec![
                second.clone(),
                second.clone(),
                third.clone(),
                first.clone(),
                third.clone(),
            ],
        ]
        .into_iter()
        .enumerate()
        {
            let mut expected_votes = vec![stored.original.clone()];
            // Only the first scenario starts with a duplicate before the conflict.
            let skipped = if case == 0 { 1 } else { 0 };
            expected_votes.extend(
                votes
                    .iter()
                    .skip(skipped)
                    .cloned()
                    .map(|vote| IncludedVote {
                        btc_block: btc_tip(100),
                        vote,
                    }),
            );
            let engine = engine(
                votes,
                Storage {
                    votes: vec![stored.clone()],
                    ..Storage::default()
                },
            );
            let result = engine.validate_votes().await.unwrap();
            assert!(result.valid_votes.is_empty());
            assert_eq!(result.removed_votes, vec![stored.clone()]);
            assert_eq!(
                result.double_signs,
                vec![DoubleSign {
                    minter: first.auth().minter().p2wsh(),
                    target_flame_height: 42.into(),
                    votes: expected_votes,
                }]
            );
        }
    }
}

#[tokio::test]
async fn skips_votes_for_a_stored_double_sign_without_reading_stored_votes() {
    let first = vote(0x51, 42, 1);
    let second = vote(0x51, 42, 2);
    let engine = engine(
        vec![first.clone(), second.clone(), vote(0x51, 42, 3)],
        Storage {
            double_signs: vec![DoubleSign {
                minter: first.auth().minter().p2wsh(),
                target_flame_height: 42.into(),
                votes: vec![
                    IncludedVote {
                        btc_block: btc_tip(98),
                        vote: first,
                    },
                    IncludedVote {
                        btc_block: btc_tip(99),
                        vote: second,
                    },
                ],
            }],
            fail: true,
            ..Storage::default()
        },
    );
    let result = engine.validate_votes().await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stored_double_sign_does_not_reject_other_minters_or_heights() {
    let first = vote(0x51, 42, 1);
    let other_minter = vote(0x52, 42, 1);
    let other_height = vote(0x51, 43, 1);
    let engine = engine(
        vec![other_minter.clone(), other_height.clone()],
        Storage {
            double_signs: vec![DoubleSign {
                minter: first.auth().minter().p2wsh(),
                target_flame_height: 42.into(),
                votes: vec![
                    IncludedVote {
                        btc_block: btc_tip(98),
                        vote: first,
                    },
                    IncludedVote {
                        btc_block: btc_tip(99),
                        vote: vote(0x51, 42, 2),
                    },
                ],
            }],
            ..Storage::default()
        },
    );
    let result = engine.validate_votes().await.unwrap();
    assert_eq!(result.valid_votes.len(), 2);
    assert!(result.valid_votes.iter().any(|v| v.vote == other_minter));
    assert!(result.valid_votes.iter().any(|v| v.vote == other_height));
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
}

#[tokio::test]
async fn propagates_double_sign_storage_errors() {
    let engine = engine(
        vec![vote(0x51, 42, 1)],
        Storage {
            fail_double_sign: true,
            ..Storage::default()
        },
    );
    assert!(matches!(
        engine.validate_votes().await,
        Err("double sign storage unavailable")
    ));
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn propagates_storage_errors() {
    let engine = engine(
        vec![vote(0x51, 42, 1)],
        Storage {
            fail: true,
            ..Storage::default()
        },
    );
    assert!(matches!(
        engine.validate_votes().await,
        Err("storage unavailable")
    ));
}

#[tokio::test]
async fn accepts_an_empty_block_without_reading_storage() {
    let engine = engine(
        vec![],
        Storage {
            fail: true,
            ..Storage::default()
        },
    );
    let result = engine.validate_votes().await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}
