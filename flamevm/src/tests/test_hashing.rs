//! Tests for hashing.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── Phase 6 ──────────────────────────────────────────────────

// ── merlin (0x69) ────────────────────────────────────────────

#[test]
fn merlin_creates_transcript() {
    // pushstr [], merlin → Merlin on top
    let mut script = pushstr_bytes(&[]);
    script.push(0x69);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Merlin(_) => {}
        other => panic!("expected Merlin, got {}", value_kind(other)),
    }
}

#[test]
fn merlin_is_noncopyable_and_nondroppable() {
    // pushstr [], merlin, dup:0 → TypeNotCopyable
    let mut script = pushstr_bytes(&[]);
    script.push(0x69);
    script.push(0x20);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));

    // pushstr [], merlin, drop → TypeNotDroppable
    let mut script = pushstr_bytes(&[]);
    script.push(0x69);
    script.push(0x1c);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotDroppable
    ));
}

// ── merlinwrite / merlinread (0x6a, 0x6b) ────────────────────

#[test]
fn merlin_write_then_read_produces_bytes() {
    // pushstr "init", merlin            -- transcript
    // pushstr "lbl", pushstr "data", merlinwrite  -- absorb data
    // pushstr "rd", push:8, merlinread  -- squeeze 8 bytes
    let mut script = pushstr_bytes(b"init");
    script.push(0x69);
    script.extend_from_slice(&pushstr_bytes(b"lbl"));
    script.extend_from_slice(&pushstr_bytes(b"data"));
    script.push(0x6a);
    script.extend_from_slice(&pushstr_bytes(b"rd"));
    script.push(0x08); // push:8
    script.push(0x6b);
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
fn merlin_read_is_deterministic() {
    // Two scripts that absorb the same input should produce the same
    // challenge bytes.
    fn build_script() -> Vec<u8> {
        let mut script = pushstr_bytes(b"label");
        script.push(0x69);
        script.extend_from_slice(&pushstr_bytes(b"k"));
        script.extend_from_slice(&pushstr_bytes(b"value"));
        script.push(0x6a);
        script.extend_from_slice(&pushstr_bytes(b"r"));
        script.push(0x10); // pushint8
        script.push(16);
        script.push(0x6b);
        script
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
fn merlin_read_diverges_on_different_label() {
    // Same data, different label → different challenge bytes.
    fn build(label: &[u8]) -> Vec<u8> {
        let mut script = pushstr_bytes(b"l");
        script.push(0x69);
        script.extend_from_slice(&pushstr_bytes(b"k"));
        script.extend_from_slice(&pushstr_bytes(b"data"));
        script.push(0x6a);
        script.extend_from_slice(&pushstr_bytes(label));
        script.push(0x10);
        script.push(16);
        script.push(0x6b);
        script
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
fn merlin_write_requires_merlin_on_bottom() {
    // push:5 (wrong type), pushstr "lbl", pushstr "data", merlinwrite
    let mut script = vec![0x05];
    script.extend_from_slice(&pushstr_bytes(b"lbl"));
    script.extend_from_slice(&pushstr_bytes(b"data"));
    script.push(0x6a);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotMerlin
    ));
}

// ── sha256 (0x6c) ────────────────────────────────────────────

#[test]
fn sha256_empty() {
    let mut script = pushstr_bytes(b"");
    script.push(0x6c);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn sha256_abc() {
    let mut script = pushstr_bytes(b"abc");
    script.push(0x6c);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

// ── sha512 (0x6d) ────────────────────────────────────────────

#[test]
fn sha512_empty() {
    let mut script = pushstr_bytes(b"");
    script.push(0x6d);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
         47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

// ── sha3 (0x6e) ──────────────────────────────────────────────

#[test]
fn sha3_empty() {
    let mut script = pushstr_bytes(b"");
    script.push(0x6e);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn sha3_abc() {
    let mut script = pushstr_bytes(b"abc");
    script.push(0x6e);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

// ── keccak256 (0x4e) ─────────────────────────────────────────

#[test]
fn keccak256_empty() {
    let mut script = pushstr_bytes(b"");
    script.push(0x4e);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = hex_to_bytes(
        "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
    );
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn keccak256_abc() {
    let mut script = pushstr_bytes(b"abc");
    script.push(0x4e);
    let mut vm = vm_with_script(script);
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
    let mut script_a = pushstr_bytes(b"abc");
    script_a.push(0x6e); // sha3
    let mut script_b = pushstr_bytes(b"abc");
    script_b.push(0x4e); // keccak256
    let mut a = vm_with_script(script_a);
    run_to_end(&mut a).unwrap();
    let mut b = vm_with_script(script_b);
    run_to_end(&mut b).unwrap();
    match (&a.current_call.stack[0], &b.current_call.stack[0]) {
        (Value::String(sa), Value::String(sb)) => {
            assert_ne!(sa.as_bytes(), sb.as_bytes());
        }
        _ => panic!(),
    }
}

