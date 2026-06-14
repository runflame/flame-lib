//! Tests for int253 ops.
//!
//! Opcode results are checked *inside the VM* via the self-checking
//! harness (`assert_stack` / `assert_top` append `eq; verify`), so no
//! test reaches into `vm.current_call.stack`. See test_helpers §Bucket B.

#![allow(unused_imports)]

use super::test_helpers::*;

fn b() -> ScriptBuilder {
    ScriptBuilder::new()
}

#[test]
fn abs_of_negative_pushes_magnitude_and_sign() {
    // -9 → [magnitude=9, sign=1] (sign on top).
    assert_stack_ints(b().push_int(-9i64).abs(), &[1, 9]);
}

#[test]
fn abs_of_positive_pushes_sign_zero() {
    assert_stack_ints(b().push_int(9u64).abs(), &[0, 9]);
}

#[test]
fn abs_of_zero_is_sign_zero() {
    assert_stack_ints(b().push_int(0u64).abs(), &[0, 0]);
}

#[test]
fn eq_pushes_one_for_equal_ints() {
    // [7, 7, 1] — eq is non-consuming, so both operands remain.
    assert_stack_ints(b().push_int(7u64).push_int(7u64).eq(), &[1, 7, 7]);
}

#[test]
fn eq_pushes_zero_for_distinct_ints() {
    assert_stack_ints(b().push_int(7u64).push_int(8u64).eq(), &[0, 8, 7]);
}

#[test]
fn eq_cross_type_is_zero() {
    // pushpoint, push:0, eq — different variants → 0 on top (Point
    // residue below, so check the top only).
    assert_top(b().push_point([0u8; 32]).push_int(0u64).eq(), 0u64);
}

#[test]
fn eq_underflow_errors() {
    assert!(matches!(run_err(b().push_int(5u64).eq()), VMError::StackUnderflow));
}

#[test]
fn eq_noncomparable_linear_type_errors() {
    // Two ClearTokens of same flavor — same variant, but linear.
    let s = b()
        .push_int(7u64).pushtoken()
        .push_int(7u64).pushtoken()
        .eq();
    assert!(matches!(run_err(s), VMError::TypeNotComparable));
}

#[test]
fn eq_two_dicts_is_not_comparable() {
    let s = b().push_int(0u64).dict().push_int(0u64).dict().eq();
    assert!(matches!(run_err(s), VMError::TypeNotComparable));
}

#[test]
fn neg_flips_sign() {
    assert_stack_ints(b().push_int(5u64).neg(), &[-5]);
}

#[test]
fn neg_of_zero_stays_positive() {
    assert_stack_ints(b().push_int(0u64).neg(), &[0]);
}

#[test]
fn add_basic() {
    assert_stack_ints(b().push_int(7u64).push_int(3u64).add(), &[10]);
}

#[test]
fn add_with_negative() {
    assert_stack_ints(b().push_int(-7i64).push_int(3u64).add(), &[-4]);
}

#[test]
fn mul_basic() {
    assert_stack_ints(b().push_int(7u64).push_int(3u64).mul(), &[21]);
}

#[test]
fn mul_sign_xor() {
    assert_stack_ints(b().push_int(-6i64).push_int(7u64).mul(), &[-42]);
}

#[test]
fn add_requires_int_operands() {
    // pushpoint, push:1, add — left operand not Int253.
    let s = b().push_point([0u8; 32]).push_int(1u64).add();
    assert!(matches!(run_err(s), VMError::TypeNotInt253));
}

#[test]
fn divmod_basic() {
    // 13 / 5 → d=2 (bottom), r=3 (top).
    assert_stack_ints(b().push_int(13u64).push_int(5u64).divmod(), &[3, 2]);
}

#[test]
fn divmod_negative_dividend() {
    // -13 / 5 → d=-2, r=-3.
    assert_stack_ints(b().push_int(-13i64).push_int(5u64).divmod(), &[-3, -2]);
}

#[test]
fn divmod_by_zero_errors() {
    assert!(matches!(run_err(b().push_int(7u64).push_int(0u64).divmod()), VMError::DivByZero));
}

#[test]
fn divmod_full_width_magnitude_succeeds() {
    // 2^128 / 1 → d = 2^128, r = 0. Regression guard against an earlier
    // `MagnitudeTooLarge` failure on operands beyond u64::MAX.
    let mut huge = [0u8; 32];
    huge[16] = 1; // 2^128
    let huge_int = Int253::from_bytes(huge).unwrap();
    assert_stack(b().push_int(huge_int).push_int(1u64).divmod(), &[Int253::ZERO, huge_int]);
}

#[test]
fn mod252_empty_string_is_zero() {
    assert_stack_ints(b().push_str(String::from(Vec::<u8>::new())).mod252(), &[0]);
}

#[test]
fn mod252_short_string_is_le_value() {
    // [0x07, 0x00, 0x01] LE = 7 + 0*256 + 1*65536 = 65543.
    assert_stack_ints(b().push_str(String::from(vec![0x07, 0x00, 0x01])).mod252(), &[65543]);
}

#[test]
fn mod252_64_bytes_reduces() {
    // 64 bytes of 0xff — should equal 2^512 - 1 reduced mod ℓ.
    let expected = Int253::from(Scalar::from_bytes_mod_order_wide(&[0xff; 64]));
    assert_stack(b().push_str(String::from(vec![0xffu8; 64])).mod252(), &[expected]);
}

#[test]
fn mod252_too_long_errors() {
    let s = b().push_str(String::from(vec![0u8; 65])).mod252();
    assert!(matches!(run_err(s), VMError::StringTooLongForModReduction));
}

#[test]
fn not_zero_to_one() {
    assert_stack_ints(b().push_int(0u64).not(), &[1]);
}

#[test]
fn not_nonzero_to_zero() {
    assert_stack_ints(b().push_int(5u64).not(), &[0]);
}

#[test]
fn and_truth_table() {
    let cases: &[(u64, u64, i64)] = &[(1, 1, 1), (1, 0, 0), (0, 1, 0), (0, 0, 0)];
    for &(a, c, expected) in cases {
        assert_stack_ints(b().push_int(a).push_int(c).and(), &[expected]);
    }
}

#[test]
fn or_truth_table() {
    let cases: &[(u64, u64, i64)] = &[(0, 0, 0), (1, 0, 1), (0, 1, 1)];
    for &(a, c, expected) in cases {
        assert_stack_ints(b().push_int(a).push_int(c).or(), &[expected]);
    }
}

#[test]
fn size_of_string() {
    // [String("abc"), 3] — String residue below, so check the top.
    assert_top(b().push_str(String::from(b"abc".to_vec())).size(), 3u64);
}

#[test]
fn size_of_int_errors() {
    assert!(matches!(run_err(b().push_int(5u64).size()), VMError::TypeHasNoLength));
}

#[test]
fn size_underflow_errors() {
    assert!(matches!(run_err(b().size()), VMError::StackUnderflow));
}
