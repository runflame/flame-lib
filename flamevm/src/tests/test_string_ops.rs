//! Tests for string ops.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Convenience for `push_str(String::from(bytes.to_vec()))` — every
/// test here pushes a fixed byte string as the operand under test.
fn s(bytes: &[u8]) -> String {
    String::from(bytes.to_vec())
}

fn builder_bytes(bytes: &[u8]) -> ScriptBuilder {
    ScriptBuilder::new()
        .push_str(s(bytes))
        .slice()
        .builder()
        .append_bytes()
        .roll_k(1)
        .drop_()
}

#[test]
fn read_point_success() {
    let mut bytes = vec![0x55u8; 32];
    bytes.push(0xaa);
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&bytes))
            .read_point()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa]);
    match &vm.current_call.stack[1] {
        Value::Point(p) => assert_eq!(p.to_bytes(), [0x55u8; 32]),
        other => panic!("expected Point, got {}", value_kind(other)),
    }
    assert_int(&vm.current_call.stack[2], Scalar::from(1u64));
}

#[test]
fn read_point_too_short() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0; 31]))
            .read_point()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Scalar::from(0u64));
}

#[test]
fn write_zeros_appends_n_zero_bytes() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xaa]))
            .push_int(3u64)
            .write_zeros()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa, 0, 0, 0]);
}

#[test]
fn bit_not_inverts() {
    let mut vm = vm_with_script(builder_bytes(&[0x00, 0xff, 0xa5]).bit_not().to_bytecode());
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xff, 0x00, 0x5a]);
}

#[test]
fn bit_or_basic() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xa0, 0x0f])
            .push_str(s(&[0x05, 0xf0]))
            .bit_or()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xa5, 0xff]);
}

#[test]
fn bit_and_basic() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xff, 0xf0])
            .push_str(s(&[0xa5, 0xa5]))
            .bit_and()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xa5, 0xa0]);
}

#[test]
fn bit_xor_basic() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xff, 0x00])
            .push_str(s(&[0xa5, 0xa5]))
            .bit_xor()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x5a, 0xa5]);
}

#[test]
fn bit_or_size_mismatch_errors() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xa0])
            .push_str(s(&[0x05, 0xf0]))
            .bit_or()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::BitwiseSizeMismatch
    ));
}

#[test]
fn shift_left_by_byte() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xa0, 0xb1, 0xc2, 0xd3])
            .push_int(8u64)
            .shift_left()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xb1, 0xc2, 0xd3, 0x00]);
}

#[test]
fn shift_left_by_4_bits_discards_removed() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xab])
            .push_int(4u64)
            .shift_left()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xb0]);
}

#[test]
fn shift_left_zero_is_noop() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xab, 0xcd])
            .push_int(0u64)
            .shift_left()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xab, 0xcd]);
}

#[test]
fn shift_right_by_byte() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xa0, 0xb1, 0xc2, 0xd3])
            .push_int(8u64)
            .shift_right()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x00, 0xa0, 0xb1, 0xc2]);
}

#[test]
fn shift_right_by_4_bits_discards_removed() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xab])
            .push_int(4u64)
            .shift_right()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x0a]);
}

#[test]
fn shift_too_large_errors() {
    let mut vm = vm_with_script(
        builder_bytes(&[0xab])
            .push_int(257u64) // > 256
            .shift_left()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}
