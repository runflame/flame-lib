use bitcoin::{
    Amount, BlockHash as BtcBlockHash, Script, ScriptBuf, Transaction, TxIn, TxOut, Txid, Witness,
    absolute, hashes::Hash, script::PushBytes, transaction,
};
use btc_integration::{
    MintingEndorsementValidator, UncheckedMintingEndorsement, protocol::minter_witness_script,
    watcher::BlockId,
};
use flamechain::{BlockHash, CoreBlockTip};
use flamevm::Predicate;

use crate::consensus::{
    ConsensusStorage, EndorsementIndex, IncludedEndorsement, WeightedEndorsement,
    storage::{InMemoryConsensusStorage, InMemoryConsensusStorageError},
};

fn endorsements(minter: u8, height: u32, hash: u8) -> [IncludedEndorsement; 2] {
    let witness_script = minter_witness_script::build_with_authorization(
        &Predicate::opaque(Predicate::unspendable_key()),
        &ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        Script::from_bytes(&[minter]),
    );
    let mut payload = b"FLMB".to_vec();
    payload.push(1);
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(&[hash; 32]);
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::from_consensus(u32::from(minter)),
        input: vec![
            TxIn {
                witness: Witness::from_slice(&[witness_script.as_bytes()]),
                ..TxIn::default()
            };
            2
        ],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                <&PushBytes>::try_from(payload.as_slice()).unwrap(),
            ),
        }],
    };
    let unchecked: [UncheckedMintingEndorsement; 2] =
        UncheckedMintingEndorsement::from_tx(&transaction)
            .try_into()
            .unwrap();
    unchecked.map(|endorsement| IncludedEndorsement {
        btc_block: BlockId {
            hash: BtcBlockHash::from_byte_array([3; 32]),
            height: 100,
        },
        endorsement: MintingEndorsementValidator::validate(
            endorsement,
            &TxOut {
                value: Amount::from_sat(1),
                script_pubkey: witness_script.to_p2wsh(),
            },
        )
        .unwrap(),
    })
}

fn weighted_endorsement(minter: u8, height: u32, hash: u8) -> WeightedEndorsement {
    let [original, _] = endorsements(minter, height, hash);
    WeightedEndorsement {
        original,
        effective_minting_power: 42,
    }
}

#[tokio::test]
async fn removes_the_exact_bitcoin_inclusion_from_pending_and_counted_state_idempotently() {
    let storage = InMemoryConsensusStorage::new();
    let [first, other_input] = endorsements(0x51, 1, 1);
    let mut foreign = first.clone();
    foreign.btc_block.hash = BtcBlockHash::from_byte_array([4; 32]);
    let tip = first.endorsement.block_tip();
    let weighted = WeightedEndorsement {
        original: foreign.clone(),
        effective_minting_power: 42,
    };
    storage
        .store_endorsement(foreign.btc_block, &weighted)
        .await
        .unwrap();
    for entry in [&first, &other_input] {
        storage.store_pending_endorsement(tip, entry).await.unwrap();
    }
    for _ in 0..2 {
        storage.remove_endorsement_inclusion(&first).await.unwrap();
        assert_eq!(
            storage.get_endorsements_for_block(tip).await.unwrap(),
            vec![weighted.clone()]
        );
        assert_eq!(
            storage
                .get_pending_endorsements_for_block(tip)
                .await
                .unwrap(),
            vec![other_input.clone()]
        );
    }
    storage
        .store_pending_endorsement(tip, &foreign)
        .await
        .unwrap();
    for _ in 0..2 {
        storage.remove_endorsement_inclusion(&first).await.unwrap();
        assert_eq!(
            storage
                .get_minter_endorsement_state(&first.endorsement.auth().minter().p2wsh())
                .await
                .unwrap()
                .endorsements,
            vec![weighted.clone()]
        );
    }
    for _ in 0..2 {
        storage
            .remove_endorsement_inclusion(&foreign)
            .await
            .unwrap();
        assert!(
            storage
                .get_endorsements_for_block(tip)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            storage
                .get_pending_endorsements_for_block(tip)
                .await
                .unwrap(),
            vec![other_input.clone()]
        );
    }
}

#[tokio::test]
async fn restores_removed_endorsement_without_duplicates() {
    let storage = InMemoryConsensusStorage::new();
    let original = weighted_endorsement(0x51, 1, 1);
    let minter = original.original.endorsement.auth().minter().p2wsh();
    let height = original.original.block_height();
    let tip = original.original.endorsement.block_tip();
    storage
        .store_endorsement(original.original.btc_block, &original)
        .await
        .unwrap();
    let previous = storage
        .get_minter_endorsement_for_height(&minter, height)
        .await
        .unwrap();
    storage.remove_endorsement(&original).await.unwrap();
    assert!(
        storage
            .get_endorsements_for_block(tip)
            .await
            .unwrap()
            .is_empty()
    );

    for _ in 0..2 {
        storage
            .restore_endorsement(&minter, height, previous.as_ref())
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_minter_endorsement_for_height(&minter, height)
                .await
                .unwrap(),
            previous
        );
        assert_eq!(
            storage.get_endorsements_for_block(tip).await.unwrap(),
            vec![original.clone()]
        );
    }
}

#[tokio::test]
async fn restores_endorsement_over_replacement_and_preserves_other_keys() {
    let storage = InMemoryConsensusStorage::new();
    let original = weighted_endorsement(0x51, 1, 1);
    let mut replacement = weighted_endorsement(0x51, 1, 2);
    replacement.original.btc_block = BlockId {
        hash: BtcBlockHash::from_byte_array([4; 32]),
        height: 101,
    };
    replacement.effective_minting_power = 99;
    let minter = original.original.endorsement.auth().minter().p2wsh();
    let height = original.original.block_height();
    let other_minter = weighted_endorsement(0x52, 1, 1);
    let other_height = weighted_endorsement(0x51, 2, 3);
    for endorsement in [&replacement, &other_minter, &other_height] {
        storage
            .store_endorsement(endorsement.original.btc_block, endorsement)
            .await
            .unwrap();
    }
    assert_eq!(
        storage
            .store_endorsement(original.original.btc_block, &original)
            .await,
        Err(InMemoryConsensusStorageError::ConflictingRecord)
    );

    storage
        .restore_endorsement(&minter, height, Some(&original))
        .await
        .unwrap();
    assert_eq!(
        storage
            .get_minter_endorsement_for_height(&minter, height)
            .await
            .unwrap(),
        Some(original)
    );
    assert!(
        storage
            .get_endorsements_for_block(replacement.original.endorsement.block_tip())
            .await
            .unwrap()
            .is_empty()
    );
    for _ in 0..2 {
        storage
            .restore_endorsement(&minter, height, None)
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_minter_endorsement_for_height(&minter, height)
                .await
                .unwrap(),
            None
        );
    }
    for endorsement in [&other_minter, &other_height] {
        assert_eq!(
            storage
                .get_endorsements_for_block(endorsement.original.endorsement.block_tip())
                .await
                .unwrap(),
            vec![endorsement.clone()]
        );
    }
}

#[tokio::test]
async fn rejects_mismatched_endorsement_keys_without_changes() {
    let storage = InMemoryConsensusStorage::new();
    let original = weighted_endorsement(0x51, 1, 1);
    let other_minter = weighted_endorsement(0x52, 1, 1);
    let other_height = weighted_endorsement(0x51, 2, 1);
    for endorsement in [&original, &other_minter, &other_height] {
        storage
            .store_endorsement(endorsement.original.btc_block, endorsement)
            .await
            .unwrap();
    }
    let before = storage
        .read_state()
        .unwrap()
        .endorsements
        .endorsements
        .clone();

    for endorsement in [&other_minter, &other_height] {
        assert_eq!(
            storage
                .restore_endorsement(
                    &endorsement.original.endorsement.auth().minter().p2wsh(),
                    endorsement.original.block_height(),
                    Some(&original),
                )
                .await,
            Err(InMemoryConsensusStorageError::EndorsementKeyMismatch)
        );
        assert_eq!(
            storage.read_state().unwrap().endorsements.endorsements,
            before
        );
    }
}

#[tokio::test]
async fn reads_pending_endorsements_by_block_transaction_and_input() {
    let storage = InMemoryConsensusStorage::new();
    let endorsements = endorsements(0x51, 1, 1);
    let tip = endorsements[0].endorsement.block_tip();
    let index = EndorsementIndex::from(&endorsements[0].endorsement);
    assert_eq!(
        storage.get_pending_endorsement(tip, index).await.unwrap(),
        None
    );
    for endorsement in &endorsements {
        storage
            .store_pending_endorsement(tip, endorsement)
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_pending_endorsement(tip, EndorsementIndex::from(&endorsement.endorsement))
                .await
                .unwrap(),
            Some(endorsement.clone())
        );
    }
    for (wrong_tip, wrong_index) in [
        (
            CoreBlockTip {
                height: 2.into(),
                ..tip
            },
            index,
        ),
        (
            CoreBlockTip {
                hash: BlockHash::from([2; 32]),
                ..tip
            },
            index,
        ),
        (
            tip,
            EndorsementIndex {
                txid: Txid::from_byte_array([9; 32]),
                ..index
            },
        ),
        (
            tip,
            EndorsementIndex {
                input_index: 2,
                ..index
            },
        ),
    ] {
        assert_eq!(
            storage
                .get_pending_endorsement(wrong_tip, wrong_index)
                .await
                .unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn reads_all_pending_for_the_exact_block_without_consuming_or_exposing_storage() {
    let storage = InMemoryConsensusStorage::new();
    let [first, second] = endorsements(0x51, 1, 1);
    let [another_transaction, _] = endorsements(0x52, 1, 1);
    let [other_hash, _] = endorsements(0x51, 1, 2);
    let [other_height, _] = endorsements(0x51, 2, 1);
    let tip = first.endorsement.block_tip();
    assert!(
        storage
            .get_pending_endorsements_for_block(tip)
            .await
            .unwrap()
            .is_empty()
    );
    for endorsement in [
        &second,
        &another_transaction,
        &first,
        &other_hash,
        &other_height,
    ] {
        storage
            .store_pending_endorsement(endorsement.endorsement.block_tip(), endorsement)
            .await
            .unwrap();
    }
    let mut expected = vec![first, second, another_transaction];
    expected.sort_by_key(|included| EndorsementIndex::from(&included.endorsement));
    for _ in 0..2 {
        assert_eq!(
            storage
                .get_pending_endorsements_for_block(tip)
                .await
                .unwrap(),
            expected
        );
    }
    let mut snapshot = storage
        .get_pending_endorsements_for_block(tip)
        .await
        .unwrap();
    snapshot[0].btc_block.height = 999;
    snapshot.clear();
    assert_eq!(
        storage
            .get_pending_endorsements_for_block(tip)
            .await
            .unwrap(),
        expected
    );
    for endorsement in [other_hash, other_height] {
        assert_eq!(
            storage
                .get_pending_endorsements_for_block(endorsement.endorsement.block_tip())
                .await
                .unwrap(),
            vec![endorsement]
        );
    }
}

#[tokio::test]
async fn returns_pending_in_bitcoin_inclusion_order() {
    let storage = InMemoryConsensusStorage::new();
    let [mut lower_index, mut higher_index] = endorsements(0x51, 1, 1);
    lower_index.btc_block.height = 102;
    higher_index.btc_block.height = 100;
    let tip = lower_index.endorsement.block_tip();
    for endorsement in [&lower_index, &higher_index] {
        storage
            .store_pending_endorsement(tip, endorsement)
            .await
            .unwrap();
    }
    assert_eq!(
        storage
            .get_pending_endorsements_for_block(tip)
            .await
            .unwrap(),
        vec![higher_index, lower_index]
    );
}

#[tokio::test]
async fn restores_pending_endorsement_inclusion_without_duplicates() {
    let storage = InMemoryConsensusStorage::new();
    let [original, _] = endorsements(0x51, 1, 1);
    let tip = original.endorsement.block_tip();
    let index = EndorsementIndex::from(&original.endorsement);
    storage
        .restore_pending_endorsement(tip, index, Some(&original))
        .await
        .unwrap();
    let previous = storage.get_pending_endorsement(tip, index).await.unwrap();
    assert_eq!(previous, Some(original.clone()));
    let mut replacement = original.clone();
    replacement.btc_block = BlockId {
        hash: BtcBlockHash::from_byte_array([4; 32]),
        height: 101,
    };
    storage
        .restore_pending_endorsement(tip, index, Some(&replacement))
        .await
        .unwrap();
    assert_eq!(
        storage.get_pending_endorsement(tip, index).await.unwrap(),
        Some(replacement)
    );
    assert_eq!(
        storage.store_pending_endorsement(tip, &original).await,
        Err(InMemoryConsensusStorageError::ConflictingRecord)
    );

    for _ in 0..2 {
        storage
            .restore_pending_endorsement(tip, index, previous.as_ref())
            .await
            .unwrap();
        assert_eq!(
            storage.get_pending_endorsement(tip, index).await.unwrap(),
            previous
        );
        assert_eq!(
            storage
                .read_state()
                .unwrap()
                .endorsements
                .pending_endorsements[&tip]
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn restores_pending_absence_and_removes_only_empty_groups() {
    let storage = InMemoryConsensusStorage::new();
    let [first, second] = endorsements(0x51, 1, 1);
    let [other, _] = endorsements(0x51, 1, 2);
    let tip = first.endorsement.block_tip();
    let index = EndorsementIndex::from(&first.endorsement);
    let other_tip = other.endorsement.block_tip();
    storage
        .restore_pending_endorsement(tip, index, None)
        .await
        .unwrap();
    assert!(
        storage
            .read_state()
            .unwrap()
            .endorsements
            .pending_endorsements
            .is_empty()
    );
    for endorsement in [&first, &second, &other] {
        storage
            .store_pending_endorsement(endorsement.endorsement.block_tip(), endorsement)
            .await
            .unwrap();
    }

    for _ in 0..2 {
        storage
            .restore_pending_endorsement(tip, index, None)
            .await
            .unwrap();
        assert_eq!(
            storage.get_pending_endorsement(tip, index).await.unwrap(),
            None
        );
        assert_eq!(
            storage
                .get_pending_endorsement(
                    tip,
                    EndorsementIndex {
                        input_index: 1,
                        ..index
                    }
                )
                .await
                .unwrap(),
            Some(second.clone())
        );
    }
    storage
        .restore_pending_endorsement(
            tip,
            EndorsementIndex {
                input_index: 1,
                ..index
            },
            None,
        )
        .await
        .unwrap();
    assert!(
        !storage
            .read_state()
            .unwrap()
            .endorsements
            .pending_endorsements
            .contains_key(&tip)
    );
    assert_eq!(
        storage
            .get_pending_endorsement(other_tip, EndorsementIndex::from(&other.endorsement))
            .await
            .unwrap(),
        Some(other.clone())
    );
    storage
        .restore_pending_endorsement(other_tip, EndorsementIndex::from(&other.endorsement), None)
        .await
        .unwrap();
    assert!(
        storage
            .read_state()
            .unwrap()
            .endorsements
            .pending_endorsements
            .is_empty()
    );
}

#[tokio::test]
async fn rejects_mismatched_pending_keys_without_changes() {
    let storage = InMemoryConsensusStorage::new();
    let [first, second] = endorsements(0x51, 1, 1);
    let tip = first.endorsement.block_tip();
    let index = EndorsementIndex::from(&first.endorsement);
    for endorsement in [&first, &second] {
        storage
            .store_pending_endorsement(tip, endorsement)
            .await
            .unwrap();
    }
    let before = storage
        .read_state()
        .unwrap()
        .endorsements
        .pending_endorsements
        .clone();

    for (wrong_tip, wrong_index, expected_error) in [
        (
            CoreBlockTip {
                height: 2.into(),
                ..tip
            },
            index,
            InMemoryConsensusStorageError::CoreBlockMismatch,
        ),
        (
            CoreBlockTip {
                hash: BlockHash::from([2; 32]),
                ..tip
            },
            index,
            InMemoryConsensusStorageError::CoreBlockMismatch,
        ),
        (
            tip,
            EndorsementIndex {
                txid: Txid::from_byte_array([9; 32]),
                ..index
            },
            InMemoryConsensusStorageError::PendingEndorsementKeyMismatch,
        ),
        (
            tip,
            EndorsementIndex {
                input_index: 1,
                ..index
            },
            InMemoryConsensusStorageError::PendingEndorsementKeyMismatch,
        ),
    ] {
        assert_eq!(
            storage
                .restore_pending_endorsement(wrong_tip, wrong_index, Some(&first))
                .await,
            Err(expected_error)
        );
        assert_eq!(
            storage
                .read_state()
                .unwrap()
                .endorsements
                .pending_endorsements,
            before
        );
    }
}
