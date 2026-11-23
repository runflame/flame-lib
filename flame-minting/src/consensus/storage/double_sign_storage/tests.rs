use std::num::NonZeroU16;

use bitcoin::{BlockHash as BtcBlockHash, hashes::Hash};
use btc_integration::{MinterP2wsh, watcher::BlockId};
use flamechain::{BlockHash, CoreBlockTip};

use crate::consensus::{
    ConsensusStorage, DoubleSign, IncludedEndorsement, MintingProtocolParams,
    engine::tests::endorsement_for_tip,
    storage::{InMemoryConsensusStorage, InMemoryConsensusStorageError},
};

fn endorsement(minter: u8, height: u32, hash: u8) -> IncludedEndorsement {
    IncludedEndorsement {
        btc_block: BlockId {
            hash: BtcBlockHash::from_byte_array([3; 32]),
            height: 100,
        },
        endorsement: endorsement_for_tip(
            minter,
            CoreBlockTip {
                hash: BlockHash::from([hash; 32]),
                height: height.into(),
            },
        ),
    }
}

fn double_sign(minter: u8, height: u32) -> DoubleSign {
    let first = endorsement(minter, height, 1);
    DoubleSign {
        minter: first.endorsement.auth().minter().p2wsh(),
        target_flame_height: height.into(),
        endorsements: vec![first, endorsement(minter, height, 2)],
    }
}

#[tokio::test]
async fn removes_only_matching_inclusions_and_keeps_evidence_while_hashes_conflict() {
    let storage = InMemoryConsensusStorage::new();
    let mut sign = double_sign(0x51, 1);
    let removed = sign.endorsements[1].clone();
    let mut foreign = removed.clone();
    foreign.btc_block.hash = BtcBlockHash::from_byte_array([4; 32]);
    sign.endorsements.push(foreign.clone());
    storage.add_double_sign(&sign).await.unwrap();
    for _ in 0..2 {
        storage
            .remove_endorsement_inclusion(&removed)
            .await
            .unwrap();
        let proof = storage
            .get_double_sign(&sign.minter, sign.target_flame_height)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            proof.endorsements,
            vec![sign.endorsements[0].clone(), foreign.clone()]
        );
        assert_minter_status(&storage, &sign.minter, true).await;
    }
    for _ in 0..2 {
        storage
            .remove_endorsement_inclusion(&foreign)
            .await
            .unwrap();
        assert_minter_status(&storage, &sign.minter, false).await;
        assert!(
            storage
                .get_double_sign(&sign.minter, sign.target_flame_height)
                .await
                .unwrap()
                .is_none()
        );
    }
}

async fn assert_minter_status(
    storage: &InMemoryConsensusStorage,
    minter: &MinterP2wsh,
    expected: bool,
) {
    assert_eq!(
        storage.is_minter_double_signed(minter).await.unwrap(),
        expected
    );
    let params = MintingProtocolParams {
        acquisition_maturity: 1,
        default_acquisition_duration: NonZeroU16::MIN,
        min_acquisition_duration: NonZeroU16::MIN,
        max_endorsement_delay: 10,
    };
    let acquisitions = storage
        .get_active_minter_acquisitions_at_height(minter, 100, &params)
        .await
        .unwrap();
    assert_eq!(acquisitions.is_double_signed, expected);
}

#[tokio::test]
async fn restores_exact_previous_record_after_endorsements_were_merged() {
    let storage = InMemoryConsensusStorage::new();
    let original = double_sign(0x51, 1);
    let minter = original.minter;
    let height = original.target_flame_height;
    storage.add_double_sign(&original).await.unwrap();
    let previous = storage.get_double_sign(&minter, height).await.unwrap();
    let mut extended = original.clone();
    extended.endorsements.push(endorsement(0x51, 1, 3));
    storage.add_double_sign(&extended).await.unwrap();
    assert_eq!(
        storage.get_double_sign(&minter, height).await.unwrap(),
        Some(extended)
    );

    for _ in 0..2 {
        storage
            .restore_double_sign(&minter, height, previous.as_ref())
            .await
            .unwrap();
        assert_eq!(
            storage.get_double_sign(&minter, height).await.unwrap(),
            Some(original.clone())
        );
        assert_minter_status(&storage, &minter, true).await;
    }
}

#[tokio::test]
async fn restores_missing_record_and_absence_idempotently() {
    let storage = InMemoryConsensusStorage::new();
    let original = double_sign(0x51, 1);
    let minter = original.minter;
    let height = original.target_flame_height;
    let previous = storage.get_double_sign(&minter, height).await.unwrap();
    assert_eq!(previous, None);
    storage
        .restore_double_sign(&minter, height, None)
        .await
        .unwrap();
    assert_minter_status(&storage, &minter, false).await;

    for _ in 0..2 {
        storage
            .restore_double_sign(&minter, height, Some(&original))
            .await
            .unwrap();
        assert_eq!(
            storage.get_double_sign(&minter, height).await.unwrap(),
            Some(original.clone())
        );
        assert_minter_status(&storage, &minter, true).await;
    }
    for _ in 0..2 {
        storage
            .restore_double_sign(&minter, height, previous.as_ref())
            .await
            .unwrap();
        assert_eq!(
            storage.get_double_sign(&minter, height).await.unwrap(),
            None
        );
        assert_minter_status(&storage, &minter, false).await;
    }
}

#[tokio::test]
async fn removing_one_violation_preserves_other_heights_and_minters() {
    let storage = InMemoryConsensusStorage::new();
    let first = double_sign(0x51, 1);
    let second = double_sign(0x51, 2);
    let other = double_sign(0x52, 1);
    for sign in [&first, &second, &other] {
        storage.add_double_sign(sign).await.unwrap();
    }
    for _ in 0..2 {
        storage
            .restore_double_sign(&first.minter, first.target_flame_height, None)
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_double_sign(&first.minter, first.target_flame_height)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_double_sign(&second.minter, second.target_flame_height)
                .await
                .unwrap(),
            Some(second.clone())
        );
        assert_minter_status(&storage, &first.minter, true).await;
    }
    storage
        .restore_double_sign(&second.minter, second.target_flame_height, None)
        .await
        .unwrap();
    assert_minter_status(&storage, &first.minter, false).await;
    assert_minter_status(&storage, &other.minter, true).await;
    assert_eq!(
        storage
            .get_double_sign(&other.minter, other.target_flame_height)
            .await
            .unwrap(),
        Some(other)
    );
}

#[tokio::test]
async fn rejects_mismatched_keys_without_changing_records_or_status() {
    let storage = InMemoryConsensusStorage::new();
    let original = double_sign(0x51, 1);
    let other = double_sign(0x52, 1);
    let other_height = double_sign(0x51, 2);
    for sign in [&original, &other, &other_height] {
        storage.add_double_sign(sign).await.unwrap();
    }
    let before = storage.read_state().unwrap().double_signs.signs.clone();
    let unknown_minter = MinterP2wsh::from([9; 32]);

    for (minter, height) in [
        (other.minter, original.target_flame_height),
        (original.minter, other_height.target_flame_height),
        (unknown_minter, original.target_flame_height),
    ] {
        assert_eq!(
            storage
                .restore_double_sign(&minter, height, Some(&original))
                .await,
            Err(InMemoryConsensusStorageError::DoubleSignKeyMismatch)
        );
        assert_eq!(storage.read_state().unwrap().double_signs.signs, before);
        assert_minter_status(&storage, &original.minter, true).await;
        assert_minter_status(&storage, &other.minter, true).await;
        assert_minter_status(&storage, &unknown_minter, false).await;
    }
}

#[tokio::test]
async fn rejects_invalid_evidence_before_creating_or_replacing_a_record() {
    let original = double_sign(0x51, 1);
    let first = original.endorsements[0].clone();
    let invalid_endorsements = [
        vec![],
        vec![first.clone()],
        vec![first.clone(), first.clone()],
        vec![first.clone(), endorsement(0x52, 1, 2)],
        vec![first, endorsement(0x51, 2, 2)],
    ];
    for existing in [false, true] {
        let storage = InMemoryConsensusStorage::new();
        if existing {
            storage.add_double_sign(&original).await.unwrap();
        }
        let before = storage.read_state().unwrap().double_signs.signs.clone();
        for endorsements in &invalid_endorsements {
            let invalid = DoubleSign {
                endorsements: endorsements.clone(),
                ..original.clone()
            };
            assert_eq!(
                storage
                    .restore_double_sign(
                        &original.minter,
                        original.target_flame_height,
                        Some(&invalid)
                    )
                    .await,
                Err(InMemoryConsensusStorageError::InvalidDoubleSign)
            );
            assert_eq!(storage.read_state().unwrap().double_signs.signs, before);
            assert_minter_status(&storage, &original.minter, existing).await;
        }
    }
}
