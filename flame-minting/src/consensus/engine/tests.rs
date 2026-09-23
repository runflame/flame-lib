use super::*;
use crate::consensus::{
    DoubleSign, IncludedVote, MinterAcquisitions, WeightedBlockHeader, WeightedVote,
};
use btc_integration::{
    Acquisition, AcquisitionData, AuthenticatedMintingVote, BtcBlockTip, MinterP2wsh,
    MintingVoteValidator, UncheckedMintingVote, protocol::minter_witness_script,
};
use corepc_client::bitcoin::{
    Amount, Script, ScriptBuf, Transaction, TxIn, TxOut, Witness, absolute, hashes::Hash,
    transaction,
};
use flamechain::{
    Block, BlockHash, BlockHeader, BlockTip, Blockchain, ChainParams, CoreBlockHeader,
    CoreBlockTip, CoreFlameHeight,
};
use flamevm::Predicate;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct Storage {
    pub(crate) votes: Vec<WeightedVote>,
    pub(crate) acquisitions: HashMap<MinterP2wsh, MinterAcquisitions>,
    acquisition_reads: Mutex<Vec<u64>>,
    pub(crate) active_acquisition_reads: Mutex<Vec<(MinterP2wsh, u64)>>,
    cummulative_weights: HashMap<BlockTip, WeightedBlockHeader>,
    weight_reads: Mutex<Vec<BlockTip>>,
    header_reads: Mutex<Vec<CoreBlockTip>>,
    descendants_reads: Mutex<Vec<CoreBlockTip>>,
    blocks: HashMap<BlockHash, BlockHeader>,
    fail_blocks: bool,
    double_signs: Vec<DoubleSign>,
    fail: bool,
    pub(crate) fail_vote_reads: bool,
    pub(crate) fail_active_acquisition_reads: bool,
    fail_double_sign: bool,
    reads: Mutex<Vec<(MinterP2wsh, CoreFlameHeight)>>,
}

impl ConsensusStorage for Storage {
    type Error = &'static str;

    async fn get_block_tip_with_most_weight(&self) -> Result<Option<BlockTip>, Self::Error> {
        unreachable!()
    }

    async fn get_active_minter_acquisitions_at_height(
        &self,
        minter: &MinterP2wsh,
        btc_height: u64,
        params: &MintingProtocolParams,
    ) -> Result<MinterAcquisitions, Self::Error> {
        self.active_acquisition_reads
            .lock()
            .unwrap()
            .push((*minter, btc_height));
        if self.fail || self.fail_active_acquisition_reads {
            return Err("storage unavailable");
        }
        let mut active = self
            .acquisitions
            .get(minter)
            .cloned()
            .unwrap_or(MinterAcquisitions {
                is_double_signed: false,
                acquisitions: vec![],
            });
        active.is_double_signed |= self.double_signs.iter().any(|sign| sign.minter == *minter);
        active
            .acquisitions
            .retain(|acquisition| acquisition.is_active_at(btc_height, params));
        Ok(active)
    }

    async fn get_cumulative_weight(
        &self,
        tip: BlockTip,
    ) -> Result<Option<WeightedBlockHeader>, Self::Error> {
        self.weight_reads.lock().unwrap().push(tip);
        if self.fail {
            return Err("storage unavailable");
        }
        Ok(self.cummulative_weights.get(&tip).cloned())
    }

    async fn store_cumulative_weight(
        &self,
        _: BlockTip,
        _: &WeightedBlockHeader,
    ) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn store_acquisition(
        &self,
        _: BtcBlockTip,
        _: &crate::consensus::IncludedAcquisition,
    ) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_acquisitions_by_minters(
        &self,
        btc_height: u64,
    ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, Self::Error> {
        self.acquisition_reads.lock().unwrap().push(btc_height);
        if self.fail {
            return Err("storage unavailable");
        }
        let mut minters = self.acquisitions.clone();
        for minter in minters.values_mut() {
            minter
                .acquisitions
                .retain(|acquisition| acquisition.btc_block.height <= btc_height);
        }
        Ok(minters)
    }

    async fn store_vote(&self, _: BtcBlockTip, _: &WeightedVote) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_votes_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Vec<WeightedVote>, Self::Error> {
        if self.fail || self.fail_vote_reads {
            return Err("storage unavailable");
        }
        Ok(self
            .votes
            .iter()
            .filter(|vote| vote.original.vote.block_tip() == tip)
            .cloned()
            .collect())
    }

    async fn remove_vote(&self, _: &WeightedVote) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn store_pending_vote(
        &self,
        _: CoreBlockTip,
        _: &IncludedVote,
    ) -> Result<(), Self::Error> {
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
        if self.fail || self.fail_vote_reads {
            return Err("storage unavailable");
        }
        Ok(self
            .votes
            .iter()
            .find(|vote| {
                vote.original.vote.auth().minter().p2wsh() == *minter
                    && vote.original.block_height() == height
            })
            .cloned())
    }
}

impl ChainStorage for Storage {
    type Error = &'static str;

    async fn add_block(&self, _: &Block) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Block>, Self::Error> {
        if self.fail_blocks {
            return Err("chain storage unavailable");
        }
        Ok(self
            .blocks
            .get(&tip.hash)
            .filter(|header| header.height == tip.height.as_u64())
            .map(|header| Block {
                header: header.clone(),
                transactions: vec![],
            }))
    }

    async fn get_block_header_by_block_tip(
        &self,
        tip: BlockTip,
    ) -> Result<Option<BlockHeader>, Self::Error> {
        if self.fail_blocks {
            return Err("chain storage unavailable");
        }
        Ok(self
            .blocks
            .get(&tip.hash)
            .filter(|header| header.height == tip.height.as_u64())
            .cloned())
    }

    async fn get_core_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Option<CoreBlockHeader>, Self::Error> {
        if self.fail_blocks {
            return Err("chain storage unavailable");
        }
        Ok(self
            .blocks
            .get(&tip.hash)
            .and_then(|header| header.core_block.as_ref())
            .filter(|core| core.height == tip.height)
            .cloned())
    }

    async fn get_block_header(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Option<BlockHeader>, Self::Error> {
        self.header_reads.lock().unwrap().push(tip);
        if self.fail_blocks {
            return Err("chain storage unavailable");
        }
        Ok(self
            .blocks
            .get(&tip.hash)
            .filter(|header| {
                header
                    .core_block
                    .as_ref()
                    .is_some_and(|core| core.height == tip.height)
            })
            .cloned())
    }

    async fn get_core_block_header_with_core_descendants(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Option<(BlockHeader, Vec<BlockHeader>)>, Self::Error> {
        self.descendants_reads.lock().unwrap().push(tip);
        let Some(block_header) = self.get_block_header(tip).await? else {
            return Ok(None);
        };
        let mut descendants = Vec::new();
        let mut parents = vec![tip.hash];
        while let Some(parent) = parents.pop() {
            for header in self
                .blocks
                .values()
                .filter(|header| header.parent == parent)
            {
                parents.push(header.id());
                descendants.push(header.clone());
            }
        }
        Ok(Some((block_header, descendants)))
    }
}

impl CanonicalStorage for Storage {
    type Error = &'static str;

    async fn get_tip(&self) -> Result<Option<BlockTip>, Self::Error> {
        unreachable!()
    }

    async fn get_state(&self) -> Result<Option<(BlockHash, Blockchain)>, Self::Error> {
        unreachable!()
    }

    async fn commit_state(&self, _: BlockHash, _: &Blockchain) -> Result<(), Self::Error> {
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
    vote_for_tip(
        minter,
        CoreBlockTip {
            height: height.into(),
            hash: BlockHash::new([hash; 32]),
        },
    )
}

pub(crate) fn vote_for_tip(minter: u8, tip: CoreBlockTip) -> AuthenticatedMintingVote {
    let witness_script = minter_witness_script::build_with_authorization(
        &Predicate::opaque(Predicate::unspendable_key()),
        Script::from_bytes(&[minter]),
    );
    let mut payload = b"FLMB".to_vec();
    payload.push(1);
    payload.extend_from_slice(&tip.height.as_u32().to_le_bytes());
    payload.extend_from_slice(tip.hash.as_bytes());
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
    let blocks = votes
        .iter()
        .map(|vote| {
            let mut block = Blockchain::new(ChainParams::default())
                .unwrap()
                .build_block([0; 32], vec![])
                .unwrap();
            block.header.core_block = Some(CoreBlockHeader {
                height: vote.block_height(),
                target_btc_height: 100,
            });
            (vote.block_hash(), block.header)
        })
        .collect();
    let mut engine = MintingEngine::new(
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
        Arc::new(Storage {
            blocks,
            ..Storage::default()
        }),
        Arc::new(Storage::default()),
    );
    let acquisitions = active_acquisitions(&engine);
    Arc::get_mut(&mut engine.consensus_storage)
        .unwrap()
        .acquisitions
        .extend(acquisitions);
    engine
}

fn active_acquisitions(
    engine: &MintingEngine<Storage, Storage, Storage>,
) -> HashMap<MinterP2wsh, MinterAcquisitions> {
    engine
        .new_block
        .votes
        .iter()
        .map(|vote| {
            let minter = vote.auth().minter().p2wsh();
            (
                minter,
                MinterAcquisitions {
                    is_double_signed: engine
                        .consensus_storage
                        .double_signs
                        .iter()
                        .any(|sign| sign.minter == minter),
                    acquisitions: vec![IncludedAcquisition {
                        btc_block: btc_tip(99),
                        acquisition: acquisition_for_minter(minter, None),
                    }],
                },
            )
        })
        .collect()
}

fn acquisition(duration: Option<u16>) -> Acquisition {
    acquisition_for_minter([0x11; 32].into(), duration)
}

fn acquisition_for_minter(minter: MinterP2wsh, duration: Option<u16>) -> Acquisition {
    acquisition_with_amount(minter, duration, 1)
}

fn acquisition_with_amount(minter: MinterP2wsh, duration: Option<u16>, amount: u64) -> Acquisition {
    let mut data = AcquisitionData::new(
        minter,
        Predicate::opaque(Predicate::unspendable_key()),
        ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
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
    let minter = first.auth().minter().p2wsh();
    let engine = engine(vec![first.clone(), first.clone()], Storage::default());
    let result = engine.validate_votes(&[]).await.unwrap();
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
    assert_eq!(
        *engine
            .consensus_storage
            .active_acquisition_reads
            .lock()
            .unwrap(),
        vec![(minter, 100), (minter, 100)]
    );
    assert!(
        engine
            .consensus_storage
            .acquisition_reads
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn groups_conflicts_by_minter_and_height_and_appends_subsequent_votes() {
    let first = vote(0x51, 42, 1);
    let second = vote(0x51, 42, 2);
    let third = vote(0x51, 42, 3);
    let other_height = vote(0x51, 43, 4);
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
    let result = engine.validate_votes(&[]).await.unwrap();
    assert_eq!(result.valid_votes.len(), 1);
    assert!(!result.valid_votes.iter().any(|v| v.vote == other_height));
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
    let result = engine.validate_votes(&[]).await.unwrap();
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
        let result = engine.validate_votes(&[]).await.unwrap();
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
    let result = engine.validate_votes(&[]).await.unwrap();
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
            let result = engine.validate_votes(&[]).await.unwrap();
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
            fail_vote_reads: true,
            ..Storage::default()
        },
    );
    let result = engine.validate_votes(&[]).await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stored_double_sign_rejects_other_heights_but_not_other_minters() {
    let first = vote(0x51, 42, 1);
    let other_minter = vote(0x52, 42, 1);
    let other_height = vote(0x51, 43, 4);
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
    let result = engine.validate_votes(&[]).await.unwrap();
    assert_eq!(result.valid_votes.len(), 1);
    assert!(result.valid_votes.iter().any(|v| v.vote == other_minter));
    assert!(!result.valid_votes.iter().any(|v| v.vote == other_height));
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
}

#[tokio::test]
async fn propagates_chain_storage_errors() {
    let mut engine = engine(vec![vote(0x51, 42, 1)], Storage::default());
    Arc::get_mut(&mut engine.chain_storage).unwrap().fail_blocks = true;
    assert!(matches!(
        engine.validate_votes(&[]).await,
        Err(VoteValidationError::ChainStorage(
            "chain storage unavailable"
        ))
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
        engine.validate_votes(&[]).await,
        Err(VoteValidationError::ConsensusStorage("storage unavailable"))
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
    let result = engine.validate_votes(&[]).await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn skips_votes_without_active_acquisitions_before_reading_stored_votes() {
    let incoming = vote(0x51, 42, 1);
    let minter = incoming.auth().minter().p2wsh();
    let mut engine = engine(
        vec![incoming, vote(0x51, 42, 2)],
        Storage {
            fail_vote_reads: true,
            fail_double_sign: true,
            ..Storage::default()
        },
    );
    for acquisitions in [
        HashMap::new(),
        HashMap::from([(
            minter,
            MinterAcquisitions {
                is_double_signed: false,
                acquisitions: vec![],
            },
        )]),
    ] {
        Arc::get_mut(&mut engine.consensus_storage)
            .unwrap()
            .acquisitions = acquisitions;
        let result = engine.validate_votes(&[]).await.unwrap();
        assert!(result.valid_votes.is_empty());
        assert!(result.double_signs.is_empty());
        assert!(result.removed_votes.is_empty());
        assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn accepts_only_votes_from_minters_with_active_acquisitions() {
    let active = vote(0x51, 42, 1);
    let missing = vote(0x52, 42, 1);
    let empty = vote(0x53, 42, 1);
    let mut engine = engine(
        vec![missing.clone(), active.clone(), empty.clone()],
        Storage::default(),
    );
    let mut acquisitions = active_acquisitions(&engine);
    acquisitions.remove(&missing.auth().minter().p2wsh());
    acquisitions
        .get_mut(&empty.auth().minter().p2wsh())
        .unwrap()
        .acquisitions
        .clear();

    Arc::get_mut(&mut engine.consensus_storage)
        .unwrap()
        .acquisitions = acquisitions;
    let result = engine.validate_votes(&[]).await.unwrap();
    assert_eq!(
        result.valid_votes,
        vec![IncludedVote {
            btc_block: btc_tip(100),
            vote: active.clone(),
        }]
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert_eq!(
        engine.consensus_storage.reads.lock().unwrap().as_slice(),
        &[(active.auth().minter().p2wsh(), 42.into())]
    );
}

#[tokio::test]
async fn validates_vote_inclusion_against_target_btc_height() {
    for inclusion_height in [99, 100, 101, 111, u64::from(u32::MAX) + 1] {
        let incoming = vote(0x51, 42, 1);
        let mut engine = engine(vec![incoming.clone()], Storage::default());
        Arc::make_mut(&mut engine.new_block).btc_block_tip = btc_tip(inclusion_height);

        let result = engine.validate_votes(&[]).await.unwrap();
        if inclusion_height < 100 {
            assert!(result.valid_votes.is_empty());
            assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
        } else {
            assert_eq!(
                result.valid_votes,
                vec![IncludedVote {
                    btc_block: btc_tip(inclusion_height),
                    vote: incoming,
                }]
            );
        }
        assert!(result.double_signs.is_empty());
        assert!(result.removed_votes.is_empty());
        assert!(result.pending_votes.is_empty());
    }
}

#[tokio::test]
async fn premature_vote_does_not_conflict_with_a_stored_vote() {
    let mut engine = engine(
        vec![vote(0x51, 42, 2)],
        Storage {
            votes: vec![WeightedVote {
                original: IncludedVote {
                    btc_block: btc_tip(99),
                    vote: vote(0x51, 42, 1),
                },
                effective_minting_power: 7,
            }],
            ..Storage::default()
        },
    );
    Arc::get_mut(&mut engine.chain_storage)
        .unwrap()
        .blocks
        .get_mut(&BlockHash::from([2; 32]))
        .unwrap()
        .core_block
        .as_mut()
        .unwrap()
        .target_btc_height = 101;

    let result = engine.validate_votes(&[]).await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(result.pending_votes.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn defers_votes_for_unknown_blocks() {
    let incoming = vote(0x51, 42, 1);
    let mut engine = engine(vec![incoming.clone()], Storage::default());
    Arc::get_mut(&mut engine.chain_storage)
        .unwrap()
        .blocks
        .clear();

    let result = engine.validate_votes(&[]).await.unwrap();
    assert_eq!(
        result.pending_votes,
        vec![IncludedVote {
            btc_block: btc_tip(100),
            vote: incoming,
        }]
    );
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
    assert!(
        engine
            .consensus_storage
            .active_acquisition_reads
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn defers_votes_without_a_matching_core_header() {
    let incoming = vote(0x51, 42, 1);
    let mut engine = engine(vec![incoming.clone()], Storage::default());
    Arc::get_mut(&mut engine.chain_storage)
        .unwrap()
        .blocks
        .get_mut(&incoming.block_hash())
        .unwrap()
        .core_block = None;

    let result = engine.validate_votes(&[]).await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert_eq!(
        result.pending_votes,
        vec![IncludedVote {
            btc_block: btc_tip(100),
            vote: incoming,
        }]
    );
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn validates_acquisition_activity_at_the_vote_target_height() {
    // The acquisition is included at 90. Explicit duration overrides the default of 7.
    for (maturity, duration, target, accepted) in [
        (10, Some(5), 89, false),
        (10, Some(5), 90, false),
        (10, Some(5), 99, false),
        (10, Some(5), 100, true),
        (10, Some(5), 104, true),
        (10, Some(5), 105, false),
        (10, None, 99, false),
        (10, None, 100, true),
        (10, None, 106, true),
        (10, None, 107, false),
        (0, Some(5), 89, false),
        (0, Some(5), 90, true),
        (0, Some(5), 94, true),
        (0, Some(5), 95, false),
    ] {
        let incoming = vote(0x51, 42, 1);
        let minter = incoming.auth().minter().p2wsh();
        let mut engine = engine(vec![incoming.clone()], Storage::default());
        let params = Arc::make_mut(&mut engine.protocol_params);
        params.acquisition_maturity = maturity;
        params.default_acquisition_duration = 7.try_into().unwrap();
        // At inclusion, the acquisition has expired in every case.
        Arc::make_mut(&mut engine.new_block).btc_block_tip = btc_tip(110);
        Arc::get_mut(&mut engine.chain_storage)
            .unwrap()
            .blocks
            .get_mut(&incoming.block_hash())
            .unwrap()
            .core_block
            .as_mut()
            .unwrap()
            .target_btc_height = target;
        let acquisitions = HashMap::from([(
            minter,
            MinterAcquisitions {
                is_double_signed: false,
                acquisitions: vec![IncludedAcquisition {
                    btc_block: btc_tip(90),
                    acquisition: acquisition_for_minter(minter, duration),
                }],
            },
        )]);

        Arc::get_mut(&mut engine.consensus_storage)
            .unwrap()
            .acquisitions = acquisitions;
        let result = engine.validate_votes(&[]).await.unwrap();
        let expected = if accepted {
            vec![IncludedVote {
                btc_block: btc_tip(110),
                vote: incoming,
            }]
        } else {
            vec![]
        };
        assert_eq!(
            result.valid_votes, expected,
            "{maturity}, {duration:?}, {target}"
        );
        assert!(result.double_signs.is_empty());
        assert!(result.removed_votes.is_empty());
        assert!(result.pending_votes.is_empty());
        assert_eq!(
            engine.consensus_storage.reads.lock().unwrap().len(),
            usize::from(accepted)
        );
    }
}

#[tokio::test]
async fn finds_an_active_acquisition_separately_for_each_vote_target() {
    let active_target = vote(0x51, 42, 1);
    let expired_target = vote(0x51, 43, 2);
    let minter = active_target.auth().minter().p2wsh();
    let mut engine = engine(
        vec![active_target.clone(), expired_target.clone()],
        Storage::default(),
    );
    Arc::make_mut(&mut engine.new_block).btc_block_tip = btc_tip(110);
    Arc::get_mut(&mut engine.chain_storage)
        .unwrap()
        .blocks
        .get_mut(&expired_target.block_hash())
        .unwrap()
        .core_block
        .as_mut()
        .unwrap()
        .target_btc_height = 101;
    let acquisitions = HashMap::from([(
        minter,
        MinterAcquisitions {
            is_double_signed: false,
            acquisitions: [90, 109, u64::MAX, 99]
                .into_iter()
                .map(|height| IncludedAcquisition {
                    btc_block: btc_tip(height),
                    acquisition: acquisition_for_minter(minter, Some(1)),
                })
                .collect(),
        },
    )]);

    Arc::get_mut(&mut engine.consensus_storage)
        .unwrap()
        .acquisitions = acquisitions;
    let result = engine.validate_votes(&[]).await.unwrap();
    assert_eq!(
        result.valid_votes,
        vec![IncludedVote {
            btc_block: btc_tip(110),
            vote: active_target,
        }]
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_votes.is_empty());
    assert!(result.pending_votes.is_empty());
    assert_eq!(
        engine.consensus_storage.reads.lock().unwrap().as_slice(),
        &[(minter, 42.into())]
    );
    assert_eq!(
        *engine
            .consensus_storage
            .active_acquisition_reads
            .lock()
            .unwrap(),
        vec![(minter, 100), (minter, 101)]
    );
    assert!(
        engine
            .consensus_storage
            .acquisition_reads
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn removes_all_current_votes_from_double_signers_regardless_of_order() {
    let first = vote(0x51, 42, 1);
    let second = vote(0x51, 42, 2);
    let other_height = vote(0x51, 43, 3);
    let pending = vote(0x51, 44, 4);
    let other_minter = vote(0x52, 43, 3);
    let other_minter_pending = vote(0x52, 44, 4);

    for stored_conflict in [false, true] {
        let stored = WeightedVote {
            original: IncludedVote {
                btc_block: btc_tip(99),
                vote: first.clone(),
            },
            effective_minting_power: 7,
        };
        let stored_votes = if stored_conflict {
            vec![stored.clone()]
        } else {
            vec![]
        };
        let mut votes = vec![
            first.clone(),
            second.clone(),
            other_height.clone(),
            pending.clone(),
            other_minter.clone(),
            other_minter_pending.clone(),
        ];
        for _ in 0..votes.len() {
            let mut engine = engine(
                votes.clone(),
                Storage {
                    votes: stored_votes.clone(),
                    ..Storage::default()
                },
            );
            Arc::get_mut(&mut engine.chain_storage)
                .unwrap()
                .blocks
                .remove(&pending.block_hash());

            let result = engine.validate_votes(&[]).await.unwrap();
            assert_eq!(
                result.valid_votes,
                vec![IncludedVote {
                    btc_block: btc_tip(100),
                    vote: other_minter.clone(),
                }]
            );
            assert_eq!(
                result.pending_votes,
                vec![IncludedVote {
                    btc_block: btc_tip(100),
                    vote: other_minter_pending.clone(),
                }]
            );
            assert_eq!(result.removed_votes, stored_votes);
            assert_eq!(result.double_signs.len(), 1);
            let sign = &result.double_signs[0];
            assert_eq!(sign.minter, first.auth().minter().p2wsh());
            assert_eq!(sign.target_flame_height, 42.into());
            assert!(sign.votes.iter().any(|v| v.vote == first));
            assert!(sign.votes.iter().any(|v| v.vote == second));
            if stored_conflict {
                assert!(sign.votes.contains(&stored.original));
            }
            votes.rotate_left(1);
        }
    }
}

#[tokio::test]
async fn preserves_double_sign_evidence_at_multiple_heights() {
    let engine = engine(
        vec![
            vote(0x51, 42, 1),
            vote(0x51, 42, 2),
            vote(0x51, 43, 3),
            vote(0x51, 43, 4),
            vote(0x51, 44, 5),
        ],
        Storage::default(),
    );
    let result = engine.validate_votes(&[]).await.unwrap();
    assert!(result.valid_votes.is_empty());
    assert!(result.pending_votes.is_empty());
    assert!(result.removed_votes.is_empty());
    assert_eq!(result.double_signs.len(), 2);
    for height in [42, 43] {
        let sign = result
            .double_signs
            .iter()
            .find(|sign| sign.target_flame_height == height.into())
            .unwrap();
        assert_eq!(sign.votes.len(), 2);
        assert!(sign.votes.iter().all(|v| v.block_height() == height.into()));
        assert_ne!(sign.votes[0].block_hash(), sign.votes[1].block_hash());
    }
}

mod acquisition_provider;
mod outcome;
mod vote_weight;
mod weighter;
