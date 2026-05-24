//! Tests for confidential value.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── Phase 13.5: encrypted borrow + mix ───────────────────────

#[test]
fn encrypted_borrow_produces_widetoken_and_token_pair() {
    // pushstr(open commitment for qty=42) commit
    // pushstr(open commitment for flv=7) commit
    // borrow
    // After borrow the stack is [WideToken(-42, 7), Token(42, 7)].
    // Neither is droppable on its own; we can't end a script with
    // them sitting on the stack. So we inspect the borrow result
    // by hand-driving the VM and stopping after the borrow.
    let pc_gens = PedersenGens::default();
    let qty_int = Int253::from(42u64);
    let flv_int = Int253::from(7u64);
    let qty_blind = curve25519_dalek::scalar::Scalar::from(11u64);
    let flv_blind = curve25519_dalek::scalar::Scalar::from(13u64);
    let qty_commit = crate::Commitment::blinded_with_factor(qty_int, qty_blind);
    let flv_commit = crate::Commitment::blinded_with_factor(flv_int, flv_blind);
    let program = Program::new()
        .push_str(String::commitment(qty_commit))
        .commit()
        .push_str(String::commitment(flv_commit))
        .commit()
        .borrow();
    let mut prover = Prover::new(&pc_gens);
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new_with_run(
            Run::from_program(program),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    // Step until borrow has executed (5 instructions: 4 setup + borrow).
    for _ in 0..5 {
        vm.step_external(&mut prover).expect("step ok");
    }
    // Stack: [WideToken, Token].
    assert_eq!(vm.current_call.stack.len(), 2);
    match (&vm.current_call.stack[0], &vm.current_call.stack[1]) {
        (Value::WideToken(_), Value::Token(t)) => {
            // +T side has the original qty commitment witness preserved.
            assert_eq!(t.qty.assignment(), Some(Int253::from(42u64)));
            assert_eq!(t.flv.assignment(), Some(Int253::from(7u64)));
        }
        _ => panic!("expected [WideToken, Token]"),
    }
}

#[test]
fn mix_with_single_in_single_out_balances() {
    // The simplest cloak: 1 input Token, 1 output Token with the
    // same (qty, flv). Effectively a no-op shuffle that exercises
    // the cloak gadget's range proof on the output.
    //
    // Program:
    //   pushstr(qty_commit) commit            // builds Variable
    //   pushstr(flv_commit) commit            // builds Variable
    //   borrow                                // stack: [-T, +T]
    //   roll:1                                // bring -T to top
    //   ... actually borrow's WideToken is non-portable so we
    //   can't easily plumb it through mix. Simpler: skip borrow,
    //   build a Token directly and run mix(1,1) on it.
    //
    // Construct a Token via pushstr+commit on both halves, then
    // assemble manually... actually we don't have a `make_token`
    // opcode. Use the test helper to push a Token onto the stack
    // and then run mix(1, 1).
    let pc_gens = PedersenGens::default();
    let qty_int = Int253::from(10u64);
    let flv_int = Int253::from(7u64);
    let qty_blind = curve25519_dalek::scalar::Scalar::from(11u64);
    let flv_blind = curve25519_dalek::scalar::Scalar::from(13u64);
    let qty_commit = crate::Commitment::blinded_with_factor(qty_int, qty_blind);
    let flv_commit = crate::Commitment::blinded_with_factor(flv_int, flv_blind);
    let token = crate::Token::new(qty_commit.clone(), flv_commit.clone());

    // Build a Program that supplies the output's commitment pair
    // and runs mix. We pre-push the Token via the harness.
    let program = Program::new()
        // Output commitments (pushed deepest first per op_mix):
        // first qty, then flv. Mix pops them in reverse: first
        // pop flv (top), then qty.
        .push_str(String::commitment(qty_commit))
        .push_str(String::commitment(flv_commit))
        .push_int(1u64) // m (input count)
        .push_int(1u64) // n (output count)
        .mix();
    let mut prover = Prover::new(&pc_gens);
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new_with_run(
            Run::from_program(program),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    // Pre-load the Token at the bottom of the stack (mix pops it as input).
    vm.push_value(Value::Token(token));
    // Step exactly 5 times (the program's 5 instructions). Don't
    // drive to completion — that would trigger StackNotClean on
    // the leftover Token output (which is fine; the cloak gadget
    // built its constraints regardless).
    for _ in 0..5 {
        vm.step_external(&mut prover).expect("step ok");
    }
    // Stack: [Token(10, 7)] — the single mix output.
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Token(_) => {}
        _ => panic!("expected single Token output"),
    }
    // Confirm prover.into_proof works on the accumulated CS.
    let _proof = prover.into_proof().expect("proof builds");
}

#[test]
fn cleartext_borrow_unaffected_by_overload() {
    // push:5, push:7, borrow → (ClearToken(-5,7), ClearToken(5,7))
    // remains the existing Phase-8 behavior because top-two aren't
    // Variables (the new dispatch peek doesn't catch them).
    let mut vm = vm_with_script(vec![0x05, 0x07, 0x73]);
    run_to_end(&mut vm).expect("cleartext borrow ok");
    assert_eq!(vm.current_call.stack.len(), 2);
}

// ── Phase 17: hygiene sweep ──────────────────────────────────

#[test]
fn op_mix_m_zero_rejects() {
    // Pre-load: push n=1, m=0, then mix. Pop order: n (top), m,
    // then commitments+tokens. m=0 should reject before any
    // further pops, surfacing `MixDegenerate`.
    let mut vm = vm_external_with_script(vec![]);
    // Push m=0, n=1.
    vm.push_value(Value::Int253(Int253::from(0u64))); // m
    vm.push_value(Value::Int253(Int253::from(1u64))); // n
    let mut delegate = StubDelegate::new();
    let err = vm.op_mix(&mut delegate).unwrap_err();
    assert!(matches!(err, VMError::MixDegenerate));
}

#[test]
fn op_mix_n_zero_rejects() {
    let mut vm = vm_external_with_script(vec![]);
    vm.push_value(Value::Int253(Int253::from(1u64))); // m
    vm.push_value(Value::Int253(Int253::from(0u64))); // n
    let mut delegate = StubDelegate::new();
    let err = vm.op_mix(&mut delegate).unwrap_err();
    assert!(matches!(err, VMError::MixDegenerate));
}

