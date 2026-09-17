//! Frozen Cell-based consensus vectors, generated independently from descriptor
//! bytes, SHA256, and the documented radix-4 path layout (not VM encoders).

use super::test_helpers::*;
use crate::{code_root, state_root};

fn hex(bytes: &[u8]) -> std::string::String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn golden_consensus_hashes() {
    let header = || TxEntry::Header(dummy_header());
    let actor = ActorID::Hash([7; 32]);
    let samples = [
        (
            vec![header()],
            "881bb76904e627c9c79c0a525987f072024033dc78d45743df418b4e601dd62c",
        ),
        (
            vec![header(), TxEntry::Data(vec![1, 2, 3])],
            "d78b7ff69a8c3f308d1c347460c802f1ad39aad093e54e3f9d7a96bbdb09de84",
        ),
        (
            vec![header(), TxEntry::Fee(1000)],
            "191ca034b9a0acd11e92f87accdff54fef6b0d631e81e1d4fc123cfdc3c810f5",
        ),
        (
            vec![
                header(),
                TxEntry::ActorSave {
                    actor: actor.clone(),
                    state: Value::Scalar(Scalar::from(42u64)),
                },
            ],
            "f1ee34ac75a5c37e528ca38cd498860a5aac7c4352dd2d94ecce849121963ebc",
        ),
        (
            vec![
                header(),
                TxEntry::SetCode {
                    actor: actor.clone(),
                    code: vec![0x1d],
                },
            ],
            "864ce848ec493fe3df21e8b765be7613a65b0f07828bd1dc89c36c24eb25e8f0",
        ),
        (
            vec![
                header(),
                TxEntry::ActorDeploy {
                    actor,
                    code: vec![0x1d],
                },
            ],
            "cda8173e68708ad6c830610128d6b329e920b111ea35127df580ed443053791d",
        ),
    ];
    for (entries, expected) in samples {
        assert_eq!(hex(TxID::from_log(&entries).as_bytes()), expected);
        let log = crate::TxLog::from(entries);
        let decoded: crate::TxLog = decode_envelope(&log.to_envelope().unwrap().encode()).unwrap();
        assert_eq!(decoded.txid(), log.txid());
    }
    assert_eq!(
        hex(&state_root(&Value::Scalar(Scalar::from(42u64)))),
        "4377380d51c730f99228f59486cdd68bfcc79ab9281fc80c249bb8478984ea35"
    );
    assert_eq!(
        hex(&code_root(&[0x1d])),
        "9bce55aad1742ba6593542c4e6f8162568c1f7c211e7c46d6ad29542f9150a5c"
    );
    assert_eq!(
        hex(&ActorID::Constructor(vec![1, 2, 3]).to_hash()),
        "2ce7935d93d22e90e74c4e780d5fcf361607b431fbb1f8b217dc6fea5da9baff"
    );
    let contract = Contract::new(
        Predicate::opaque(CompressedRistretto([0x77; 32])),
        Anchor([0x88; 32]),
        Value::Scalar(Scalar::from(9u64)),
    )
    .unwrap();
    assert_eq!(
        hex(&contract.id()),
        "5b4cd468f4a5c19949c7791ecde65394b785b909345195928374114044d078f0"
    );
    assert_eq!(
        hex(TxID::from_log(&[
            header(),
            TxEntry::Input(contract.id()),
            TxEntry::Output(contract)
        ])
        .as_bytes()),
        "9773ed29353f6b97cce5f516a03aa471c798a9c88f6b58664721e9b205301c2f"
    );
    assert_ne!(
        TxID::from_log(&[header(), TxEntry::Data(vec![1]), TxEntry::Fee(7)]),
        TxID::from_log(&[header(), TxEntry::Fee(7), TxEntry::Data(vec![1])])
    );
}

/// A raw record writer independent of CellBuilder and all type codecs.
fn record(payload: &[u8], refs: &[[u8; 32]]) -> Vec<u8> {
    let descriptor = payload.len() as u16 | ((refs.len() as u16) << 13);
    let mut bytes = descriptor.to_le_bytes().to_vec();
    bytes.extend_from_slice(payload);
    for reference in refs {
        bytes.extend_from_slice(reference);
    }
    bytes
}

#[test]
fn golden_string_is_raw_bytes_in_one_cell() {
    let string = String::from(b"abc".to_vec());
    let cell = string.to_cell().unwrap();
    assert_eq!(cell.encode(), record(b"abc", &[]));
    assert_eq!(hex(&cell.encode()), "0300616263");
    assert_eq!(
        hex(&cell.id()),
        "c602df104f2bfde94e05197faa97f07dda1d43d559f0c423f93e9bb9669d24ff"
    );
    let value = Value::String(string).to_cell().unwrap();
    assert_eq!(value.encode(), record(&[1], &[cell.id()]));
    assert_eq!(
        hex(&value.id()),
        "d2b64a4e5655f7a299aa1898303e2e9024569783fe55527f38cc5624a63d8f0b"
    );
}

#[test]
fn golden_txlog_wire_encoding() {
    let mut header = vec![0];
    header.extend_from_slice(&1u32.to_le_bytes());
    header.extend_from_slice(&7u32.to_le_bytes());
    assert_eq!(
        TxEntry::Header(TxHeader {
            version: 1,
            locktime: 7
        })
        .to_cell()
        .unwrap()
        .encode(),
        record(&header, &[])
    );
    for (entry, tag, data) in [
        (TxEntry::Input([0x11; 32]), 2, [0x11; 32]),
        (TxEntry::Receive([0x22; 32]), 3, [0x22; 32]),
        (TxEntry::CellWitness([0x33; 32]), 15, [0x33; 32]),
        (
            TxEntry::ActorDestroy {
                actor: ActorID::Hash([7; 32]),
            },
            13,
            [7; 32],
        ),
    ] {
        assert_eq!(
            entry.to_cell().unwrap().encode(),
            record(&[&[tag][..], &data].concat(), &[])
        );
    }
    for (entry, tag, first, second) in [
        (
            TxEntry::IssuePriv(
                CompressedRistretto([0x33; 32]),
                CompressedRistretto([0x44; 32]),
            ),
            6,
            [0x33; 32],
            [0x44; 32],
        ),
        (
            TxEntry::Retire(
                CompressedRistretto([0x55; 32]),
                CompressedRistretto([0x66; 32]),
            ),
            7,
            [0x55; 32],
            [0x66; 32],
        ),
    ] {
        assert_eq!(
            entry.to_cell().unwrap().encode(),
            record(&[&[tag][..], &first, &second].concat(), &[])
        );
    }
    let issue = TxEntry::IssuePub(Scalar::from(5u64), Scalar::from(-3i64));
    assert_eq!(
        issue.to_cell().unwrap().payload(),
        [
            &[5][..],
            Scalar::from(5u64).as_bytes(),
            Scalar::from(-3i64).as_bytes()
        ]
        .concat()
    );
    assert_eq!(
        TxEntry::Fee(1000).to_cell().unwrap().encode(),
        record(&[&[8][..], &1000u64.to_le_bytes()].concat(), &[])
    );

    let actor = ActorID::Hash([7; 32]);
    for (entry, tag, child) in [
        (TxEntry::Data(vec![1, 2, 3]), 1, code_root(&[1, 2, 3])),
        (
            TxEntry::ActorSave {
                actor: actor.clone(),
                state: Value::Scalar(Scalar::from(42u64)),
            },
            9,
            state_root(&Value::Scalar(Scalar::from(42u64))),
        ),
        (
            TxEntry::SetCode {
                actor: actor.clone(),
                code: vec![0x1d],
            },
            10,
            code_root(&[0x1d]),
        ),
        (
            TxEntry::ActorDeploy {
                actor: actor.clone(),
                code: vec![0x1d],
            },
            14,
            code_root(&[0x1d]),
        ),
    ] {
        let payload = if tag == 1 {
            vec![tag]
        } else {
            [&[tag][..], &[7; 32]].concat()
        };
        assert_eq!(
            entry.to_cell().unwrap().encode(),
            record(&payload, &[child])
        );
    }
    let contract = fixture_contract();
    assert_eq!(
        TxEntry::Output(contract.clone())
            .to_cell()
            .unwrap()
            .encode(),
        record(&[4], &[contract.id()])
    );
    let message = dummy_message(50);
    assert_eq!(
        TxEntry::Send(message.clone()).to_cell().unwrap().encode(),
        record(&[11], &[message.to_cell().unwrap().id()])
    );
}

#[test]
fn golden_storage_effect_wire_encoding() {
    let purchase = TxEntry::StoragePurchase {
        actor: ActorID::Hash([7; 32]),
        bytes: 1024,
        expiry_height: 52_500,
        fee_sparks: Scalar::from(1_000_007_630u64),
    };
    let payload = [
        &[12][..],
        &[7; 32],
        &1024u64.to_le_bytes(),
        &52_500u64.to_le_bytes(),
        Scalar::from(1_000_007_630u64).as_bytes(),
    ]
    .concat();
    assert_eq!(purchase.to_cell().unwrap().encode(), record(&payload, &[]));
}
