//! Tests for string ops.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Convenience for `push_str(String::from(bytes.to_vec()))` — every
/// test here pushes a fixed byte string as the operand under test.
fn s(bytes: &[u8]) -> String { String::from(bytes.to_vec()) }

#[test]
fn read_bits_n_zero_succeeds_and_yields_zero() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xaa, 0xbb]))
            .push_int(0u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_str(&vm.current_call.stack[0], &[0xaa, 0xbb]);
    assert_int(&vm.current_call.stack[1], Int253::ZERO);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_bits_partial_byte_masks_high_bits() {
    // Source byte: 0b1111_1111 = 0xff. Read 5 bits LSB-first → low 5 bits = 0b11111 = 31.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xff, 0x00]))
            .push_int(5u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x00]);
    assert_int(&vm.current_call.stack[1], Int253::from(31u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_bits_too_short_preserves_string() {
    // n=16 requires 2 bytes; only 1 available.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xaa]))
            .push_int(16u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_str(&vm.current_call.stack[0], &[0xaa]);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_n_257_hard_fails() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0u8; 33]))
            .push_int(257u64)
            .read_bits()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn read_bits_negative_count_hard_fails() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[]))
            .push_int(-1i64)
            .read_bits()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

/// Canonical curve order ℓ (Ristretto subgroup order) — first byte
/// non-canonical when interpreted as a scalar magnitude.
const ELL_LE: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
    0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

#[test]
fn read_bits_magnitude_at_ell_soft_fails() {
    // Canonical scalar magnitude exactly ℓ (the order) is *not*
    // canonical — `from_canonical_bytes` rejects. With n=256 and
    // sign bit = 0, bytes are the encoding of ℓ.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&ELL_LE))
            .push_int(256u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    // Soft-fail: original 32-byte string restored, marker = 0.
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_str(&vm.current_call.stack[0], &ELL_LE);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_magnitude_above_ell_soft_fails() {
    // ℓ + 1: still non-canonical, must soft-fail.
    let mut bytes = ELL_LE;
    bytes[0] = bytes[0].wrapping_add(1);
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&bytes))
            .push_int(256u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &bytes);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_negative_zero_soft_fails() {
    // n=256, magnitude = 0, sign bit = 1 → negative zero. Reject.
    let mut bytes = [0u8; 32];
    bytes[31] = 0x80;
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&bytes))
            .push_int(256u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &bytes);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_roundtrip_nonneg_at_various_n() {
    let cases: &[(usize, u64)] = &[
        (1, 1),
        (8, 0xab),
        (64, 0x0123_4567_89ab_cdef),
        (252, 0xdead_beef_cafe_babe),
        (253, 0xfeed_face_0123_4567),
        (255, 0x5555_5555_5555_5555),
        (256, 0x7fff_ffff_ffff_ffff),
    ];
    for (n, v) in cases.iter().copied() {
        let value = Int253::from(v);
        let bytes = writebits_bytes(&value, n);
        let mut vm = vm_with_script(
            ScriptBuilder::new()
                .push_str(s(&bytes))
                .push_int(n as u64)
                .read_bits()
                .to_bytecode(),
        );
        run_to_end(&mut vm).unwrap_or_else(|e| panic!("n={} v={} err={:?}", n, v, e));
        assert_int(&vm.current_call.stack[1], value);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
        assert_str(&vm.current_call.stack[0], &[]);
    }
}

#[test]
fn read_bits_roundtrip_negative_at_n_256() {
    let value = Int253::from_parts(true, Scalar::from(12345u64));
    let bytes = value.to_bytes();
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&bytes))
            .push_int(256u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_int_positive_roundtrip() {
    let value = Int253::from(1u64);
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&value.to_bytes())).read_int().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_int_negative_roundtrip() {
    let value = Int253::from_parts(true, Scalar::from(1u64));
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&value.to_bytes())).read_int().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
}

#[test]
fn read_int_zero_roundtrip() {
    let value = Int253::ZERO;
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&value.to_bytes())).read_int().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
}

#[test]
fn read_int_large_magnitude_roundtrip() {
    // ℓ - 1 (max canonical magnitude), positive.
    let mut ell_minus_1 = ELL_LE;
    ell_minus_1[0] = 0xec;
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&ell_minus_1)).read_int().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[1] {
        Value::Int253(i) => assert_eq!(i.to_bytes(), ell_minus_1),
        other => panic!("expected Int253, got {}", value_kind(other)),
    }
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_int_too_short_preserves_string() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&[0xaa; 31])).read_int().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_str(&vm.current_call.stack[0], &[0xaa; 31]);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_int_negative_zero_soft_fails() {
    let mut bytes = [0u8; 32];
    bytes[31] = 0x80;
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&bytes)).read_int().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &bytes);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_str_success() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[1, 2, 3, 4, 5]))
            .push_int(2u64)
            .read_str()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[3, 4, 5]);
    assert_str(&vm.current_call.stack[1], &[1, 2]);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_str_too_short_preserves() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[1]))
            .push_int(5u64)
            .read_str()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[1]);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_point_success() {
    let mut bytes = vec![0x55u8; 32];
    bytes.push(0xaa);
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&bytes)).read_point().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa]);
    match &vm.current_call.stack[1] {
        Value::Point(p) => assert_eq!(p.to_bytes(), [0x55u8; 32]),
        other => panic!("expected Point, got {}", value_kind(other)),
    }
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_point_too_short() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&[0; 31])).read_point().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn write_bits_full_byte() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xaa]))
            .push_int(0xabu64)
            .push_int(8u64)
            .write_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa, 0xab]);
}

#[test]
fn write_bits_non_aligned_hard_fails() {
    // n=5 is not a multiple of 8 → hard-fail with BitCountOutOfRange.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[]))
            .push_int(0xffu64)
            .push_int(5u64)
            .write_bits()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::BitCountOutOfRange
    ));
}

#[test]
fn write_bits_n_zero_is_noop() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xaa]))
            .push_int(7u64)
            .push_int(0u64)
            .write_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa]);
}

#[test]
fn write_bits_n_257_hard_fails() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[]))
            .push_int(7u64)
            .push_int(257u64)
            .write_bits()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn write_then_read_bits_roundtrip_nonneg() {
    let cases: &[(usize, u64)] = &[
        (8, 0xab),
        (64, 0x0123_4567_89ab_cdef),
        (256, 0x7fff_ffff_ffff_ffff),
    ];
    for (n, v) in cases.iter().copied() {
        // writebits requires n to be a multiple of 8.
        let value = Int253::from(v);
        let mut vm = vm_with_script(
            ScriptBuilder::new()
                .push_str(s(&[]))
                .push_int(v)
                .push_int(n as u64)
                .write_bits()
                .push_int(n as u64)
                .read_bits()
                .to_bytecode(),
        );
        run_to_end(&mut vm).unwrap_or_else(|e| panic!("n={} v={} err={:?}", n, v, e));
        assert_str(&vm.current_call.stack[0], &[]);
        assert_int(&vm.current_call.stack[1], value);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }
}

#[test]
fn write_then_read_bits_roundtrip_negative_n_256() {
    // For n=256, sign bit at position 255 is preserved.
    let value = Int253::from_parts(true, Scalar::from(12345u64));
    let bytes = value.to_bytes();
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[]))
            .push_str(s(&bytes))
            .append()
            .push_int(256u64)
            .read_bits()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn write_then_read_int_roundtrip_signs_and_extremes() {
    let mut ell_minus_1 = ELL_LE;
    ell_minus_1[0] = 0xec;
    let mut neg_ell_minus_1 = ell_minus_1;
    neg_ell_minus_1[31] |= 0x80;
    let values: Vec<Int253> = vec![
        Int253::from(1u64),
        Int253::from_parts(true, Scalar::from(1u64)),
        Int253::from_bytes(ell_minus_1).unwrap(),
        Int253::from_bytes(neg_ell_minus_1).unwrap(),
        Int253::ZERO,
        Int253::from(0xdead_beef_cafe_babe_u64),
    ];
    for v in values {
        let bytes = v.to_bytes();
        // pushstr(empty), pushstr(bytes), append, readint
        let mut vm = vm_with_script(
            ScriptBuilder::new()
                .push_str(s(&[]))
                .push_str(s(&bytes))
                .append()
                .read_int()
                .to_bytecode(),
        );
        run_to_end(&mut vm)
            .unwrap_or_else(|e| panic!("value={:?} err={:?}", v, e));
        match &vm.current_call.stack[1] {
            Value::Int253(i) => assert_eq!(
                i.to_bytes(),
                v.to_bytes(),
                "roundtrip differed for {:?}",
                v
            ),
            other => panic!("expected Int253, got {}", value_kind(other)),
        }
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }
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
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_str(s(&[0x00, 0xff, 0xa5])).bit_not().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xff, 0x00, 0x5a]);
}

#[test]
fn bit_or_basic() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xa0, 0x0f]))
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
        ScriptBuilder::new()
            .push_str(s(&[0xff, 0xf0]))
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
        ScriptBuilder::new()
            .push_str(s(&[0xff, 0x00]))
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
        ScriptBuilder::new()
            .push_str(s(&[0xa0]))
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
        ScriptBuilder::new()
            .push_str(s(&[0xa0, 0xb1, 0xc2, 0xd3]))
            .push_int(8u64)
            .shift_left()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xb1, 0xc2, 0xd3, 0x00]);
    assert_str(&vm.current_call.stack[1], &[0xa0]);
}

#[test]
fn shift_left_by_4_bits_left_pads_removed() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xab]))
            .push_int(4u64)
            .shift_left()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xb0]);
    assert_str(&vm.current_call.stack[1], &[0x0a]);
}

#[test]
fn shift_left_zero_is_noop() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xab, 0xcd]))
            .push_int(0u64)
            .shift_left()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xab, 0xcd]);
    assert_str(&vm.current_call.stack[1], &[]);
}

#[test]
fn shift_right_by_byte() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xa0, 0xb1, 0xc2, 0xd3]))
            .push_int(8u64)
            .shift_right()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x00, 0xa0, 0xb1, 0xc2]);
    assert_str(&vm.current_call.stack[1], &[0xd3]);
}

#[test]
fn shift_right_by_4_bits_right_pads_removed() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xab]))
            .push_int(4u64)
            .shift_right()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x0a]);
    assert_str(&vm.current_call.stack[1], &[0xb0]);
}

#[test]
fn shift_too_large_errors() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_str(s(&[0xab]))
            .push_int(257u64)                   // > 256
            .shift_left()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}
