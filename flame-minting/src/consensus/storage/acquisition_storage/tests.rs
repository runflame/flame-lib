use bitcoin::{Amount, BlockHash, Transaction, TxOut, Txid, absolute, hashes::Hash, transaction};
use btc_integration::{Burn, BurnData, watcher::BlockId};
use flamevm::Predicate;

use crate::consensus::{
    AcquisitionIndex, ConsensusStorage, IncludedAcquisition,
    storage::{InMemoryConsensusStorage, InMemoryConsensusStorageError},
};

fn acquisitions() -> [IncludedAcquisition; 2] {
    let data = BurnData::new(
        [1; 32].into(),
        Predicate::opaque(Predicate::unspendable_key()),
    );
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: Vec::new(),
        output: [10, 20]
            .into_iter()
            .map(|amount| TxOut {
                value: Amount::from_sat(amount),
                script_pubkey: data.to_script(),
            })
            .collect(),
    };
    Burn::from_tx(&transaction)
        .into_iter()
        .map(|acquisition| IncludedAcquisition {
            btc_block: BlockId {
                hash: BlockHash::from_byte_array([3; 32]),
                height: 100,
            },
            acquisition,
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

#[tokio::test]
async fn reads_acquisitions_by_transaction_and_output_index() {
    let storage = InMemoryConsensusStorage::new();
    let acquisitions = acquisitions();
    let index = AcquisitionIndex::from(&acquisitions[0].acquisition);
    assert_eq!(storage.get_acquisition(index).await.unwrap(), None);

    for acquisition in &acquisitions {
        storage
            .store_acquisition(acquisition.btc_block, acquisition)
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_acquisition(AcquisitionIndex::from(&acquisition.acquisition))
                .await
                .unwrap(),
            Some(acquisition.clone())
        );
    }
    assert_eq!(
        storage
            .get_acquisition(AcquisitionIndex {
                output_index: 2,
                ..index
            })
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        storage
            .get_acquisition(AcquisitionIndex {
                txid: Txid::from_byte_array([9; 32]),
                ..index
            })
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn restores_missing_acquisition_and_repeated_restore_is_idempotent() {
    let storage = InMemoryConsensusStorage::new();
    let [previous, _] = acquisitions();
    let index = AcquisitionIndex::from(&previous.acquisition);

    for _ in 0..2 {
        storage
            .restore_acquisition(index, Some(&previous))
            .await
            .unwrap();
        assert_eq!(
            storage.get_acquisition(index).await.unwrap(),
            Some(previous.clone())
        );
    }
    let minters = storage.get_acquisitions_by_minters(100).await.unwrap();
    assert_eq!(minters.len(), 1);
    assert_eq!(
        minters[&previous.acquisition.data().minter_p2wsh].acquisitions,
        vec![previous.clone()]
    );
}

#[tokio::test]
async fn restores_previous_bitcoin_inclusion_and_height_queries() {
    let storage = InMemoryConsensusStorage::new();
    let [previous, _] = acquisitions();
    let index = AcquisitionIndex::from(&previous.acquisition);
    let mut current = previous.clone();
    current.btc_block = BlockId {
        hash: BlockHash::from_byte_array([4; 32]),
        height: 101,
    };
    storage
        .store_acquisition(current.btc_block, &current)
        .await
        .unwrap();
    assert!(
        storage
            .get_acquisitions_by_minters(100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        storage
            .store_acquisition(previous.btc_block, &previous)
            .await,
        Err(InMemoryConsensusStorageError::ConflictingRecord)
    );
    let saved = storage.get_acquisition(index).await.unwrap();
    assert_eq!(saved, Some(current));

    storage
        .restore_acquisition(index, Some(&previous))
        .await
        .unwrap();
    assert_eq!(
        storage.get_acquisition(index).await.unwrap(),
        Some(previous.clone())
    );
    let minters = storage.get_acquisitions_by_minters(100).await.unwrap();
    assert_eq!(
        minters[&previous.acquisition.data().minter_p2wsh].acquisitions,
        vec![previous]
    );

    storage
        .restore_acquisition(index, saved.as_ref())
        .await
        .unwrap();
    assert_eq!(storage.get_acquisition(index).await.unwrap(), saved);
    assert!(
        storage
            .get_acquisitions_by_minters(100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn restores_absence_without_removing_other_outputs() {
    let storage = InMemoryConsensusStorage::new();
    let [first, second] = acquisitions();
    let index = AcquisitionIndex::from(&first.acquisition);
    let previous = storage.get_acquisition(index).await.unwrap();
    for acquisition in [&first, &second] {
        storage
            .store_acquisition(acquisition.btc_block, acquisition)
            .await
            .unwrap();
    }

    for _ in 0..2 {
        storage
            .restore_acquisition(index, previous.as_ref())
            .await
            .unwrap();
        assert_eq!(storage.get_acquisition(index).await.unwrap(), None);
        assert_eq!(
            storage
                .get_acquisition(AcquisitionIndex {
                    output_index: 1,
                    ..index
                })
                .await
                .unwrap(),
            Some(second.clone())
        );
    }
    let minters = storage.get_acquisitions_by_minters(100).await.unwrap();
    assert_eq!(
        minters[&second.acquisition.data().minter_p2wsh].acquisitions,
        vec![second]
    );
    storage
        .restore_acquisition(
            AcquisitionIndex {
                output_index: 1,
                ..index
            },
            None,
        )
        .await
        .unwrap();
    assert!(
        storage
            .get_acquisitions_by_minters(100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn rejects_mismatched_keys_without_changing_storage() {
    let storage = InMemoryConsensusStorage::new();
    let [first, second] = acquisitions();
    let index = AcquisitionIndex::from(&first.acquisition);
    for acquisition in [&first, &second] {
        storage
            .store_acquisition(acquisition.btc_block, acquisition)
            .await
            .unwrap();
    }
    let before = storage.get_acquisitions_by_minters(100).await.unwrap();

    for wrong_index in [
        AcquisitionIndex {
            txid: Txid::from_byte_array([9; 32]),
            ..index
        },
        AcquisitionIndex {
            output_index: 1,
            ..index
        },
    ] {
        assert_eq!(
            storage.restore_acquisition(wrong_index, Some(&first)).await,
            Err(InMemoryConsensusStorageError::AcquisitionKeyMismatch)
        );
        assert_eq!(
            storage.get_acquisitions_by_minters(100).await.unwrap(),
            before
        );
    }
}
