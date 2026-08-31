//! Tests for hashing.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Helper: makes a String from a byte slice (for `push_str` in tests).
fn s(bytes: &[u8]) -> String {
    String::from(bytes.to_vec())
}

#[test]
fn transcript_is_noncopyable_but_droppable() {
    // pushstr [], transcript, dup:0 → TypeNotCopyable (still linear
    // wrt stack duplication — has internal CS-style state).
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(b""))
            .transcript()
            .dup_k(0)
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));

    // pushstr [], transcript, drop → succeeds. Transcript carries no
    // asset value (pure computation), so dropping it is legal — see
    // `Value::is_droppable`.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(b""))
            .transcript()
            .drop_()
            .to_bytecode(),
    );
    run_to_end(&mut vm).expect("transcript is droppable (pure computation)");
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn twrite_requires_merlin_on_bottom() {
    // push:5 (wrong type), pushstr "lbl", pushstr "data", merlinwrite
    let script = ScriptBuilder::new()
        .push_int(5u64)
        .push_str(s(b"lbl"))
        .push_str(s(b"data"))
        .twrite()
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
        ScriptBuilder::new()
            .push_str(s(b"abc"))
            .sha3()
            .to_bytecode(),
    );
    run_to_end(&mut a).unwrap();
    let mut b = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(b"abc"))
            .keccak256()
            .to_bytecode(),
    );
    run_to_end(&mut b).unwrap();
    match (&a.current_call.stack[0], &b.current_call.stack[0]) {
        (Value::String(sa), Value::String(sb)) => {
            assert_ne!(sa.as_opaque().unwrap(), sb.as_opaque().unwrap());
        }
        _ => panic!(),
    }
}

#[test]
fn calibrated_hash_cost_tracks_compression_blocks() {
    assert_eq!(hash_gas(0, 64).unwrap(), GAS_HASH_BASE + GAS_HASH_BLOCK);
    assert_eq!(hash_gas(63, 64).unwrap(), GAS_HASH_BASE + GAS_HASH_BLOCK);
    assert_eq!(
        hash_gas(64, 64).unwrap(),
        GAS_HASH_BASE + 2 * GAS_HASH_BLOCK,
    );
    assert!(matches!(hash_gas(1, 0), Err(VMError::OutOfGas)));
}

#[test]
fn sha256_debits_the_calibrated_work_before_hashing() {
    let mut vm = vm_with_script(ScriptBuilder::new().sha256().to_bytecode());
    vm.current_call.stack = vec![Value::String(String::from(vec![0x5a; 64]))];
    vm.current_call.gas_limit = GAS_PER_INSTRUCTION + hash_gas(64, 64).unwrap();
    assert!(vm.step_internal().expect("exact budget succeeds"));
    assert_eq!(vm.current_call.gas_used, vm.current_call.gas_limit);

    let mut short = vm_with_script(ScriptBuilder::new().sha256().to_bytecode());
    short.current_call.stack = vec![Value::String(String::from(vec![0x5a; 64]))];
    short.current_call.gas_limit = GAS_PER_INSTRUCTION + hash_gas(64, 64).unwrap() - 1;
    assert!(matches!(short.step_internal(), Err(VMError::OutOfGas)));
}
