use super::*;
use crate::consensus::engine::weight::weighter::WeighterError;
use crate::consensus::engine::{
    acquisition_provider::AcquisitionProvider,
    endorsement_validator::{
        EndorsementValidationError, EndorsementValidationResult, EndorsementValidator,
    },
};
use crate::consensus::{
    DoubleSign, IncludedEndorsement, MinterAcquisitions, WeightedBlockHeader, WeightedEndorsement,
};
use bitcoin::{
    Amount, Script, ScriptBuf, Transaction, TxIn, TxOut, Witness, absolute, hashes::Hash,
    transaction,
};
use btc_integration::{
    AuthenticatedMintingEndorsement, Burn, BurnData, MinterP2wsh, MintingEndorsementValidator,
    UncheckedMintingEndorsement, protocol::minter_witness_script, watcher::BlockId,
};
use flamechain::{
    Block, BlockHash, BlockHeader, BlockTip, Blockchain, ChainParams, CoreBlockHeader,
    CoreBlockTip, CoreFlameHeight,
};
use flamevm::Predicate;
use std::collections::HashMap;
use std::sync::Mutex;

impl MintingEngine<Storage, Storage, Storage> {
    async fn validate_endorsements(
        &self,
        new_acquisitions: &[IncludedAcquisition],
    ) -> Result<EndorsementValidationResult, EndorsementValidationError<&'static str, &'static str>>
    {
        let endorsements: Vec<_> = self
            .new_block
            .endorsements
            .iter()
            .cloned()
            .map(|endorsement| IncludedEndorsement {
                btc_block: self.new_block.id,
                endorsement,
            })
            .collect();
        EndorsementValidator {
            endorsements: &endorsements,
            protocol_params: &self.protocol_params,
            consensus_storage: self.consensus_storage.as_ref(),
            chain_storage: self.chain_storage.as_ref(),
            new_acquisitions,
        }
        .validate()
        .await
    }
}

#[derive(Default)]
pub(crate) struct Storage {
    pub(crate) endorsements: Vec<WeightedEndorsement>,
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
    pub(crate) fail_endorsement_reads: bool,
    pub(crate) fail_active_acquisition_reads: bool,
    fail_double_sign: bool,
    reads: Mutex<Vec<(MinterP2wsh, CoreFlameHeight)>>,
}

impl ConsensusStorage for Storage {
    type Error = &'static str;

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

    async fn get_endorsements_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Vec<WeightedEndorsement>, Self::Error> {
        if self.fail || self.fail_endorsement_reads {
            return Err("storage unavailable");
        }
        Ok(self
            .endorsements
            .iter()
            .filter(|endorsement| endorsement.original.endorsement.block_tip() == tip)
            .cloned()
            .collect())
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

    async fn get_minter_endorsement_for_height(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<WeightedEndorsement>, Self::Error> {
        self.reads.lock().unwrap().push((*minter, height));
        if self.fail || self.fail_endorsement_reads {
            return Err("storage unavailable");
        }
        Ok(self
            .endorsements
            .iter()
            .find(|endorsement| {
                endorsement.original.endorsement.auth().minter().p2wsh() == *minter
                    && endorsement.original.block_height() == height
            })
            .cloned())
    }
}

impl ChainStorage for Storage {
    type Error = &'static str;

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Arc<Block>>, Self::Error> {
        if self.fail_blocks {
            return Err("chain storage unavailable");
        }
        Ok(self
            .blocks
            .get(&tip.hash)
            .filter(|header| header.height == tip.height.as_u64())
            .map(|header| {
                Arc::new(Block {
                    header: header.clone(),
                    transactions: vec![],
                })
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
}

fn btc_tip(height: u64) -> BlockId {
    BlockId {
        hash: bitcoin::BlockHash::from_byte_array([height as u8; 32]),
        height,
    }
}

fn endorsement(minter: u8, height: u32, hash: u8) -> AuthenticatedMintingEndorsement {
    endorsement_for_tip(
        minter,
        CoreBlockTip {
            height: height.into(),
            hash: BlockHash::new([hash; 32]),
        },
    )
}

pub(crate) fn endorsement_for_tip(
    minter: u8,
    tip: CoreBlockTip,
) -> AuthenticatedMintingEndorsement {
    let witness_script = minter_witness_script::build_with_authorization(
        &Predicate::opaque(Predicate::unspendable_key()),
        &ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        Script::from_bytes(&[minter]),
    );
    let mut payload = b"FLMB".to_vec();
    payload.push(1);
    payload.extend_from_slice(&tip.height.as_u32().to_le_bytes());
    payload.extend_from_slice(tip.hash.as_bytes());
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::from_consensus(u32::from(minter)),
        input: vec![TxIn {
            witness: Witness::from_slice(&[witness_script.as_bytes()]),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                <&bitcoin::script::PushBytes>::try_from(payload.as_slice()).unwrap(),
            ),
        }],
    };
    MintingEndorsementValidator::validate(
        UncheckedMintingEndorsement::from_tx(&tx).pop().unwrap(),
        &TxOut {
            value: Amount::from_sat(1),
            script_pubkey: witness_script.to_p2wsh(),
        },
    )
    .unwrap()
}

fn engine(
    endorsements: Vec<AuthenticatedMintingEndorsement>,
    storage: Storage,
) -> MintingEngine<Storage, Storage, Storage> {
    let blocks = endorsements
        .iter()
        .map(|endorsement| {
            let mut block = Blockchain::new(ChainParams::default())
                .unwrap()
                .build_block([0; 32], vec![])
                .unwrap();
            block.header.core_block = Some(CoreBlockHeader {
                height: endorsement.block_height(),
                target_btc_height: 100,
            });
            (endorsement.block_hash(), block.header)
        })
        .collect();
    let mut engine = MintingEngine::new(
        Arc::new(BtcBlockRecords {
            id: btc_tip(100),
            burns: vec![],
            endorsements,
        }),
        Arc::new(MintingProtocolParams {
            acquisition_maturity: 1,
            default_acquisition_duration: 1.try_into().unwrap(),
            min_acquisition_duration: 1.try_into().unwrap(),
            max_endorsement_delay: 10,
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
        .endorsements
        .iter()
        .map(|endorsement| {
            let minter = endorsement.auth().minter().p2wsh();
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

fn acquisition(duration: Option<u16>) -> Burn {
    acquisition_for_minter([0x11; 32].into(), duration)
}

fn acquisition_for_minter(minter: MinterP2wsh, duration: Option<u16>) -> Burn {
    acquisition_with_amount(minter, duration, 1)
}

fn acquisition_with_amount(minter: MinterP2wsh, duration: Option<u16>, amount: u64) -> Burn {
    let mut data = BurnData::new(minter, Predicate::opaque(Predicate::unspendable_key()));
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
    Burn::from_tx(&transaction).pop().unwrap()
}

#[test]
fn validates_acquisition_duration_boundaries() {
    let mut engine = engine(vec![], Storage::default());
    Arc::make_mut(&mut engine.protocol_params).min_acquisition_duration = 10.try_into().unwrap();
    let acquisitions: Vec<_> = [0, 9, 10, 11, u16::MAX]
        .into_iter()
        .map(|duration| acquisition(Some(duration)))
        .collect();
    Arc::make_mut(&mut engine.new_block).burns = acquisitions.clone();

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
        Arc::make_mut(&mut engine.new_block).burns = vec![acquisition.clone()];

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
async fn accepts_endorsements_and_deduplicates_the_same_block() {
    let first = endorsement(0x51, 42, 1);
    let minter = first.auth().minter().p2wsh();
    let engine = engine(vec![first.clone(), first.clone()], Storage::default());
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert_eq!(
        result.valid_endorsements,
        vec![IncludedEndorsement {
            btc_block: btc_tip(100),
            endorsement: first
        }]
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
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
async fn groups_conflicts_by_minter_and_height_and_appends_subsequent_endorsements() {
    let first = endorsement(0x51, 42, 1);
    let second = endorsement(0x51, 42, 2);
    let third = endorsement(0x51, 42, 3);
    let other_height = endorsement(0x51, 43, 4);
    let other_minter = endorsement(0x52, 42, 2);
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
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert_eq!(result.valid_endorsements.len(), 1);
    assert!(
        !result
            .valid_endorsements
            .iter()
            .any(|v| v.endorsement == other_height)
    );
    assert!(
        result
            .valid_endorsements
            .iter()
            .any(|v| v.endorsement == other_minter)
    );
    assert_eq!(
        result.double_signs,
        vec![DoubleSign {
            minter: first.auth().minter().p2wsh(),
            target_flame_height: 42.into(),
            endorsements: [first.clone(), second, third, first]
                .into_iter()
                .map(|endorsement| IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement
                })
                .collect(),
        }]
    );
    assert!(result.removed_endorsements.is_empty());
    assert_eq!(engine.consensus_storage.reads.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn removes_a_conflicting_stored_endorsement_once_and_preserves_its_inclusion() {
    let stored = WeightedEndorsement {
        original: IncludedEndorsement {
            btc_block: btc_tip(99),
            endorsement: endorsement(0x51, 42, 1),
        },
        effective_minting_power: 7,
    };
    let second = endorsement(0x51, 42, 2);
    let third = endorsement(0x51, 42, 3);
    let engine = engine(
        vec![second.clone(), third.clone()],
        Storage {
            endorsements: vec![stored.clone()],
            ..Storage::default()
        },
    );
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert_eq!(result.removed_endorsements, vec![stored.clone()]);
    assert_eq!(
        result.double_signs,
        vec![DoubleSign {
            minter: second.auth().minter().p2wsh(),
            target_flame_height: 42.into(),
            endorsements: vec![
                stored.original,
                IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement: second
                },
                IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement: third
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
async fn deduplicates_endorsements_when_storage_has_the_same_hash() {
    for power in [0, 7] {
        let incoming = endorsement(0x51, 42, 1);
        let engine = engine(
            vec![incoming.clone(), incoming],
            Storage {
                endorsements: vec![WeightedEndorsement {
                    original: IncludedEndorsement {
                        btc_block: btc_tip(99),
                        endorsement: endorsement(0x51, 42, 1),
                    },
                    effective_minting_power: power,
                }],
                ..Storage::default()
            },
        );
        let result = engine.validate_endorsements(&[]).await.unwrap();
        assert!(result.valid_endorsements.is_empty());
        assert!(result.double_signs.is_empty());
        assert!(result.removed_endorsements.is_empty());
    }
}

#[tokio::test]
async fn detects_a_conflicting_stored_endorsement_with_zero_power() {
    let stored = WeightedEndorsement {
        original: IncludedEndorsement {
            btc_block: btc_tip(99),
            endorsement: endorsement(0x51, 42, 1),
        },
        effective_minting_power: 0,
    };
    let incoming = endorsement(0x51, 42, 2);
    let engine = engine(
        vec![incoming.clone()],
        Storage {
            endorsements: vec![stored.clone()],
            ..Storage::default()
        },
    );
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert_eq!(result.removed_endorsements, vec![stored.clone()]);
    assert_eq!(
        result.double_signs,
        vec![DoubleSign {
            minter: incoming.auth().minter().p2wsh(),
            target_flame_height: 42.into(),
            endorsements: vec![
                stored.original,
                IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement: incoming,
                }
            ],
        }]
    );
}

#[tokio::test]
async fn removes_stored_endorsement_regardless_of_current_endorsement_order_and_power() {
    let first = endorsement(0x51, 42, 1);
    let second = endorsement(0x51, 42, 2);
    let third = endorsement(0x51, 42, 3);
    for power in [0, 7] {
        let stored = WeightedEndorsement {
            original: IncludedEndorsement {
                btc_block: btc_tip(99),
                endorsement: first.clone(),
            },
            effective_minting_power: power,
        };
        for (case, endorsements) in [
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
            let mut expected_endorsements = vec![stored.original.clone()];
            // Only the first scenario starts with a duplicate before the conflict.
            let skipped = if case == 0 { 1 } else { 0 };
            expected_endorsements.extend(endorsements.iter().skip(skipped).cloned().map(
                |endorsement| IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement,
                },
            ));
            let engine = engine(
                endorsements,
                Storage {
                    endorsements: vec![stored.clone()],
                    ..Storage::default()
                },
            );
            let result = engine.validate_endorsements(&[]).await.unwrap();
            assert!(result.valid_endorsements.is_empty());
            assert_eq!(result.removed_endorsements, vec![stored.clone()]);
            assert_eq!(
                result.double_signs,
                vec![DoubleSign {
                    minter: first.auth().minter().p2wsh(),
                    target_flame_height: 42.into(),
                    endorsements: expected_endorsements,
                }]
            );
        }
    }
}

#[tokio::test]
async fn skips_endorsements_for_a_stored_double_sign_without_reading_stored_endorsements() {
    let first = endorsement(0x51, 42, 1);
    let second = endorsement(0x51, 42, 2);
    let engine = engine(
        vec![first.clone(), second.clone(), endorsement(0x51, 42, 3)],
        Storage {
            double_signs: vec![DoubleSign {
                minter: first.auth().minter().p2wsh(),
                target_flame_height: 42.into(),
                endorsements: vec![
                    IncludedEndorsement {
                        btc_block: btc_tip(98),
                        endorsement: first,
                    },
                    IncludedEndorsement {
                        btc_block: btc_tip(99),
                        endorsement: second,
                    },
                ],
            }],
            fail_endorsement_reads: true,
            ..Storage::default()
        },
    );
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stored_double_sign_rejects_other_heights_but_not_other_minters() {
    let first = endorsement(0x51, 42, 1);
    let other_minter = endorsement(0x52, 42, 1);
    let other_height = endorsement(0x51, 43, 4);
    let engine = engine(
        vec![other_minter.clone(), other_height.clone()],
        Storage {
            double_signs: vec![DoubleSign {
                minter: first.auth().minter().p2wsh(),
                target_flame_height: 42.into(),
                endorsements: vec![
                    IncludedEndorsement {
                        btc_block: btc_tip(98),
                        endorsement: first,
                    },
                    IncludedEndorsement {
                        btc_block: btc_tip(99),
                        endorsement: endorsement(0x51, 42, 2),
                    },
                ],
            }],
            ..Storage::default()
        },
    );
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert_eq!(result.valid_endorsements.len(), 1);
    assert!(
        result
            .valid_endorsements
            .iter()
            .any(|v| v.endorsement == other_minter)
    );
    assert!(
        !result
            .valid_endorsements
            .iter()
            .any(|v| v.endorsement == other_height)
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
}

#[tokio::test]
async fn propagates_chain_storage_errors() {
    let mut engine = engine(vec![endorsement(0x51, 42, 1)], Storage::default());
    Arc::get_mut(&mut engine.chain_storage).unwrap().fail_blocks = true;
    assert!(matches!(
        engine.validate_endorsements(&[]).await,
        Err(EndorsementValidationError::ChainStorage(
            "chain storage unavailable"
        ))
    ));
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn propagates_storage_errors() {
    let engine = engine(
        vec![endorsement(0x51, 42, 1)],
        Storage {
            fail: true,
            ..Storage::default()
        },
    );
    assert!(matches!(
        engine.validate_endorsements(&[]).await,
        Err(EndorsementValidationError::ConsensusStorage(
            "storage unavailable"
        ))
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
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn skips_endorsements_without_active_acquisitions_before_reading_stored_endorsements() {
    let incoming = endorsement(0x51, 42, 1);
    let minter = incoming.auth().minter().p2wsh();
    let mut engine = engine(
        vec![incoming, endorsement(0x51, 42, 2)],
        Storage {
            fail_endorsement_reads: true,
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
        let result = engine.validate_endorsements(&[]).await.unwrap();
        assert!(result.valid_endorsements.is_empty());
        assert!(result.double_signs.is_empty());
        assert!(result.removed_endorsements.is_empty());
        assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn accepts_only_endorsements_from_minters_with_active_acquisitions() {
    let active = endorsement(0x51, 42, 1);
    let missing = endorsement(0x52, 42, 1);
    let empty = endorsement(0x53, 42, 1);
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
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert_eq!(
        result.valid_endorsements,
        vec![IncludedEndorsement {
            btc_block: btc_tip(100),
            endorsement: active.clone(),
        }]
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert_eq!(
        engine.consensus_storage.reads.lock().unwrap().as_slice(),
        &[(active.auth().minter().p2wsh(), 42.into())]
    );
}

#[tokio::test]
async fn validates_endorsement_inclusion_against_target_btc_height() {
    for inclusion_height in [99, 100, 101, 111, u64::from(u32::MAX) + 1] {
        let incoming = endorsement(0x51, 42, 1);
        let mut engine = engine(vec![incoming.clone()], Storage::default());
        Arc::make_mut(&mut engine.new_block).id = btc_tip(inclusion_height);

        let result = engine.validate_endorsements(&[]).await.unwrap();
        if inclusion_height < 100 {
            assert!(result.valid_endorsements.is_empty());
            assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
        } else {
            assert_eq!(
                result.valid_endorsements,
                vec![IncludedEndorsement {
                    btc_block: btc_tip(inclusion_height),
                    endorsement: incoming,
                }]
            );
        }
        assert!(result.double_signs.is_empty());
        assert!(result.removed_endorsements.is_empty());
        assert!(result.pending_endorsements.is_empty());
    }
}

#[tokio::test]
async fn premature_endorsement_does_not_conflict_with_a_stored_endorsement() {
    let mut engine = engine(
        vec![endorsement(0x51, 42, 2)],
        Storage {
            endorsements: vec![WeightedEndorsement {
                original: IncludedEndorsement {
                    btc_block: btc_tip(99),
                    endorsement: endorsement(0x51, 42, 1),
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

    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert!(result.pending_endorsements.is_empty());
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn defers_endorsements_for_unknown_blocks() {
    let incoming = endorsement(0x51, 42, 1);
    let mut engine = engine(vec![incoming.clone()], Storage::default());
    Arc::get_mut(&mut engine.chain_storage)
        .unwrap()
        .blocks
        .clear();

    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert_eq!(
        result.pending_endorsements,
        vec![IncludedEndorsement {
            btc_block: btc_tip(100),
            endorsement: incoming,
        }]
    );
    assert!(result.valid_endorsements.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
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
async fn defers_endorsements_without_a_matching_core_header() {
    let incoming = endorsement(0x51, 42, 1);
    let mut engine = engine(vec![incoming.clone()], Storage::default());
    Arc::get_mut(&mut engine.chain_storage)
        .unwrap()
        .blocks
        .get_mut(&incoming.block_hash())
        .unwrap()
        .core_block = None;

    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert_eq!(
        result.pending_endorsements,
        vec![IncludedEndorsement {
            btc_block: btc_tip(100),
            endorsement: incoming,
        }]
    );
    assert!(engine.consensus_storage.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn validates_acquisition_activity_at_the_endorsement_target_height() {
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
        let incoming = endorsement(0x51, 42, 1);
        let minter = incoming.auth().minter().p2wsh();
        let mut engine = engine(vec![incoming.clone()], Storage::default());
        let params = Arc::make_mut(&mut engine.protocol_params);
        params.acquisition_maturity = maturity;
        params.default_acquisition_duration = 7.try_into().unwrap();
        // At inclusion, the acquisition has expired in every case.
        Arc::make_mut(&mut engine.new_block).id = btc_tip(110);
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
        let result = engine.validate_endorsements(&[]).await.unwrap();
        let expected = if accepted {
            vec![IncludedEndorsement {
                btc_block: btc_tip(110),
                endorsement: incoming,
            }]
        } else {
            vec![]
        };
        assert_eq!(
            result.valid_endorsements, expected,
            "{maturity}, {duration:?}, {target}"
        );
        assert!(result.double_signs.is_empty());
        assert!(result.removed_endorsements.is_empty());
        assert!(result.pending_endorsements.is_empty());
        assert_eq!(
            engine.consensus_storage.reads.lock().unwrap().len(),
            usize::from(accepted)
        );
    }
}

#[tokio::test]
async fn finds_an_active_acquisition_separately_for_each_endorsement_target() {
    let active_target = endorsement(0x51, 42, 1);
    let expired_target = endorsement(0x51, 43, 2);
    let minter = active_target.auth().minter().p2wsh();
    let mut engine = engine(
        vec![active_target.clone(), expired_target.clone()],
        Storage::default(),
    );
    Arc::make_mut(&mut engine.new_block).id = btc_tip(110);
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
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert_eq!(
        result.valid_endorsements,
        vec![IncludedEndorsement {
            btc_block: btc_tip(110),
            endorsement: active_target,
        }]
    );
    assert!(result.double_signs.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert!(result.pending_endorsements.is_empty());
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
async fn removes_all_current_endorsements_from_double_signers_regardless_of_order() {
    let first = endorsement(0x51, 42, 1);
    let second = endorsement(0x51, 42, 2);
    let other_height = endorsement(0x51, 43, 3);
    let pending = endorsement(0x51, 44, 4);
    let other_minter = endorsement(0x52, 43, 3);
    let other_minter_pending = endorsement(0x52, 44, 4);

    for stored_conflict in [false, true] {
        let stored = WeightedEndorsement {
            original: IncludedEndorsement {
                btc_block: btc_tip(99),
                endorsement: first.clone(),
            },
            effective_minting_power: 7,
        };
        let stored_endorsements = if stored_conflict {
            vec![stored.clone()]
        } else {
            vec![]
        };
        let mut endorsements = vec![
            first.clone(),
            second.clone(),
            other_height.clone(),
            pending.clone(),
            other_minter.clone(),
            other_minter_pending.clone(),
        ];
        for _ in 0..endorsements.len() {
            let mut engine = engine(
                endorsements.clone(),
                Storage {
                    endorsements: stored_endorsements.clone(),
                    ..Storage::default()
                },
            );
            Arc::get_mut(&mut engine.chain_storage)
                .unwrap()
                .blocks
                .remove(&pending.block_hash());

            let result = engine.validate_endorsements(&[]).await.unwrap();
            assert_eq!(
                result.valid_endorsements,
                vec![IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement: other_minter.clone(),
                }]
            );
            assert_eq!(
                result.pending_endorsements,
                vec![IncludedEndorsement {
                    btc_block: btc_tip(100),
                    endorsement: other_minter_pending.clone(),
                }]
            );
            assert_eq!(result.removed_endorsements, stored_endorsements);
            assert_eq!(result.double_signs.len(), 1);
            let sign = &result.double_signs[0];
            assert_eq!(sign.minter, first.auth().minter().p2wsh());
            assert_eq!(sign.target_flame_height, 42.into());
            assert!(sign.endorsements.iter().any(|v| v.endorsement == first));
            assert!(sign.endorsements.iter().any(|v| v.endorsement == second));
            if stored_conflict {
                assert!(sign.endorsements.contains(&stored.original));
            }
            endorsements.rotate_left(1);
        }
    }
}

#[tokio::test]
async fn preserves_double_sign_evidence_at_multiple_heights() {
    let engine = engine(
        vec![
            endorsement(0x51, 42, 1),
            endorsement(0x51, 42, 2),
            endorsement(0x51, 43, 3),
            endorsement(0x51, 43, 4),
            endorsement(0x51, 44, 5),
        ],
        Storage::default(),
    );
    let result = engine.validate_endorsements(&[]).await.unwrap();
    assert!(result.valid_endorsements.is_empty());
    assert!(result.pending_endorsements.is_empty());
    assert!(result.removed_endorsements.is_empty());
    assert_eq!(result.double_signs.len(), 2);
    for height in [42, 43] {
        let sign = result
            .double_signs
            .iter()
            .find(|sign| sign.target_flame_height == height.into())
            .unwrap();
        assert_eq!(sign.endorsements.len(), 2);
        assert!(
            sign.endorsements
                .iter()
                .all(|v| v.block_height() == height.into())
        );
        assert_ne!(
            sign.endorsements[0].block_hash(),
            sign.endorsements[1].block_hash()
        );
    }
}

mod acquisition_provider;
mod endorsement_weight;
mod endorsements_outcome;
mod outcome;
mod weighter;
