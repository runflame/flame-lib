//! Tests for string ops.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn read_bits_n_zero_succeeds_and_yields_zero() {
    let mut script = pushstr_bytes(&[0xaa, 0xbb]);
    push_small_uint(&mut script, 0);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    // string is unchanged (no bytes consumed)
    assert_str(&vm.current_call.stack[0], &[0xaa, 0xbb]);
    assert_int(&vm.current_call.stack[1], Int253::zero());
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_bits_partial_byte_masks_high_bits() {
    // Source byte: 0b1111_1111 = 0xff. Read 5 bits LSB-first → low 5 bits = 0b11111 = 31.
    let mut script = pushstr_bytes(&[0xff, 0x00]);
    push_small_uint(&mut script, 5);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    // One byte was consumed even though only 5 bits were "used".
    assert_str(&vm.current_call.stack[0], &[0x00]);
    assert_int(&vm.current_call.stack[1], Int253::from(31u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_bits_too_short_preserves_string() {
    // n=16 requires 2 bytes; only 1 available.
    let mut script = pushstr_bytes(&[0xaa]);
    push_small_uint(&mut script, 16);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_str(&vm.current_call.stack[0], &[0xaa]);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_n_257_hard_fails() {
    let mut script = pushstr_bytes(&[0u8; 33]);
    push_small_uint(&mut script, 257);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn read_bits_magnitude_at_ell_soft_fails() {
    // Canonical scalar magnitude exactly ℓ (the order) is *not*
    // canonical — `from_canonical_bytes` rejects. With n=256 and
    // sign bit = 0, bytes are the encoding of ℓ.
    let ell_le: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
        0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];
    let mut script = pushstr_bytes(&ell_le);
    push_small_uint(&mut script, 256);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    // Soft-fail: original 32-byte string restored, marker = 0.
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_str(&vm.current_call.stack[0], &ell_le);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_magnitude_above_ell_soft_fails() {
    // ℓ + 1: still non-canonical, must soft-fail.
    let mut bytes: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
        0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];
    bytes[0] = bytes[0].wrapping_add(1);
    let mut script = pushstr_bytes(&bytes);
    push_small_uint(&mut script, 256);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &bytes);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_negative_zero_soft_fails() {
    // n=256, magnitude = 0, sign bit = 1 → negative zero. Reject.
    let mut bytes = [0u8; 32];
    bytes[31] = 0x80;
    let mut script = pushstr_bytes(&bytes);
    push_small_uint(&mut script, 256);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &bytes);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_bits_roundtrip_nonneg_at_various_n() {
    // n=1,8,64,252,253,255,256. Value chosen distinct for each.
    let cases: &[(usize, u64)] = &[
        (1, 1),
        (8, 0xab),
        (64, 0x0123_4567_89ab_cdef),
        (252, 0xdead_beef_cafe_babe),
        (253, 0xfeed_face_0123_4567),
        (255, 0x5555_5555_5555_5555),
        (256, 0x7fff_ffff_ffff_ffff), // bit 255 = 0 → positive
    ];
    for (n, v) in cases.iter().copied() {
        let value = Int253::from(v);
        let bytes = writebits_bytes(&value, n);
        // build script: pushstr(bytes), push n, readbits
        let mut script = pushstr_bytes(&bytes);
        push_small_uint(&mut script, n as u32);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap_or_else(|e| panic!("n={} v={} err={:?}", n, v, e));
        assert_int(&vm.current_call.stack[1], value);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
        assert_str(&vm.current_call.stack[0], &[]);
    }
}

#[test]
fn read_bits_roundtrip_negative_at_n_256() {
    // Only n=256 carries the sign bit. Pick a small negative value.
    let value = Int253::from_parts(true, Scalar::from(12345u64));
    let bytes = value.to_bytes();
    let mut script = pushstr_bytes(&bytes);
    push_small_uint(&mut script, 256);
    script.push(0x40);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_int_positive_roundtrip() {
    let value = Int253::from(1u64);
    let mut script = pushstr_bytes(&value.to_bytes());
    script.push(0x41);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_int_negative_roundtrip() {
    let value = Int253::from_parts(true, Scalar::from(1u64));
    let mut script = pushstr_bytes(&value.to_bytes());
    script.push(0x41);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
}

#[test]
fn read_int_zero_roundtrip() {
    let value = Int253::zero();
    let mut script = pushstr_bytes(&value.to_bytes());
    script.push(0x41);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
}

#[test]
fn read_int_large_magnitude_roundtrip() {
    // ℓ - 1 (max canonical magnitude), positive.
    let ell_minus_1: [u8; 32] = [
        0xec, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
        0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];
    let mut script = pushstr_bytes(&ell_minus_1);
    script.push(0x41);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[1] {
        Value::Int253(i) => assert_eq!(i.to_bytes(), ell_minus_1),
        other => panic!("expected Int253, got {}", value_kind(other)),
    }
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_int_too_short_preserves_string() {
    let mut script = pushstr_bytes(&[0xaa; 31]);
    script.push(0x41);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_str(&vm.current_call.stack[0], &[0xaa; 31]);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_int_negative_zero_soft_fails() {
    let mut bytes = [0u8; 32];
    bytes[31] = 0x80;
    let mut script = pushstr_bytes(&bytes);
    script.push(0x41);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &bytes);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_str_success() {
    let mut script = pushstr_bytes(&[1, 2, 3, 4, 5]);
    script.push(0x02);
    script.push(0x42);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[3, 4, 5]);
    assert_str(&vm.current_call.stack[1], &[1, 2]);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_str_too_short_preserves() {
    let mut script = pushstr_bytes(&[1]);
    script.push(0x05);
    script.push(0x42);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[1]);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn read_point_success() {
    let mut bytes = vec![0x55u8; 32];
    bytes.push(0xaa);
    let mut script = pushstr_bytes(&bytes);
    script.push(0x43);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa]);
    match &vm.current_call.stack[1] {
        Value::Point(p) => assert_eq!(p.as_bytes(), &[0x55u8; 32]),
        other => panic!("expected Point, got {}", value_kind(other)),
    }
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn read_point_too_short() {
    let mut script = pushstr_bytes(&[0; 31]);
    script.push(0x43);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn write_bits_full_byte() {
    let mut script = pushstr_bytes(&[0xaa]);
    script.push(0x10);
    script.push(0xab);
    push_small_uint(&mut script, 8);
    script.push(0x44);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa, 0xab]);
}

#[test]
fn write_bits_non_aligned_hard_fails() {
    // n=5 is not a multiple of 8 → hard-fail with BitCountOutOfRange.
    let mut script = pushstr_bytes(&[]);
    script.push(0x10);
    script.push(0xff);
    push_small_uint(&mut script, 5);
    script.push(0x44);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::BitCountOutOfRange
    ));
}

#[test]
fn write_bits_n_zero_is_noop() {
    let mut script = pushstr_bytes(&[0xaa]);
    script.push(0x10);
    script.push(0x07);
    push_small_uint(&mut script, 0);
    script.push(0x44);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa]);
}

#[test]
fn write_bits_n_257_hard_fails() {
    let mut script = pushstr_bytes(&[]);
    script.push(0x10);
    script.push(0x07);
    push_small_uint(&mut script, 257);
    script.push(0x44);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

#[test]
fn write_then_read_bits_roundtrip_nonneg() {
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
        // writebits requires n to be a multiple of 8; skip the others
        // for this roundtrip (sub-byte n is exercised in the readbits
        // round-trip tests which do not write via the opcode).
        if n % 8 != 0 {
            continue;
        }
        let value = Int253::from(v);
        let mut script = pushstr_bytes(&[]);
        // push v (≤ u64::MAX), LE per opcode spec.
        script.push(0x14);
        script.extend_from_slice(&v.to_le_bytes());
        push_small_uint(&mut script, n as u32);
        script.push(0x44); // writebits → stack: [s']
        push_small_uint(&mut script, n as u32);
        script.push(0x40); // readbits → stack: [s'' x 1]
        let mut vm = vm_with_script(script);
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
    // Build manually to avoid relying on a "push negative int" path.
    let mut script = pushstr_bytes(&[]);
    // pushstr the int's encoding, then append to the empty string.
    script.extend_from_slice(&pushstr_bytes(&bytes));
    script.push(0x46); // append (s s' → s'')
    push_small_uint(&mut script, 256);
    script.push(0x40); // readbits
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], value);
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn write_int_appends_full_32_bytes() {
    let mut script = pushstr_bytes(&[]);
    script.push(0x10);
    script.push(0x07);
    script.push(0x45);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    let expected = Int253::from(7u64).to_bytes();
    assert_str(&vm.current_call.stack[0], &expected);
}

#[test]
fn write_then_read_int_roundtrip_signs_and_extremes() {
    // ±1, ±(ℓ-1), zero, and a moderately large positive magnitude.
    let ell_minus_1: [u8; 32] = [
        0xec, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
        0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];
    let mut neg_ell_minus_1 = ell_minus_1;
    neg_ell_minus_1[31] |= 0x80;
    let values: Vec<Int253> = vec![
        Int253::from(1u64),
        Int253::from_parts(true, Scalar::from(1u64)),
        Int253::from_bytes(ell_minus_1).unwrap(),
        Int253::from_bytes(neg_ell_minus_1).unwrap(),
        Int253::zero(),
        Int253::from(0xdead_beef_cafe_babe_u64),
    ];
    for v in values {
        let bytes = v.to_bytes();
        // pushstr(empty), pushstr(bytes), append, readint
        let mut script = pushstr_bytes(&[]);
        script.extend_from_slice(&pushstr_bytes(&bytes));
        script.push(0x46);
        script.push(0x41);
        let mut vm = vm_with_script(script);
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
fn append_concatenates() {
    let mut script = pushstr_bytes(&[1, 2]);
    script.extend_from_slice(&pushstr_bytes(&[3, 4, 5]));
    script.push(0x46);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[1, 2, 3, 4, 5]);
}

#[test]
fn write_zeros_appends_n_zero_bytes() {
    let mut script = pushstr_bytes(&[0xaa]);
    script.push(0x03);
    script.push(0x47);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xaa, 0, 0, 0]);
}

#[test]
fn bit_not_inverts() {
    let mut script = pushstr_bytes(&[0x00, 0xff, 0xa5]);
    script.push(0x48);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xff, 0x00, 0x5a]);
}

#[test]
fn bit_or_basic() {
    let mut script = pushstr_bytes(&[0xa0, 0x0f]);
    script.extend_from_slice(&pushstr_bytes(&[0x05, 0xf0]));
    script.push(0x49);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xa5, 0xff]);
}

#[test]
fn bit_and_basic() {
    let mut script = pushstr_bytes(&[0xff, 0xf0]);
    script.extend_from_slice(&pushstr_bytes(&[0xa5, 0xa5]));
    script.push(0x4a);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xa5, 0xa0]);
}

#[test]
fn bit_xor_basic() {
    let mut script = pushstr_bytes(&[0xff, 0x00]);
    script.extend_from_slice(&pushstr_bytes(&[0xa5, 0xa5]));
    script.push(0x4b);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x5a, 0xa5]);
}

#[test]
fn bit_or_size_mismatch_errors() {
    let mut script = pushstr_bytes(&[0xa0]);
    script.extend_from_slice(&pushstr_bytes(&[0x05, 0xf0]));
    script.push(0x49);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::BitwiseSizeMismatch
    ));
}

#[test]
fn shift_left_by_byte() {
    let mut script = pushstr_bytes(&[0xa0, 0xb1, 0xc2, 0xd3]);
    script.push(0x10);
    script.push(8);
    script.push(0x4c);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xb1, 0xc2, 0xd3, 0x00]);
    assert_str(&vm.current_call.stack[1], &[0xa0]);
}

#[test]
fn shift_left_by_4_bits_left_pads_removed() {
    let mut script = pushstr_bytes(&[0xab]);
    script.push(0x04);
    script.push(0x4c);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xb0]);
    assert_str(&vm.current_call.stack[1], &[0x0a]);
}

#[test]
fn shift_left_zero_is_noop() {
    let mut script = pushstr_bytes(&[0xab, 0xcd]);
    script.push(0x00);
    script.push(0x4c);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0xab, 0xcd]);
    assert_str(&vm.current_call.stack[1], &[]);
}

#[test]
fn shift_right_by_byte() {
    let mut script = pushstr_bytes(&[0xa0, 0xb1, 0xc2, 0xd3]);
    script.push(0x10);
    script.push(8);
    script.push(0x4d);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x00, 0xa0, 0xb1, 0xc2]);
    assert_str(&vm.current_call.stack[1], &[0xd3]);
}

#[test]
fn shift_right_by_4_bits_right_pads_removed() {
    let mut script = pushstr_bytes(&[0xab]);
    script.push(0x04);
    script.push(0x4d);
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[0x0a]);
    assert_str(&vm.current_call.stack[1], &[0xb0]);
}

#[test]
fn shift_too_large_errors() {
    let mut script = pushstr_bytes(&[0xab]);
    script.push(0x12); // pushint16 positive
    script.extend_from_slice(&257u16.to_le_bytes()); // 257 > 256
    script.push(0x4c);
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::IndexOutOfRange
    ));
}

