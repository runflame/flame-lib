//! Tests for witness.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Single-Token cell: encode → input with witness → assert the
/// re-attached commitments are `Open` (witness-bearing).
#[test]
fn phase22_input_with_witness_upgrades_closed_to_open() {
    let (token, tw) =
        make_token_witness_pair(100, 7, 11, 13);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    // Push bytes then dispatch op_input with a witness queue.
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![tw.clone()] };
    vm.op_input(Some(&witnesses)).expect("input ok");
    // Top of stack must be a Cell whose Token payload now carries
    // Open commitments (witness present).
    match &vm.current_call.stack[0] {
        Value::Cell(c) => match &c.payload[0] {
            Value::Token(t) => {
                assert!(
                    t.qty.witness().is_some(),
                    "qty must be Open after witness attach"
                );
                assert!(
                    t.flv.witness().is_some(),
                    "flv must be Open after witness attach"
                );
            }
            _ => panic!("payload[0] not Token"),
        },
        _ => panic!("stack[0] not Cell"),
    }
}

/// No-witness branch — payload Tokens stay `Closed` (verifier
/// path, or prover with no commitments to recover).
#[test]
fn phase22_input_no_witness_keeps_closed() {
    let (token, _tw) =
        make_token_witness_pair(100, 7, 11, 13);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    vm.op_input(None).expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => match &c.payload[0] {
            Value::Token(t) => {
                assert!(
                    t.qty.witness().is_none(),
                    "qty must be Closed without witness"
                );
            }
            _ => panic!("payload[0] not Token"),
        },
        _ => panic!("stack[0] not Cell"),
    }
}

/// Witness queue with too few entries → `WitnessCountMismatch`.
#[test]
fn phase22_input_witness_count_too_few_rejects() {
    let (t1, _w1) = make_token_witness_pair(10, 7, 11, 13);
    let (t2, _w2) = make_token_witness_pair(20, 7, 14, 15);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(t1), Value::Token(t2)],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    // Pass only ONE witness for TWO tokens.
    let (_, w_one) = make_token_witness_pair(10, 7, 11, 13);
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![w_one] };
    let err = vm.op_input(Some(&witnesses)).unwrap_err();
    assert!(matches!(err, VMError::WitnessCountMismatch));
}

/// Witness queue with too many entries → `WitnessCountMismatch`.
#[test]
fn phase22_input_witness_count_too_many_rejects() {
    let (t1, _) = make_token_witness_pair(10, 7, 11, 13);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(t1)],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    let (_, w1) = make_token_witness_pair(10, 7, 11, 13);
    let (_, w2) = make_token_witness_pair(20, 7, 14, 15);
    // TWO witnesses for ONE token.
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![w1, w2] };
    let err = vm.op_input(Some(&witnesses)).unwrap_err();
    assert!(matches!(err, VMError::WitnessCountMismatch));
}

/// Witness with bogus blinding factor → `WitnessPointMismatch`.
/// Guards against silent CS failure later in the pipeline.
#[test]
fn phase22_input_witness_point_mismatch_rejects() {
    let (token, _) = make_token_witness_pair(10, 7, 11, 13);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    // Witness with DIFFERENT blinding → different point.
    let (_, bogus_w) = make_token_witness_pair(10, 7, 999, 13);
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![bogus_w] };
    let err = vm.op_input(Some(&witnesses)).unwrap_err();
    assert!(matches!(err, VMError::WitnessPointMismatch));
}

/// `TokenWitness` built with `Commitment::Closed` (point-only)
/// is rejected with `WitnessNotOpen`. The witness path's job is
/// to re-attach openings; a Closed-only witness defeats the
/// purpose and would silently cascade into `WitnessMissing`
/// from `mix`/`commit_variable` later.
#[test]
fn phase22_input_witness_closed_commitment_rejects() {
    let (token, _) = make_token_witness_pair(10, 7, 11, 13);
    // Use the SAME points as the cell, but as `Closed` (no
    // witness). The point-equality check would otherwise pass.
    let qty_closed = crate::Commitment::Closed(token.qty.to_point());
    let flv_closed = crate::Commitment::Closed(token.flv.to_point());
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    let bogus = crate::witness::TokenWitness {
        qty: qty_closed,
        flv: flv_closed,
    };
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![bogus] };
    let err = vm.op_input(Some(&witnesses)).unwrap_err();
    assert!(matches!(err, VMError::WitnessNotOpen));
}

/// Non-Token payload entries are passed through without
/// consuming the witness queue. A cell with [Int253, Token,
/// Int253] payload needs exactly one witness.
#[test]
fn phase22_input_witness_for_non_token_payload_skipped() {
    let (token, witness) =
        make_token_witness_pair(50, 9, 17, 19);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![
            Value::Int253(Int253::from(1u64)),
            Value::Token(token),
            Value::Int253(Int253::from(2u64)),
        ],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![witness] };
    vm.op_input(Some(&witnesses)).expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => {
            assert_eq!(c.payload.len(), 3);
            // payload[0] / payload[2] still Int253; payload[1]
            // now witnessed Open Token.
            assert!(matches!(c.payload[0], Value::Int253(_)));
            match &c.payload[1] {
                Value::Token(t) => {
                    assert!(t.qty.witness().is_some());
                }
                _ => panic!("payload[1] not Token"),
            }
            assert!(matches!(c.payload[2], Value::Int253(_)));
        }
        _ => panic!("stack[0] not Cell"),
    }
}

/// Encoded byte for `Instruction::Input(Some(witness))` is
/// still just `0x90`. The witness lives prover-side only.
#[test]
fn phase22_input_with_witness_encodes_to_bare_byte() {
    let (_, w) = make_token_witness_pair(1, 2, 3, 4);
    let witnesses =
        crate::witness::InputWitnesses { tokens: vec![w] };
    let mut buf = Vec::new();
    crate::ops::Instruction::Input(Some(Box::new(witnesses)))
        .encode(&mut buf);
    assert_eq!(buf, vec![0x90]);
    // Parsing reconstructs Input(None).
    let mut r: &[u8] = &buf;
    let parsed = crate::ops::Instruction::parse(&mut r)
        .expect("parses");
    assert!(matches!(parsed, crate::ops::Instruction::Input(None)));
}

