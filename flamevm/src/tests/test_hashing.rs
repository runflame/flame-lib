//! Tests for hashing.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Helper: makes a String from a byte slice (for `push_str` in tests).
fn s(bytes: &[u8]) -> String { String::from(bytes.to_vec()) }

#[test]
fn transcript_creates_merlin() {
    // pushstr [], merlin → Merlin on top
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).transcript().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Merlin(_) => {}
        other => panic!("expected Merlin, got {}", value_kind(other)),
    }
}

#[test]
fn transcript_is_noncopyable_and_nondroppable() {
    // pushstr [], merlin, dup:0 → TypeNotCopyable
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).transcript().dup_k(0).to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));

    // pushstr [], merlin, drop → TypeNotDroppable
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).transcript().drop_().to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotDroppable
    ));
}

#[test]
fn transcript_write_then_read_produces_bytes() {
    // pushstr "init", merlin            -- transcript
    // pushstr "lbl", pushstr "data", merlinwrite  -- absorb data
    // pushstr "rd", push:8, merlinread  -- squeeze 8 bytes
    let script = Program::new()
        .push_str(s(b"init")).transcript()
        .push_str(s(b"lbl")).push_str(s(b"data")).twrite()
        .push_str(s(b"rd")).push_int(8u64).tread()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    // Stack: [Merlin, String(8 bytes)]
    assert_eq!(vm.current_call.stack.len(), 2);
    match &vm.current_call.stack[0] {
        Value::Merlin(_) => {}
        other => panic!("expected Merlin, got {}", value_kind(other)),
    }
    match &vm.current_call.stack[1] {
        Value::String(s) => assert_eq!(s.len(), 8),
        other => panic!("expected String, got {}", value_kind(other)),
    }
}

#[test]
fn tread_is_deterministic() {
    // Two scripts that absorb the same input should produce the same
    // challenge bytes.
    fn build_script() -> Vec<u8> {
        Program::new()
            .push_str(s(b"label")).transcript()
            .push_str(s(b"k")).push_str(s(b"value")).twrite()
            .push_str(s(b"r")).push_int(16u64).tread()
            .to_bytecode()
    }
    let mut a = vm_with_script(build_script());
    run_to_end(&mut a).unwrap();
    let mut b = vm_with_script(build_script());
    run_to_end(&mut b).unwrap();
    let bytes_a = match &a.current_call.stack[1] {
        Value::String(s) => s.as_bytes().to_vec(),
        _ => panic!("expected String"),
    };
    let bytes_b = match &b.current_call.stack[1] {
        Value::String(s) => s.as_bytes().to_vec(),
        _ => panic!("expected String"),
    };
    assert_eq!(bytes_a, bytes_b);
}

#[test]
fn tread_diverges_on_different_label() {
    // Same data, different label → different challenge bytes.
    fn build(label: &[u8]) -> Vec<u8> {
        Program::new()
            .push_str(s(b"l")).transcript()
            .push_str(s(b"k")).push_str(s(b"data")).twrite()
            .push_str(s(label)).push_int(16u64).tread()
            .to_bytecode()
    }
    let mut a = vm_with_script(build(b"A"));
    let mut b = vm_with_script(build(b"B"));
    run_to_end(&mut a).unwrap();
    run_to_end(&mut b).unwrap();
    let ba = match &a.current_call.stack[1] {
        Value::String(s) => s.as_bytes().to_vec(),
        _ => panic!(),
    };
    let bb = match &b.current_call.stack[1] {
        Value::String(s) => s.as_bytes().to_vec(),
        _ => panic!(),
    };
    assert_ne!(ba, bb);
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
fn sha256_empty() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).sha256().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn sha256_abc() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"abc")).sha256().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn sha512_empty() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).sha512().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
         47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn sha3_empty() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).sha3().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn sha3_abc() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"abc")).sha3().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn keccak256_empty() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"")).keccak256().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn keccak256_abc() {
    let mut vm = vm_with_script(
        Program::new().push_str(s(b"abc")).keccak256().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45",
    );
    assert_str(&vm.current_call.stack[0], &expected);
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
            assert_ne!(sa.as_bytes(), sb.as_bytes());
        }
        _ => panic!(),
    }
}
