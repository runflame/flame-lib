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
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    )
    .expect("payload is portable");
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(String::cell(cell)));
    vm.op_input().expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => match &c.payload()[0] {
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
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token)],
    )
    .expect("payload is portable");
    let cell_bytes = cell.to_bytes();
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(String::from(cell_bytes)));
    vm.op_input().expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Cell(c) => match &c.payload()[0] {
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
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token1)],
    )
    .expect("payload is portable");
    let cell2 = Cell::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        vec![Value::Token(token2)],
    )
    .expect("payload is portable");
    let cell_bytes = cell2.to_bytes();

    let mut vm_p = vm_external_with_script(Vec::new());
    vm_p.push_value(Value::String(String::cell(cell1)));
    vm_p.op_input().expect("input ok");
    let id_p = match &vm_p.current_call.stack[0] {
        Value::Cell(c) => c.id(),
        _ => panic!(),
    };

    let mut vm_v = vm_external_with_script(Vec::new());
    vm_v.push_value(Value::String(String::from(cell_bytes)));
    vm_v.op_input().expect("input ok");
    let id_v = match &vm_v.current_call.stack[0] {
        Value::Cell(c) => c.id(),
        _ => panic!(),
    };

    assert_eq!(id_p, id_v, "cell id is wire-derived; same under both paths");
}

/// Cloning a `String::Cell` deep-copies the cell witness-preserving:
/// the clone serializes to identical bytes and decodes to the same id.
#[test]
fn string_cell_clone_preserves_cell() {
    let token = make_open_token(7, 11, 13, 17);
    let cell = Cell::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x55; 32]),
        vec![Value::Token(token)],
    )
    .expect("payload is portable");
    let id = cell.id();
    let s = String::cell(cell);
    let cloned = s.clone();
    // The clone serializes to identical canonical bytes.
    assert_eq!(s.to_bytes_vec(), cloned.to_bytes_vec());
    // The clone decodes to the same cell id.
    let decoded = cloned.to_cell().expect("decode ok");
    assert_eq!(decoded.id(), id);
}
