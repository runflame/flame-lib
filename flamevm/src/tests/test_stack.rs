//! Tests for stack.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Canonicality: a push-int value representable by a narrower width class
/// must use it (the encoder always picks narrowest). Decoder rejects the
/// non-minimal forms. See ADR/audit (decode canonicality).
#[test]
fn pushint_non_minimal_width_rejected() {
    // pushint8 pos 5 → should be push:5
    assert!(matches!(Program::parse(&[0x10, 0x05]).unwrap_err(), VMError::InvalidInt253Encoding));
    // pushint8 pos 0 → should be push:0
    assert!(matches!(Program::parse(&[0x10, 0x00]).unwrap_err(), VMError::InvalidInt253Encoding));
    // pushint16 pos 5 → fits push:k
    assert!(matches!(Program::parse(&[0x12, 0x05, 0x00]).unwrap_err(), VMError::InvalidInt253Encoding));
    // Minimal forms accepted: pushint8 neg 5 (no narrower negative), push:5.
    assert!(Program::parse(&[0x11, 0x05]).is_ok());
    assert!(Program::parse(&[0x05]).is_ok());
}

/// pushstr with a huge claimed length but no payload must fail-bounded,
/// not force a giant allocation (OOM-on-adversarial-input guard).
#[test]
fn pushstr_overlong_length_is_bounded_not_oom() {
    // pushstr, sub-varint U32 ≈ 4.3 GB, zero payload bytes.
    let script = vec![0x19, 0x02, 0xff, 0xff, 0xff, 0xff];
    assert!(matches!(Program::parse(&script).unwrap_err(), VMError::UnexpectedEndOfScript));
}

#[test]
fn pushint_full_rejects_negative_zero() {
    // sign bit set, magnitude zero: -0, not representable. Parse-time
    // failure now (Program::parse happens at VM entry, not lazily
    // during dispatch).
    let mut bytes = [0u8; 32];
    bytes[31] = 0x80;
    let mut script = vec![0x18];
    script.extend_from_slice(&bytes);
    let err = Program::parse(&script).unwrap_err();
    assert!(matches!(err, VMError::InvalidInt253Encoding));
}

#[test]
fn pushint8_at_end_of_script_errors() {
    // Bare `pushint8` opcode (0x10) with no immediate byte — fails
    // at parse time.
    let err = Program::parse(&[0x10]).unwrap_err();
    assert!(matches!(err, VMError::UnexpectedEndOfScript));
}

#[test]
fn pushstr_short_input_errors() {
    // length says 4, only 1 byte present — fails at parse time.
    let script = vec![0x19, 0x00, 0x04, b'a'];
    let err = Program::parse(&script).unwrap_err();
    assert!(matches!(err, VMError::UnexpectedEndOfScript));
}

#[test]
fn pushtoken_zero_qty_with_flavor() {
    // push:7, pushtoken — flavor comes from the stack now.
    let mut vm = vm_with_script(
        Program::new().push_int(7u64).pushtoken().to_bytecode(),
    );
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
    let mut vm = vm_with_script(
        Program::new()
            .push_str(String::from(b"x".to_vec()))
            .pushtoken()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

#[test]
fn pushtoken_full_flavor_via_pushint_full() {
    // push a non-small flavor (encoder picks the right pushint variant),
    // then pushtoken.
    let flv = Int253::from(0x1234567890abcdefu64);
    let mut vm = vm_with_script(
        Program::new().push_int(flv).pushtoken().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => {
            assert!(t.is_zero_qty());
            assert_eq!(t.flv(), flv);
        }
        _ => panic!("expected ClearToken"),
    }
}

#[test]
fn drop_droppable_int() {
    let mut vm = vm_with_script(
        Program::new().push_int(5u64).drop_().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn drop_underflow_errors() {
    let mut vm = vm_with_script(Program::new().drop_().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}

#[test]
fn dup_immediate_zero_copies_top() {
    let mut vm = vm_with_script(
        Program::new().push_int(7u64).dup_k(0).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    assert_int(&vm.current_call.stack[1], Int253::from(7u64));
}

#[test]
fn dup_immediate_k_picks_kth_from_top() {
    // push:1, push:2, push:3, dup:2 → 1 2 3 1
    let mut vm = vm_with_script(
        Program::new()
            .push_int(1u64).push_int(2u64).push_int(3u64).dup_k(2)
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 4);
    assert_int(&vm.current_call.stack[3], Int253::from(1u64));
}

#[test]
fn dup_dynamic_pops_index() {
    // push:9, push:8, push:0, dup → 9 8 (k=0) → 9 8 8
    let mut vm = vm_with_script(
        Program::new()
            .push_int(9u64).push_int(8u64).push_int(0u64).dup()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Int253::from(8u64));
}

#[test]
fn dup_out_of_range_errors() {
    // push:5, dup:5 (only 1 item on stack)
    let mut vm = vm_with_script(
        Program::new().push_int(5u64).dup_k(5).to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn dup_noncopyable_errors() {
    // push:1, pushtoken (linear), dup:0
    let mut vm = vm_with_script(
        Program::new().push_int(1u64).pushtoken().dup_k(0).to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

#[test]
fn roll_immediate_moves_kth_to_top() {
    // push:1, push:2, push:3, roll:2 → 2 3 1
    let mut vm = vm_with_script(
        Program::new()
            .push_int(1u64).push_int(2u64).push_int(3u64).roll_k(2)
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let stack = &vm.current_call.stack;
    assert_int(&stack[0], Int253::from(2u64));
    assert_int(&stack[1], Int253::from(3u64));
    assert_int(&stack[2], Int253::from(1u64));
}

#[test]
fn roll_zero_is_noop() {
    let mut vm = vm_with_script(
        Program::new().push_int(7u64).roll_k(0).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
}

#[test]
fn roll_dynamic_pops_index() {
    // push:1, push:2, push:1, roll  → roll k=1 → stack {1,2} -> {2,1}
    let mut vm = vm_with_script(
        Program::new()
            .push_int(1u64).push_int(2u64).push_int(1u64).roll()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let stack = &vm.current_call.stack;
    assert_int(&stack[0], Int253::from(2u64));
    assert_int(&stack[1], Int253::from(1u64));
}

#[test]
fn roll_out_of_range_errors() {
    let mut vm = vm_with_script(
        Program::new().push_int(5u64).roll_k(5).to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn dup_of_copyable_dict_succeeds() {
    // {5: 50}, dup:0 — should copy the dict.
    let mut vm = vm_with_script(
        Program::new()
            .push_int(50u64).push_int(5u64).push_int(1u64).dict()
            .dup_k(0)
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
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
    let mut vm = vm_with_script(
        Program::new()
            .push_int(7u64).pushtoken()
            .push_int(5u64).push_int(1u64).dict()
            .dup_k(0)
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

