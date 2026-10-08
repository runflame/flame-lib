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
            "bbdc11ab33c50d53c003b29031345bea4082ad79150e32c854c1f7224f3333c9",
        ),
        (
            vec![header(), TxEntry::Data(vec![1, 2, 3])],
            "984221dd9f0f7790f9d930310e082b87169cba1ca9946ce695850925c49b0d98",
        ),
        (
            vec![header(), TxEntry::Fee(1000)],
            "73b3bf8b22b057d6dacd6644300fa86d52f5a7da4f635f88f9b1a7300ef744e9",
        ),
        (
            vec![
                header(),
                TxEntry::ActorSave {
                    actor: actor.clone(),
                    state: Value::Scalar(Scalar::from(42u64)),
                },
            ],
            "aecd677b8e5e59c3e5dc5bbe3ab0133c9d4ccc000291eb3730c3f15374fb55f8",
        ),
        (
            vec![
                header(),
                TxEntry::SetCode {
                    actor: actor.clone(),
                    code: vec![0x1d],
                },
            ],
            "9960bca42b13cba109408f33c4e43040e8ea95a0653d56d97b9235bbfc76b8f7",
        ),
        (
            vec![
                header(),
                TxEntry::ActorDeploy {
                    actor,
                    code: vec![0x1d],
                },
            ],
            "0ad321a9edfe0f1ac5ba625f879636f47f2d4ef981984a1dcaae0dc5dc87b50a",
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
        "228e117e651446f12d17e89a542ac6b5af34a88c3cc1fd1e496dfdd94b0cabcf"
    );
    assert_eq!(
        hex(&code_root(&[0x1d])),
        "0eb1b652895c5e73d79a53885b13cbe62ca1c79d4152a5ddd4b59bc798076fca"
    );
    assert_eq!(
        hex(&ActorID::Constructor(vec![1, 2, 3]).to_hash()),
        "e461d3fc97418e23296c184677afb7db33e77234c3bdaf1e8d09aa5c6d28cc34"
    );
    let contract = Contract::new(
        Predicate::opaque(CompressedRistretto([0x77; 32])),
        Anchor([0x88; 32]),
        Value::Scalar(Scalar::from(9u64)),
    )
    .unwrap();
    assert_eq!(
        hex(&contract.id()),
        "64627d3704e75e34c296d2718ccb37724cbaf979c766fa06128cb9d717f09ba9"
    );
    assert_eq!(
        hex(TxID::from_log(&[
            header(),
            TxEntry::Input(contract.id()),
            TxEntry::Output(contract)
        ])
        .as_bytes()),
        "5412d3b07ac33d69caddde19f517fe5cb32772b26348fa6328e81c9af3e50827"
    );
    assert_ne!(
        TxID::from_log(&[header(), TxEntry::Data(vec![1]), TxEntry::Fee(7)]),
        TxID::from_log(&[header(), TxEntry::Fee(7), TxEntry::Data(vec![1])])
    );
}

/// A raw record writer independent of CellBuilder and all type codecs.
fn record(payload: &[u8], refs: &[([u8; 32], u16)]) -> Vec<u8> {
    let descriptor = payload.len() as u16 | ((refs.len() as u16) << 12);
    let mut bytes = descriptor.to_le_bytes().to_vec();
    bytes.extend_from_slice(payload);
    for (hash, depth) in refs {
        bytes.extend_from_slice(&0u16.to_le_bytes()); // ordinary level mask
        bytes.extend_from_slice(hash);
        bytes.extend_from_slice(&depth.to_le_bytes());
    }
    bytes
}

/// Independent physical-depth calculation for these fully resident, level-zero fixtures.
fn depth(cell: &Cell) -> u16 {
    cell.refs()
        .iter()
        .map(|child| depth(child.as_resident().unwrap()) + 1)
        .max()
        .unwrap_or(0)
}

#[test]
fn golden_string_is_raw_bytes_in_one_cell() {
    let string = String::from(b"abc".to_vec());
    let cell = string.to_cell().unwrap();
    assert_eq!(cell.encode(), record(b"abc", &[]));
    assert_eq!(hex(&cell.encode()), "0300616263");
    assert_eq!(
        hex(&cell.id()),
        "3da9865b43fa2ec490f78da9db16acd5638704dbce5cc7b3df2e3c7a23addf19"
    );
    let value = Value::String(string).to_cell().unwrap();
    assert_eq!(value.encode(), record(&[1], &[(cell.id(), 0)]));
    assert_eq!(
        hex(&value.id()),
        "9148045685ff8f24d5a306e327717912730bd3a741306ab3affc59d61c55db93"
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
            record(&payload, &[(child, 0)])
        );
    }
    let contract = fixture_contract();
    assert_eq!(
        TxEntry::Output(contract.clone())
            .to_cell()
            .unwrap()
            .encode(),
        record(
            &[4],
            &[(contract.id(), depth(&contract.to_cell().unwrap()))]
        )
    );
    let message = dummy_message(50);
    assert_eq!(
        TxEntry::Send(message.clone()).to_cell().unwrap().encode(),
        record(&[11], &[(message.to_cell().unwrap().id(), 1)])
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
