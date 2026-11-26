use bitcoin::{
    Amount, Script, ScriptBuf, Transaction, TxIn, TxOut, Witness, absolute, hashes::Hash,
    script::PushBytes, transaction,
};
use btc_integration::{
    MintingEndorsementValidator, UncheckedMintingEndorsement, protocol::minter_witness_script,
};
use flamevm::Predicate;

use super::*;

fn endorsement(height: u32, hash: u8) -> MintingEndorsementData {
    MintingEndorsementData::V1 {
        flame_block_height: height,
        flame_block_hash: BlockHash::from([hash; 32]),
    }
}

fn authenticated_endorsement(
    data: &MintingEndorsementData,
    minter: u8,
) -> AuthenticatedMintingEndorsement {
    let witness_script = minter_witness_script::build_with_authorization(
        &Predicate::opaque(Predicate::unspendable_key()),
        &ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        Script::from_bytes(&[minter]),
    );
    let mut payload = b"FLMB".to_vec();
    payload.push(1);
    payload.extend_from_slice(&data.block_height().as_u32().to_le_bytes());
    payload.extend_from_slice(data.block_hash().as_bytes());
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            witness: Witness::from_slice(&[witness_script.as_bytes()]),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                <&PushBytes>::try_from(payload.as_slice()).unwrap(),
            ),
        }],
    };
    MintingEndorsementValidator::validate(
        UncheckedMintingEndorsement::from_tx(&transaction)
            .pop()
            .unwrap(),
        &TxOut {
            value: Amount::from_sat(1),
            script_pubkey: witness_script.to_p2wsh(),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn replayed_writes_preserve_confirmation_and_reservation_across_clones() {
    let journal = InMemoryMinterJournal::new();
    let clone = journal.clone();
    let data = endorsement(1, 1);
    let confirmed = authenticated_endorsement(&data, 0x51);
    assert_eq!(
        journal.write_reserve_endorsement(&data).await.unwrap(),
        EndorsementReservation::Reserved
    );
    for _ in 0..2 {
        clone
            .write_sent_endorsement(&data, &confirmed.txid())
            .await
            .unwrap();
        journal
            .write_committed_endorsement(&confirmed)
            .await
            .unwrap();
        assert_eq!(
            clone.write_reserve_endorsement(&data).await.unwrap(),
            EndorsementReservation::AlreadyReserved
        );
    }
    clone
        .write_sent_endorsement(&data, &confirmed.txid())
        .await
        .unwrap();
    {
        let endorsements = journal.endorsements.read().unwrap();
        let record = endorsements.get(&data.block_height()).unwrap();
        assert!(
            matches!(&record.state, EndorsementState::Committed(stored) if stored == &confirmed)
        );
    }
    assert_eq!(
        journal
            .write_reserve_endorsement(&endorsement(2, 2))
            .await
            .unwrap(),
        EndorsementReservation::Reserved
    );
}

#[tokio::test]
async fn conflicting_blocks_cannot_replace_an_endorsement_at_any_stage() {
    let journal = InMemoryMinterJournal::new();
    let data = endorsement(1, 1);
    let conflicting = endorsement(1, 2);
    let confirmed = authenticated_endorsement(&data, 0x51);
    let conflicting_confirmation = authenticated_endorsement(&conflicting, 0x51);
    journal.write_reserve_endorsement(&data).await.unwrap();
    for stage in 0..3 {
        let expected = || InMemoryMinterJournalError::ConflictingEndorsement {
            height: data.block_height(),
            expected: data.block_hash(),
            actual: conflicting.block_hash(),
        };
        assert_eq!(
            journal.write_reserve_endorsement(&conflicting).await,
            Err(expected())
        );
        assert_eq!(
            journal
                .write_sent_endorsement(&conflicting, &confirmed.txid())
                .await,
            Err(expected())
        );
        assert_eq!(
            journal
                .write_committed_endorsement(&conflicting_confirmation)
                .await,
            Err(expected())
        );
        match stage {
            0 => journal
                .write_sent_endorsement(&data, &confirmed.txid())
                .await
                .unwrap(),
            1 => journal
                .write_committed_endorsement(&confirmed)
                .await
                .unwrap(),
            _ => {}
        }
    }
}

#[tokio::test]
async fn writes_require_reservation_and_confirmation_requires_a_matching_sent_transaction() {
    let journal = InMemoryMinterJournal::new();
    let data = endorsement(1, 1);
    let confirmed = authenticated_endorsement(&data, 0x51);
    let other_txid = Txid::from_byte_array([0x42; 32]);
    assert_eq!(
        journal
            .write_sent_endorsement(&data, &confirmed.txid())
            .await,
        Err(InMemoryMinterJournalError::EndorsementNotReserved(
            data.block_height()
        ))
    );
    assert_eq!(
        journal.write_committed_endorsement(&confirmed).await,
        Err(InMemoryMinterJournalError::EndorsementNotReserved(
            data.block_height()
        ))
    );
    journal.write_reserve_endorsement(&data).await.unwrap();
    assert_eq!(
        journal.write_committed_endorsement(&confirmed).await,
        Err(InMemoryMinterJournalError::EndorsementNotSent(
            data.block_height()
        ))
    );
    journal
        .write_sent_endorsement(&data, &other_txid)
        .await
        .unwrap();
    assert_eq!(
        journal.write_committed_endorsement(&confirmed).await,
        Err(InMemoryMinterJournalError::TransactionIdMismatch {
            expected: other_txid,
            actual: confirmed.txid()
        })
    );
    assert_eq!(
        journal
            .write_sent_endorsement(&data, &confirmed.txid())
            .await,
        Err(InMemoryMinterJournalError::TransactionIdMismatch {
            expected: other_txid,
            actual: confirmed.txid()
        })
    );
    let endorsements = journal.endorsements.read().unwrap();
    assert!(
        matches!(endorsements.get(&data.block_height()).unwrap().state, EndorsementState::Sent(txid) if txid == other_txid)
    );
}

#[test]
fn concurrent_reservations_allow_only_one_writer_for_the_same_height() {
    for hashes in [[1, 1], [1, 2]] {
        let journal = InMemoryMinterJournal::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let writers: Vec<_> = hashes
            .into_iter()
            .map(|hash| {
                let journal = journal.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap();
                    let data = endorsement(1, hash);
                    barrier.wait();
                    let result = runtime.block_on(journal.write_reserve_endorsement(&data));
                    (data, result)
                })
            })
            .collect();
        let results: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|(_, result)| *result == Ok(EndorsementReservation::Reserved))
                .count(),
            1
        );
        let records = journal.endorsements.read().unwrap();
        assert_eq!(records.len(), 1);
        let stored = &records.get(&1.into()).unwrap().endorsement;
        for (data, result) in results {
            if data == *stored {
                assert!(matches!(
                    result,
                    Ok(EndorsementReservation::Reserved | EndorsementReservation::AlreadyReserved)
                ));
            } else {
                assert_eq!(
                    result,
                    Err(InMemoryMinterJournalError::ConflictingEndorsement {
                        height: data.block_height(),
                        expected: stored.block_hash(),
                        actual: data.block_hash(),
                    })
                );
            }
        }
    }
}
