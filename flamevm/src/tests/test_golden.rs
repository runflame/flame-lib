//! Absolute golden vectors for consensus-committed hashes. Unlike the
//! relative TxID tests (which only compare two computed roots), these
//! pin frozen outputs, so a uniform shift — domain-label rename, field
//! reorder in `Message::encode`, LE/BE flip — that moves every leaf
//! identically is caught here (it would silently fork the chain against
//! any other implementation). Audit p4.
//!
//! Regenerate (only on a deliberate consensus change) by flipping
//! `REGEN` to true, running with `--nocapture`, and pasting the printed
//! values back into the asserts.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::tx::{TxEntry, TxHeader, TxID, TxLog};
use crate::{code_root, state_root};

const REGEN: bool = false;

fn h(version: u32, locktime: u32) -> TxHeader {
    TxHeader { version, locktime }
}

#[test]
fn golden_consensus_hashes() {
    let actor = ActorID::Hash([0x07; 32]);

    let empty = format!("{:?}", TxID::from_log(&[TxEntry::Header(h(1, 0))]));
    let data = format!(
        "{:?}",
        TxID::from_log(&[TxEntry::Header(h(1, 0)), TxEntry::Data(vec![1, 2, 3])])
    );
    let fee = format!(
        "{:?}",
        TxID::from_log(&[TxEntry::Header(h(1, 0)), TxEntry::Fee(1000)])
    );
    let save = format!(
        "{:?}",
        TxID::from_log(&[
            TxEntry::Header(h(1, 0)),
            TxEntry::ActorSave { actor: actor.clone(), state: Value::Int253(Int253::from(42u64)) },
        ])
    );
    let setcode = format!(
        "{:?}",
        TxID::from_log(&[
            TxEntry::Header(h(1, 0)),
            TxEntry::SetCode { actor: actor.clone(), code: vec![0x1d] },
        ])
    );
    let state_root = format!("{:?}", state_root(&Value::Int253(Int253::from(42u64))));
    let code_root = format!("{:?}", code_root(&[0x1d]));
    let ctor_id = format!("{:?}", ActorID::Constructor(vec![1, 2, 3]).to_hash());

    if REGEN {
        for (k, v) in [
            ("empty", &empty), ("data", &data), ("fee", &fee), ("save", &save),
            ("setcode", &setcode), ("state_root", &state_root),
            ("code_root", &code_root), ("ctor_id", &ctor_id),
        ] {
            eprintln!("GOLDEN {k} = {v}");
        }
        panic!("REGEN — paste values into asserts, set REGEN=false");
    }

    assert_eq!(empty, "TxID(Hash(78dc804f3642b6109767b19068165ba37a437071f0c07ea70103c27e6cf02d3a))");
    assert_eq!(data, "TxID(Hash(fcaeb40b2b33204c7ce80dd6082aa90ce6b2e67822143cc8e7338adda1c65e30))");
    assert_eq!(fee, "TxID(Hash(929da748969f08c76bbe87b4df3e7f97c90a875e352eb1f3960c8dddf0568e40))");
    assert_eq!(save, "TxID(Hash(c16bcabda54dfb5f7d63b81a452bc1a14d8e13d15cedfb40b9825d2836cf30d7))");
    assert_eq!(setcode, "TxID(Hash(6cc0bb6da93f45f4015000ce5e6f4054a2b341024a0363516508f4531e81346f))");
    assert_eq!(state_root, "[55, 153, 101, 152, 10, 104, 94, 233, 140, 125, 135, 90, 162, 101, 75, 190, 192, 18, 163, 108, 189, 73, 155, 248, 201, 51, 241, 232, 8, 129, 177, 182]");
    assert_eq!(code_root, "[49, 139, 70, 84, 16, 230, 41, 133, 96, 138, 164, 96, 140, 192, 168, 177, 16, 219, 77, 211, 98, 38, 102, 149, 123, 241, 218, 249, 204, 253, 100, 198]");
    assert_eq!(ctor_id, "[78, 236, 146, 161, 207, 157, 64, 230, 4, 244, 221, 152, 140, 235, 119, 206, 6, 194, 76, 78, 145, 123, 221, 5, 115, 126, 193, 163, 197, 207, 76, 100]");

    // Structural: header-only ≠ single-effect; effect order is committed.
    assert_ne!(empty, data);
    let ab = format!("{:?}", TxID::from_log(&[TxEntry::Header(h(1, 0)), TxEntry::Data(vec![1]), TxEntry::Fee(7)]));
    let ba = format!("{:?}", TxID::from_log(&[TxEntry::Header(h(1, 0)), TxEntry::Fee(7), TxEntry::Data(vec![1])]));
    assert_ne!(ab, ba, "effect order is committed");
}

/// Golden wire bytes for the canonical TxLog/TxEntry encoding
/// (encode-only; spec §TxLog transport). Pins tag values, field order,
/// and LE widths — any serializer drift shows up as a diff here.
#[test]
fn golden_txlog_wire_encoding() {
    use curve25519_dalek::ristretto::CompressedRistretto as CR;
    use readerwriter::Encodable;
    let actor = ActorID::Hash([0x07; 32]);
    let log = TxLog::from(vec![
        TxEntry::Header(h(1, 7)),
        TxEntry::Data(vec![0xab, 0xcd]),
        TxEntry::Input([0x11; 32]),
        TxEntry::Receive([0x22; 32]),
        TxEntry::IssuePub(Int253::from(5u64), Int253::from(-3i64)),
        TxEntry::IssuePriv(CR([0x33; 32]), CR([0x44; 32])),
        TxEntry::Retire(CR([0x55; 32]), CR([0x66; 32])),
        TxEntry::Fee(1_000),
        TxEntry::ActorSave { actor: actor.clone(), state: Value::Int253(Int253::from(42u64)) },
        TxEntry::SetCode { actor: actor.clone(), code: vec![0x1d] },
        TxEntry::Output(Cell::new(
            Predicate::opaque(CR([0x77; 32])),
            Anchor([0x88; 32]),
            vec![Value::Int253(Int253::from(9u64))],
        )),
        TxEntry::Send(Message {
            target: actor,
            caller: None,
            anchor: Anchor([0x99; 32]),
            payload: Vec::new(),
            gas: 50,
            vbytes: 60,
            refund_predicate: Predicate::opaque(CR([0xaa; 32])),
        }),
    ]);
    let wire = log.encode_to_vec();
    let hex: std::string::String = wire.iter().map(|b| format!("{b:02x}")).collect();
    if REGEN {
        eprintln!("GOLDEN wire = {hex}");
        panic!("REGEN — paste wire hex");
    }
    assert_eq!(hex, "0c00000000000000000100000007000000010200000000000000abcd021111111111111111111111111111111111111111111111111111111111111111032222222222222222222222222222222222222222222222222222222222222222050540010633333333333333333333333333333333333333333333333333333333333333334444444444444444444444444444444444444444444444444444444444444444075555555555555555555555555555555555555555555555555555555555555555666666666666666666666666666666666666666666666666666666666666666608e803000000000000090007070707070707070707070707070707070707070707070707070707070707072a0a00070707070707070707070707070707070707070707070707070707070707070701000000000000001d0483f8777777777777777777777777777777777777777777777777777777777777777764888888888888888888888888888888888888888888888888888888888888888881090b999999999999999999999999999999999999999999999999999999999999999900070707070707070707070707070707070707070707070707070707070707070700aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa32000000000000003c000000000000000000000000000000");
}
