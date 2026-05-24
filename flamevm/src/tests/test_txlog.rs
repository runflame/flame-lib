//! Tests for txlog.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── TxID + log opcode ────────────────────

#[test]
fn log_opcode_emits_txentry_data() {
    // pushstr "hello", log → txlog has Header + TxEntry::Data(b"hello").
    let mut script = pushstr_bytes(b"hello");
    script.push(0x6f); // log
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).expect("log ok");
    assert!(vm.current_call.stack.is_empty());
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Data(bytes) => assert_eq!(bytes, b"hello"),
        _ => panic!("expected Data entry"),
    }
}

#[test]
fn log_opcode_requires_string() {
    // push:5, log — top is Int253 not String.
    let mut vm = vm_with_script(vec![0x05, 0x6f]);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TypeNotString));
}

#[test]
fn txid_from_log_is_deterministic_and_distinguishes_entries() {
    use crate::tx::{TxEntry, TxID};
    let log1 = vec![TxEntry::Data(b"hello".to_vec())];
    let log2 = vec![TxEntry::Data(b"hello".to_vec())];
    let log3 = vec![TxEntry::Data(b"world".to_vec())];
    let id1 = TxID::from_log(&log1);
    let id2 = TxID::from_log(&log2);
    let id3 = TxID::from_log(&log3);
    // Determinism.
    assert_eq!(id1.0, id2.0);
    // Different payload → different TxID.
    assert_ne!(id1.0, id3.0);
}

#[test]
fn txid_distinguishes_entry_order() {
    // Permuting entries must change the TxID (merkle order matters).
    use crate::tx::{TxEntry, TxID};
    let a = TxEntry::Data(b"a".to_vec());
    let b = TxEntry::Data(b"b".to_vec());
    let id_ab = TxID::from_log(&[a, b]);
    let a2 = TxEntry::Data(b"a".to_vec());
    let b2 = TxEntry::Data(b"b".to_vec());
    let id_ba = TxID::from_log(&[b2, a2]);
    assert_ne!(id_ab.0, id_ba.0);
}

#[test]
fn txid_distinguishes_input_from_output_entries() {
    use crate::tx::{TxEntry, TxID};
    // Two single-entry logs with identical 32-byte payload but
    // different variant tags should hash differently — confirms
    // the domain separation in `MerkleItem for TxEntry::commit`.
    let id_data = TxID::from_log(&[TxEntry::Data([0u8; 32].to_vec())]);
    let id_input = TxID::from_log(&[TxEntry::Input([0u8; 32])]);
    assert_ne!(id_data.0, id_input.0);
}

#[test]
fn instruction_log_roundtrip() {
    use crate::ops::Instruction;
    let mut buf = Vec::new();
    Instruction::Log.encode(&mut buf);
    assert_eq!(buf, vec![0x6f]);
    let mut r: &[u8] = &buf;
    assert!(matches!(
        Instruction::parse(&mut r).expect("parses"),
        Instruction::Log
    ));
}

