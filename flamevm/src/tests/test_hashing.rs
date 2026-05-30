//! Tests for hashing.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Helper: makes a String from a byte slice (for `push_str` in tests).
fn s(bytes: &[u8]) -> String { String::from(bytes.to_vec()) }

#[test]
fn transcript_is_noncopyable_but_droppable() {
    // pushstr [], transcript, dup:0 → TypeNotCopyable (still linear
    // wrt stack duplication — has internal CS-style state).
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).transcript().dup_k(0).to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));

    // pushstr [], transcript, drop → succeeds. Transcript carries no
    // asset value (pure computation), so dropping it is legal — see
    // `Value::is_droppable`.
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).transcript().drop_().to_bytecode(),
    );
    run_to_end(&mut vm).expect("transcript is droppable (pure computation)");
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn twrite_requires_merlin_on_bottom() {
    // push:5 (wrong type), pushstr "lbl", pushstr "data", merlinwrite
    let script = Program::new()
        .push_int(5u64)
        .push_str(s(b"lbl")).push_str(s(b"data")).twrite()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotMerlin
    ));
}

#[test]
fn keccak256_differs_from_sha3() {
    // SHA3-256 and Keccak-256 of the same input must differ
    // (FIPS-202 added a domain separator).
    let mut a = vm_with_script(
        Program::new().push_str(s(b"abc")).sha3().to_bytecode(),
    );
    run_to_end(&mut a).unwrap();
    let mut b = vm_with_script(
        Program::new().push_str(s(b"abc")).keccak256().to_bytecode(),
    );
    run_to_end(&mut b).unwrap();
    match (&a.current_call.stack[0], &b.current_call.stack[0]) {
        (Value::String(sa), Value::String(sb)) => {
            assert_ne!(sa.as_opaque().unwrap(), sb.as_opaque().unwrap());
        }
        _ => panic!(),
    }
}
