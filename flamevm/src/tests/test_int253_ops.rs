//! Tests for int253 ops.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── ──────────────────────────────────────────────────

// ── abs (0x50) ───────────────────────────────────────────────

#[test]
fn abs_of_negative_pushes_magnitude_and_sign() {
    // pushint8(neg, 9), abs
    let mut vm = vm_with_script(vec![0x11, 9, 0x50]);
    run_to_end(&mut vm).unwrap();
    // Stack: [magnitude=9, sign=1] (sign on top)
    assert_int(&vm.current_call.stack[0], Int253::from(9u64));
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
}

#[test]
fn abs_of_positive_pushes_sign_zero() {
    let mut vm = vm_with_script(vec![0x10, 9, 0x50]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(9u64));
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn abs_of_zero_is_sign_zero() {
    let mut vm = vm_with_script(vec![0x00, 0x50]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

// ── eq (0x51) ────────────────────────────────────────────────

#[test]
fn eq_pushes_one_for_equal_ints() {
    let mut vm = vm_with_script(vec![0x07, 0x07, 0x51]);
    run_to_end(&mut vm).unwrap();
    // Stack: [7, 7, 1]
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn eq_pushes_zero_for_distinct_ints() {
    let mut vm = vm_with_script(vec![0x07, 0x08, 0x51]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[2], Int253::from(0u64));
}

#[test]
fn eq_cross_type_is_zero() {
    // pushpoint, push:0, eq — different variants → 0
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0u8; 32]);
    script.push(0x00); // push:0
    script.push(0x51);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(vm.current_call.stack.last().unwrap(), Int253::from(0u64));
}

#[test]
fn eq_underflow_errors() {
    let mut vm = vm_with_script(vec![0x05, 0x51]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}

#[test]
fn eq_noncomparable_linear_type_errors() {
    // Two ClearTokens of same flavor — same variant, but linear.
    // push:7, pushtoken, push:7, pushtoken, eq
    let mut vm = vm_with_script(vec![0x07, 0x1b, 0x07, 0x1b, 0x51]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotComparable
    ));
}

#[test]
fn eq_two_dicts_is_not_comparable() {
    // push:0, dict, push:0, dict, eq — two empty dicts; eq must err.
    let mut vm = vm_with_script(vec![0x00, 0x60, 0x00, 0x60, 0x51]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotComparable
    ));
}

// ── neg (0x52) ───────────────────────────────────────────────

#[test]
fn neg_flips_sign() {
    let mut vm = vm_with_script(vec![0x05, 0x52]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-5i64));
}

#[test]
fn neg_of_zero_stays_positive() {
    let mut vm = vm_with_script(vec![0x00, 0x52]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

// ── add (0x53), mul (0x54) ───────────────────────────────────

#[test]
fn add_basic() {
    // push:7, push:3, add  → 10
    let mut vm = vm_with_script(vec![0x07, 0x03, 0x53]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(10u64));
}

#[test]
fn add_with_negative() {
    // pushint8(neg, 7), push:3, add  → -4
    let mut vm = vm_with_script(vec![0x11, 7, 0x03, 0x53]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-4i64));
}

#[test]
fn mul_basic() {
    let mut vm = vm_with_script(vec![0x07, 0x03, 0x54]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(21u64));
}

#[test]
fn mul_sign_xor() {
    // pushint8(neg, 6), push:7, mul → -42
    let mut vm = vm_with_script(vec![0x11, 6, 0x07, 0x54]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-42i64));
}

#[test]
fn add_requires_int_operands() {
    // pushpoint, push:1, add — left operand not Int253.
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0u8; 32]);
    script.push(0x01);
    script.push(0x53);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

// ── divmod (0x55) ────────────────────────────────────────────

#[test]
fn divmod_basic() {
    // push:13, push:5, divmod → d=2, r=3
    let mut vm = vm_with_script(vec![
        0x10, 13, // pushint8(pos, 13)
        0x10, 5, // pushint8(pos, 5)
        0x55,
    ]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(2u64));
    assert_int(&vm.current_call.stack[1], Int253::from(3u64));
}

#[test]
fn divmod_negative_dividend() {
    // -13 / 5 → d=-2, r=-3
    let mut vm = vm_with_script(vec![0x11, 13, 0x10, 5, 0x55]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-2i64));
    assert_int(&vm.current_call.stack[1], Int253::from(-3i64));
}

#[test]
fn divmod_by_zero_errors() {
    let mut vm = vm_with_script(vec![0x07, 0x00, 0x55]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DivByZero
    ));
}

#[test]
fn divmod_full_width_magnitude_succeeds() {
    // 2^128 / 1 → d = 2^128, r = 0. Regression guard against an
    // earlier `MagnitudeTooLarge` failure on operands beyond
    // u64::MAX.
    let mut huge = [0u8; 32];
    huge[16] = 1; // 2^128
    let mut script = vec![0x18];
    script.extend_from_slice(&huge);
    script.push(0x01); // push:1
    script.push(0x55);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from_bytes(huge).unwrap());
    assert_int(&vm.current_call.stack[1], Int253::zero());
}

// ── mod252 (0x56) ────────────────────────────────────────────

#[test]
fn mod252_empty_string_is_zero() {
    let script = vec![0x19, 0x00, 0x00, 0x56]; // pushstr "", mod252
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn mod252_short_string_is_le_value() {
    // pushstr [0x07, 0x00, 0x01], mod252
    // LE interpretation = 7 + 0*256 + 1*65536 = 65543
    let script = vec![0x19, 0x00, 0x03, 0x07, 0x00, 0x01, 0x56];
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(65543u64));
}

#[test]
fn mod252_64_bytes_reduces() {
    // 64 bytes of 0xff — should equal 2^512 - 1 reduced mod ℓ.
    // sub-varint tag 0, byte 64, then 64 × 0xff, then mod252.
    let script = {
        let mut s = vec![0x19, 0x00, 64];
        s.extend_from_slice(&[0xffu8; 64]);
        s.push(0x56);
        s
    };
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    // Compare against the dalek reference path.
    let expected = Int253::from(Scalar::from_bytes_mod_order_wide(&[0xff; 64]));
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn mod252_too_long_errors() {
    // 65-byte string
    let mut s = vec![0x19, 0x00, 65];
    s.extend_from_slice(&[0u8; 65]);
    s.push(0x56);
    let mut vm = vm_with_script(s);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StringTooLongForModReduction
    ));
}

// ── not / and / or (0x57..=0x59) ─────────────────────────────

#[test]
fn not_zero_to_one() {
    let mut vm = vm_with_script(vec![0x00, 0x57]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(1u64));
}

#[test]
fn not_nonzero_to_zero() {
    let mut vm = vm_with_script(vec![0x05, 0x57]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn and_truth_table() {
    // (1, 1) → 1
    let mut vm = vm_with_script(vec![0x01, 0x01, 0x58]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(1u64));
    // (1, 0) → 0
    let mut vm = vm_with_script(vec![0x01, 0x00, 0x58]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    // (0, 1) → 0
    let mut vm = vm_with_script(vec![0x00, 0x01, 0x58]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    // (0, 0) → 0
    let mut vm = vm_with_script(vec![0x00, 0x00, 0x58]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn or_truth_table() {
    // (0, 0) → 0
    let mut vm = vm_with_script(vec![0x00, 0x00, 0x59]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    // (1, 0) → 1
    let mut vm = vm_with_script(vec![0x01, 0x00, 0x59]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(1u64));
    // (0, 1) → 1
    let mut vm = vm_with_script(vec![0x00, 0x01, 0x59]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(1u64));
}

// ── size (0x5f) ──────────────────────────────────────────────

#[test]
fn size_of_string() {
    // pushstr [a, b, c], size
    let mut script = pushstr_bytes(&[b'a', b'b', b'c']);
    script.push(0x5f);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    // Stack: [String("abc"), 3]
    assert_int(&vm.current_call.stack[1], Int253::from(3u64));
}

#[test]
fn size_of_int_errors() {
    let mut vm = vm_with_script(vec![0x05, 0x5f]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeHasNoLength
    ));
}

#[test]
fn size_underflow_errors() {
    let mut vm = vm_with_script(vec![0x5f]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}

