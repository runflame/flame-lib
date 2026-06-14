//! Tests for int253 ops.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn abs_of_negative_pushes_magnitude_and_sign() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(-9i64).abs().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    // Stack: [magnitude=9, sign=1] (sign on top)
    assert_int(&vm.current_call.stack[0], Int253::from(9u64));
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
}

#[test]
fn abs_of_positive_pushes_sign_zero() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(9u64).abs().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(9u64));
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn abs_of_zero_is_sign_zero() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(0u64).abs().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn eq_pushes_one_for_equal_ints() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(7u64).push_int(7u64).eq().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    // Stack: [7, 7, 1]
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn eq_pushes_zero_for_distinct_ints() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(7u64).push_int(8u64).eq().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[2], Int253::from(0u64));
}

#[test]
fn eq_cross_type_is_zero() {
    // pushpoint, push:0, eq — different variants → 0
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_point([0u8; 32]).push_int(0u64).eq().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(vm.current_call.stack.last().unwrap(), Int253::from(0u64));
}

#[test]
fn eq_underflow_errors() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(5u64).eq().to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}

#[test]
fn eq_noncomparable_linear_type_errors() {
    // Two ClearTokens of same flavor — same variant, but linear.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(7u64).pushtoken()
            .push_int(7u64).pushtoken()
            .eq()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotComparable
    ));
}

#[test]
fn eq_two_dicts_is_not_comparable() {
    // Two empty dicts; eq must err.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(0u64).dict()
            .push_int(0u64).dict()
            .eq()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotComparable
    ));
}

#[test]
fn neg_flips_sign() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(5u64).neg().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-5i64));
}

#[test]
fn neg_of_zero_stays_positive() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(0u64).neg().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn add_basic() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(7u64).push_int(3u64).add().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(10u64));
}

#[test]
fn add_with_negative() {
    // -7 + 3 = -4
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(-7i64).push_int(3u64).add().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-4i64));
}

#[test]
fn mul_basic() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(7u64).push_int(3u64).mul().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(21u64));
}

#[test]
fn mul_sign_xor() {
    // -6 * 7 = -42
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(-6i64).push_int(7u64).mul().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-42i64));
}

#[test]
fn add_requires_int_operands() {
    // pushpoint, push:1, add — left operand not Int253.
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_point([0u8; 32]).push_int(1u64).add().to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

#[test]
fn divmod_basic() {
    // 13 / 5 → d=2, r=3
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(13u64).push_int(5u64).divmod().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(2u64));
    assert_int(&vm.current_call.stack[1], Int253::from(3u64));
}

#[test]
fn divmod_negative_dividend() {
    // -13 / 5 → d=-2, r=-3
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(-13i64).push_int(5u64).divmod().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(-2i64));
    assert_int(&vm.current_call.stack[1], Int253::from(-3i64));
}

#[test]
fn divmod_by_zero_errors() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(7u64).push_int(0u64).divmod().to_bytecode(),
    );
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
    let huge_int = Int253::from_bytes(huge).unwrap();
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(huge_int).push_int(1u64).divmod().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], huge_int);
    assert_int(&vm.current_call.stack[1], Int253::ZERO);
}

#[test]
fn mod252_empty_string_is_zero() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(String::from(Vec::<u8>::new())).mod252().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn mod252_short_string_is_le_value() {
    // [0x07, 0x00, 0x01] LE = 7 + 0*256 + 1*65536 = 65543
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(String::from(vec![0x07, 0x00, 0x01]))
            .mod252()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(65543u64));
}

#[test]
fn mod252_64_bytes_reduces() {
    // 64 bytes of 0xff — should equal 2^512 - 1 reduced mod ℓ.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(String::from(vec![0xffu8; 64]))
            .mod252()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    let expected = Int253::from(Scalar::from_bytes_mod_order_wide(&[0xff; 64]));
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn mod252_too_long_errors() {
    // 65-byte string
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(String::from(vec![0u8; 65]))
            .mod252()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StringTooLongForModReduction
    ));
}

#[test]
fn not_zero_to_one() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(0u64).not().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(1u64));
}

#[test]
fn not_nonzero_to_zero() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(5u64).not().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn and_truth_table() {
    let cases: &[(u64, u64, u64)] = &[(1, 1, 1), (1, 0, 0), (0, 1, 0), (0, 0, 0)];
    for &(a, b, expected) in cases {
        let mut vm = vm_with_script(
            ScriptBuilder::new().push_int(a).push_int(b).and().to_bytecode(),
        );
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(expected));
    }
}

#[test]
fn or_truth_table() {
    let cases: &[(u64, u64, u64)] = &[(0, 0, 0), (1, 0, 1), (0, 1, 1)];
    for &(a, b, expected) in cases {
        let mut vm = vm_with_script(
            ScriptBuilder::new().push_int(a).push_int(b).or().to_bytecode(),
        );
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(expected));
    }
}

#[test]
fn size_of_string() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(String::from(b"abc".to_vec()))
            .size()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    // Stack: [String("abc"), 3]
    assert_int(&vm.current_call.stack[1], Int253::from(3u64));
}

#[test]
fn size_of_int_errors() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(5u64).size().to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeHasNoLength
    ));
}

#[test]
fn size_underflow_errors() {
    let mut vm = vm_with_script(ScriptBuilder::new().size().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}
