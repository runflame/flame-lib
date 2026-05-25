//! Tests for the `String::Cell` witness carrier and `op_input`.
//!
//! Mirrors zkvm's `String::Output` pattern: the prover pushes a
//! `String::Cell(c)` carrying open commitments on Token payloads;
//! the verifier pushes `String::Opaque(c.to_bytes())` and `to_cell`
//! decodes to closed commitments. There is no separate witness
//! queue and no re-attachment step.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Prover path: push `String::Cell(c)` whose Token carries
/// `Commitment::Open`. After `op_input` the Token on the stack
/// still has the open commitments — witnesses survived intact.
#[test]
fn input_string_cell_preserves_open_commitments() {
    let token = make_open_token(100, 7, 11, 13);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    );
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::cell(cell)));
    vm.op_input().expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => match &c.payload[0] {
            Value::Token(t) => {
                assert!(
                    t.qty.witness().is_some(),
                    "qty must be Open under prover-side String::Cell path"
                );
                assert!(
                    t.flv.witness().is_some(),
                    "flv must be Open under prover-side String::Cell path"
                );
            }
            _ => panic!("payload[0] not Token"),
        },
        _ => panic!("stack[0] not Cell"),
    }
}

/// Verifier path: push `String::Opaque(cell_bytes)`. After
/// `op_input` the Token has `Commitment::Closed` (no witness).
#[test]
fn input_string_opaque_yields_closed_commitments() {
    let token = make_open_token(100, 7, 11, 13);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    );
    let cell_bytes = cell.to_bytes();
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    vm.op_input().expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => match &c.payload[0] {
            Value::Token(t) => {
                assert!(
                    t.qty.witness().is_none(),
                    "qty must be Closed via opaque-bytes path"
                );
                assert!(
                    t.flv.witness().is_none(),
                    "flv must be Closed via opaque-bytes path"
                );
            }
            _ => panic!("payload[0] not Token"),
        },
        _ => panic!("stack[0] not Cell"),
    }
}

/// Prover and verifier paths produce the same cell-id — the byte
/// encoding is canonical regardless of which String variant the
/// prover chose to push.
#[test]
fn input_string_cell_and_opaque_yield_same_cell_id() {
    let token1 = make_open_token(100, 7, 11, 13);
    let token2 = make_open_token(100, 7, 11, 13);
    let cell1 = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token1)],
    );
    let cell2 = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token2)],
    );
    let cell_bytes = cell2.to_bytes();

    let mut vm_p = vm_external_with_script(Vec::new());
    vm_p.push_value(Value::String(crate::String::cell(cell1)));
    vm_p.op_input().expect("input ok");
    let id_p = match &vm_p.current_call.stack[0] {
        Value::Cell(c) => c.id(),
        _ => panic!(),
    };

    let mut vm_v = vm_external_with_script(Vec::new());
    vm_v.push_value(Value::String(crate::String::from(cell_bytes)));
    vm_v.op_input().expect("input ok");
    let id_v = match &vm_v.current_call.stack[0] {
        Value::Cell(c) => c.id(),
        _ => panic!(),
    };

    assert_eq!(id_p, id_v, "cell id is wire-derived; same under both paths");
}

/// Non-Token payload entries are decoded normally on both paths;
/// the cell payload is just a list of portable values.
#[test]
fn input_mixed_payload_decodes_on_both_paths() {
    let token = make_open_token(50, 9, 17, 19);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![
            Value::Int253(Int253::from(1u64)),
            Value::Token(token),
            Value::Int253(Int253::from(2u64)),
        ],
    );

    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::cell(cell)));
    vm.op_input().expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => {
            assert_eq!(c.payload.len(), 3);
            assert!(matches!(c.payload[0], Value::Int253(_)));
            match &c.payload[1] {
                Value::Token(t) => assert!(t.qty.witness().is_some()),
                _ => panic!("payload[1] not Token"),
            }
            assert!(matches!(c.payload[2], Value::Int253(_)));
        }
        _ => panic!("stack[0] not Cell"),
    }
}

/// `Instruction::Input` is a unit variant — encodes to exactly
/// `0x90` and round-trips. No payload, no witnesses.
#[test]
fn input_instruction_encodes_to_single_byte() {
    let mut buf = Vec::new();
    crate::ops::Instruction::Input.encode(&mut buf);
    assert_eq!(buf, vec![0x90]);
    let mut r: &[u8] = &buf;
    let parsed = crate::ops::Instruction::parse(&mut r).expect("parses");
    assert!(matches!(parsed, crate::ops::Instruction::Input));
}

/// Cloning a `String::Cell` degrades to `Opaque(bytes)` so the
/// underlying Cell (which contains non-Clonable Tokens) need not
/// be cloned. The opaque bytes still decode to the same cell id.
#[test]
fn string_cell_clone_degrades_to_opaque() {
    let token = make_open_token(7, 11, 13, 17);
    let cell = Cell::new(
        Predicate::Opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x55; 32]),
        vec![Value::Token(token)],
    );
    let id = cell.id();
    let s = crate::String::cell(cell);
    let cloned = s.clone();
    // The clone serializes to identical canonical bytes.
    assert_eq!(s.bytes_view().to_vec(), cloned.bytes_view().to_vec());
    // The clone decodes to the same cell id.
    let decoded = cloned.to_cell().expect("decode ok");
    assert_eq!(decoded.id(), id);
}
