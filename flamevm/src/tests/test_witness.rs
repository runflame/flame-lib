//! Tests for the `StringWitness::Contract` witness carrier and `op_input`.
//!
//! The prover pushes `String::contract(c)` carrying private Token openings.
//! Bytecode contains only the ContractID, while the public Contract body is
//! included in the transaction's BoC. Both paths authenticate that body first;
//! the prover then restores its matching private witnesses.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Prover path: push `String::contract(c)` whose Token carries
/// `Commitment::Open`. After `op_input` the Token on the stack
/// still has the open commitments — witnesses survived intact.
#[test]
fn input_string_contract_preserves_open_commitments() {
    let token = make_open_token(100, 7, 11, 13);
    let contract = Contract::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        test_payload(vec![Value::Token(token)]),
    )
    .expect("payload is portable");
    let mut vm = vm_external_with_script(
        ScriptBuilder::new()
            .push_str(String::contract(contract))
            .input(),
    );
    run_to_end(&mut vm).expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Contract(c) => match c.payload() {
            Value::Token(t) => {
                assert!(
                    t.qty.witness().is_some(),
                    "qty must be Open under prover-side StringWitness::Contract path"
                );
                assert!(
                    t.flv.witness().is_some(),
                    "flv must be Open under prover-side StringWitness::Contract path"
                );
            }
            _ => panic!("payload not Token"),
        },
        _ => panic!("stack[0] not Contract"),
    }
}

/// Verifier path: push `String::Opaque(contract_id)` and provide its BoC. After
/// `op_input` the Token has `Commitment::Closed` (no witness).
#[test]
fn input_string_opaque_yields_closed_commitments() {
    let token = make_open_token(100, 7, 11, 13);
    let contract = Contract::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        test_payload(vec![Value::Token(token)]),
    )
    .expect("payload is portable");
    let program = ScriptBuilder::new()
        .with_cells(contract.to_envelope().unwrap().cells().clone())
        .push_str(String::from(contract.id().to_vec()))
        .input();
    let mut vm = vm_external_with_script(program);
    run_to_end(&mut vm).expect("input ok");
    match &vm.current_call.stack[0] {
        Value::Contract(c) => match c.payload() {
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
            _ => panic!("payload not Token"),
        },
        _ => panic!("stack[0] not Contract"),
    }
}

/// Prover and verifier paths produce the same contract-id — the byte
/// encoding is canonical regardless of which String variant the
/// prover chose to push.
#[test]
fn input_string_contract_and_opaque_yield_same_contract_id() {
    let token1 = make_open_token(100, 7, 11, 13);
    let token2 = make_open_token(100, 7, 11, 13);
    let contract1 = Contract::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        test_payload(vec![Value::Token(token1)]),
    )
    .expect("payload is portable");
    let contract2 = Contract::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x42; 32]),
        test_payload(vec![Value::Token(token2)]),
    )
    .expect("payload is portable");
    let public = ScriptBuilder::new()
        .with_cells(contract2.to_envelope().unwrap().cells().clone())
        .push_str(String::from(contract2.id().to_vec()))
        .input();

    let mut vm_p = vm_external_with_script(
        ScriptBuilder::new()
            .push_str(String::contract(contract1))
            .input(),
    );
    run_to_end(&mut vm_p).expect("input ok");
    let id_p = match &vm_p.current_call.stack[0] {
        Value::Contract(c) => c.id(),
        _ => panic!(),
    };

    let mut vm_v = vm_external_with_script(public);
    run_to_end(&mut vm_v).expect("input ok");
    let id_v = match &vm_v.current_call.stack[0] {
        Value::Contract(c) => c.id(),
        _ => panic!(),
    };

    assert_eq!(
        id_p, id_v,
        "contract id is wire-derived; same under both paths"
    );
}

/// Cloning a `StringWitness::Contract` deep-copies the contract witness-preserving:
/// the clone serializes to identical bytes and decodes to the same id.
#[test]
fn string_contract_clone_preserves_contract() {
    let token = make_open_token(7, 11, 13, 17);
    let contract = Contract::new(
        Predicate::opaque(CompressedRistretto([0xaa; 32])),
        Anchor([0x55; 32]),
        test_payload(vec![Value::Token(token)]),
    )
    .expect("payload is portable");
    let id = contract.id();
    let s = String::contract(contract);
    let cloned = s.clone();
    // The clone serializes to identical canonical bytes.
    assert_eq!(s.to_bytes_vec(), cloned.to_bytes_vec());
    // The clone decodes to the same contract id.
    let decoded = cloned.to_contract().expect("decode ok");
    assert_eq!(decoded.id(), id);
}
