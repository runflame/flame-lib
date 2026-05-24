//! Tests for stack.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── push:k (0x00..=0x0f) ─────────────────────────────────────

#[test]
fn push_immediate_k_roundtrips_0_to_15() {
    for k in 0..=15u8 {
        let mut vm = vm_with_script(vec![k]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 1, "k={}", k);
        assert_int(&vm.current_call.stack[0], Int253::from(k as u64));
    }
}

// ── pushint{8,16,64,128,full} (0x10..=0x18) ──────────────────

#[test]
fn pushint8_positive_and_negative() {
    let mut pos = vm_with_script(vec![0x10, 42]);
    run_to_end(&mut pos).unwrap();
    assert_int(&pos.current_call.stack[0], Int253::from(42u64));

    let mut neg = vm_with_script(vec![0x11, 42]);
    run_to_end(&mut neg).unwrap();
    assert_int(&neg.current_call.stack[0], Int253::from(-42i64));
}

#[test]
fn pushint16_le_decoding() {
    // 0x12 = positive; bytes 0x02 0x01 LE = 258
    let mut vm = vm_with_script(vec![0x12, 0x02, 0x01]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(258u64));
}

#[test]
fn pushint64_le_decoding() {
    let val: u64 = 0x0102_0304_0506_0708;
    let mut script = vec![0x14];
    script.extend_from_slice(&val.to_le_bytes());
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(val));
}

#[test]
fn pushint128_le_decoding() {
    let val: u128 = 0xFEED_FACE_DEAD_BEEF_CAFE_BABE_BADD_CAFEu128;
    let mut script = vec![0x16];
    script.extend_from_slice(&val.to_le_bytes());
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&val.to_le_bytes());
    let expected =
        Int253::from_parts(false, Scalar::from_canonical_bytes(bytes).unwrap());
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn pushint_full_roundtrip() {
    let expected = Int253::from(-1234567i64);
    let mut script = vec![0x18];
    script.extend_from_slice(&expected.to_bytes());
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn pushint_full_rejects_negative_zero() {
    // sign bit set, magnitude zero: -0, not representable
    let mut bytes = [0u8; 32];
    bytes[31] = 0x80;
    let mut script = vec![0x18];
    script.extend_from_slice(&bytes);
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::InvalidInt253Encoding));
}

#[test]
fn pushint8_at_end_of_script_errors() {
    let mut vm = vm_with_script(vec![0x10]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::UnexpectedEndOfScript
    ));
}

// ── pushstr (0x19) ───────────────────────────────────────────

#[test]
fn pushstr_immediate_length() {
    // sub-varint tag 0, then byte 4 = length 4, then 4 bytes
    let script = vec![0x19, 0x00, 0x04, b'a', b'b', b'c', b'd'];
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::String(s) => assert_eq!(s.as_bytes(), b"abcd"),
        other => panic!("expected String, got {}", value_kind(other)),
    }
}

#[test]
fn pushstr_short_input_errors() {
    let script = vec![0x19, 0x00, 0x04, b'a']; // length says 4, only 1 byte
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::UnexpectedEndOfScript
    ));
}

// ── pushpoint (0x1a) ─────────────────────────────────────────

#[test]
fn pushpoint_roundtrip() {
    let bytes = [0x42u8; 32];
    let mut script = vec![0x1a];
    script.extend_from_slice(&bytes);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Point(p) => assert_eq!(p.as_bytes(), &bytes),
        other => panic!("expected Point, got {}", value_kind(other)),
    }
}

// ── pushtoken (0x1b) ─────────────────────────────────────────

#[test]
fn pushtoken_zero_qty_with_flavor() {
    // push:7, pushtoken — flavor comes from the stack now.
    let mut vm = vm_with_script(vec![0x07, 0x1b]);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => {
            assert!(t.is_zero_qty());
            assert_eq!(t.flv(), Int253::from(7u64));
        }
        other => panic!("expected ClearToken, got {}", value_kind(other)),
    }
}

#[test]
fn pushtoken_requires_int_flavor() {
    // pushstr "x", pushtoken — top is String, not Int253.
    let mut script = pushstr_bytes(b"x");
    script.push(0x1b);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

#[test]
fn pushtoken_full_flavor_via_pushint_full() {
    // pushint full <bytes>, pushtoken — exercises a non-small flavor.
    let flv = Int253::from(0x1234567890abcdefu64);
    let mut script = vec![0x18];
    script.extend_from_slice(&flv.to_bytes());
    script.push(0x1b);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => {
            assert!(t.is_zero_qty());
            assert_eq!(t.flv(), flv);
        }
        _ => panic!("expected ClearToken"),
    }
}

// ── drop (0x1c) ──────────────────────────────────────────────

#[test]
fn drop_droppable_int() {
    let mut vm = vm_with_script(vec![0x05, 0x1c]); // push:5, drop
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn drop_underflow_errors() {
    let mut vm = vm_with_script(vec![0x1c]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}

// ── dup / dup:k (0x1e, 0x20..=0x2f) ──────────────────────────

#[test]
fn dup_immediate_zero_copies_top() {
    let mut vm = vm_with_script(vec![0x07, 0x20]); // push:7, dup:0
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    assert_int(&vm.current_call.stack[1], Int253::from(7u64));
}

#[test]
fn dup_immediate_k_picks_kth_from_top() {
    // push:1, push:2, push:3, dup:2 → 1 2 3 1
    let mut vm = vm_with_script(vec![0x01, 0x02, 0x03, 0x22]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 4);
    assert_int(&vm.current_call.stack[3], Int253::from(1u64));
}

#[test]
fn dup_dynamic_pops_index() {
    // push:9, push:8, push:0, dup → 9 8 (k=0) → 9 8 8
    let mut vm = vm_with_script(vec![0x09, 0x08, 0x00, 0x1e]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Int253::from(8u64));
}

#[test]
fn dup_out_of_range_errors() {
    // push:5, dup:5 (only 1 item on stack)
    let mut vm = vm_with_script(vec![0x05, 0x25]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn dup_noncopyable_errors() {
    // push:1, pushtoken (linear), dup:0
    let mut vm = vm_with_script(vec![0x01, 0x1b, 0x20]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

// ── roll / roll:k (0x1f, 0x30..=0x3f) ────────────────────────

#[test]
fn roll_immediate_moves_kth_to_top() {
    // push:1, push:2, push:3, roll:2 → 2 3 1
    let mut vm = vm_with_script(vec![0x01, 0x02, 0x03, 0x32]);
    run_to_end(&mut vm).unwrap();
    let stack = &vm.current_call.stack;
    assert_int(&stack[0], Int253::from(2u64));
    assert_int(&stack[1], Int253::from(3u64));
    assert_int(&stack[2], Int253::from(1u64));
}

#[test]
fn roll_zero_is_noop() {
    let mut vm = vm_with_script(vec![0x07, 0x30]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
}

#[test]
fn roll_dynamic_pops_index() {
    // push:1, push:2, push:1, roll  → roll k=1 → stack {1,2} -> {2,1}
    let mut vm = vm_with_script(vec![0x01, 0x02, 0x01, 0x1f]);
    run_to_end(&mut vm).unwrap();
    let stack = &vm.current_call.stack;
    assert_int(&stack[0], Int253::from(2u64));
    assert_int(&stack[1], Int253::from(1u64));
}

#[test]
fn roll_out_of_range_errors() {
    let mut vm = vm_with_script(vec![0x05, 0x35]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn dup_of_copyable_dict_succeeds() {
    // {5: 50}, dup:0 — should copy the dict.
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x01, 0x60, // dict
        0x20,                       // dup:0
    ]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    // Both stack entries should be dicts of length 1.
    for v in &vm.current_call.stack {
        match v {
            Value::Dict(d) => assert_eq!(d.len(), 1),
            _ => panic!("expected Dict"),
        }
    }
}

#[test]
fn dup_of_noncopyable_dict_errors() {
    // push:7, pushtoken, push:5, push:1, dict, dup:0
    let mut vm = vm_with_script(vec![0x07, 0x1b, 0x05, 0x01, 0x60, 0x20]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

