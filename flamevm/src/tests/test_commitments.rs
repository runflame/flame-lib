//! Tests for commitments.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn op_scalar_pushes_constant_expression() {
    // Pre-load a 32-byte String on the stack, dispatch `scalar`,
    // confirm the result is Expression::Constant.
    let mut vm = vm_external_with_script(ScriptBuilder::new().scalar().to_bytecode());
    let s = String::scalar(Int253::from(99u64));
    vm.push_value(Value::String(s));
    let mut delegate = StubDelegate::new();
    vm.step_external(&mut delegate).expect("scalar ok");
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Expression(Expression::Constant(i)) => {
            assert_eq!(*i, Int253::from(99u64));
        }
        _ => panic!("expected Expression::Constant"),
    }
}

#[test]
fn op_commit_pushes_variable() {
    // Pre-load a witness-bearing String::Commitment, dispatch
    // `commit`, confirm the result is a Variable with the open
    // commitment preserved.
    let mut vm = vm_external_with_script(ScriptBuilder::new().commit().to_bytecode());
    let c = Commitment::unblinded(Int253::from(42u64));
    vm.push_value(Value::String(String::commitment(c.clone())));
    let mut delegate = StubDelegate::new();
    vm.step_external(&mut delegate).expect("commit ok");
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Variable(v) => {
            assert_eq!(v.commitment.assignment(), Some(Int253::from(42u64)));
        }
        _ => panic!("expected Variable"),
    }
}

#[test]
fn prove_then_verify_with_commit_expr_eq() {
    // pushstr <open commitment witness> ; commit ; expr ;
    // alloc(42) ; eq ; verify.
    // Both the commit-side and alloc-side Expressions point to
    // value 42 → eq holds → verify succeeds.
    let pc_gens = PedersenGens::default();
    let witness_int = Int253::from(42u64);
    // Use a blinding factor that we'll need to encode into the
    // ScriptBuilder as a witness-bearing String.
    let blinding = curve25519_dalek::scalar::Scalar::from(7u64);
    let c = Commitment::blinded_with_factor(witness_int, blinding);
    let program = ScriptBuilder::new()
        // Push the witness-bearing Commitment String. The bytecode
        // will encode it as 32 bytes (the point); the prover's
        // Run::Queue preserves the witness; the verifier walks
        // bytecode and sees String::Opaque, which downcasts to
        // Commitment::Closed(point) — sufficient for the CS to
        // bind to the same point the prover used.
        .push_str(String::commitment(c))
        .commit()
        .expr()
        .alloc(Some(witness_int))
        .eq()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).expect("prove succeeds");
    let TxResult {
        bytecode, proof, ..
    } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect("verify succeeds");
}

#[test]
fn op_decrypt_succeeds_on_matching_witness() {
    // Build a Token from cleartext (q, f); decrypt with the
    // correct (q, f, q', f') quartet pushes ClearToken(q, f).
    // The two Pedersen-opening checks are deferred into the
    // batch verifier; consume the batch at the end to confirm
    // it accepts.
    let q = Int253::from(100u64);
    let f = Int253::from(7u64);
    let q_blind = Int253::from(11u64);
    let f_blind = Int253::from(13u64);
    let qty_commit =
        Commitment::blinded_with_factor(q, curve25519_dalek::scalar::Scalar::from(11u64));
    let flv_commit =
        Commitment::blinded_with_factor(f, curve25519_dalek::scalar::Scalar::from(13u64));
    let token = Token::new(qty_commit, flv_commit);

    let mut vm = vm_external_with_script(ScriptBuilder::new().decrypt().to_bytecode());
    vm.push_value(Value::Token(token));
    vm.push_value(Value::Int253(f));
    vm.push_value(Value::Int253(f_blind));
    vm.push_value(Value::Int253(q));
    vm.push_value(Value::Int253(q_blind));
    let mut delegate = StubDelegate::new();
    vm.step_external(&mut delegate).expect("decrypt ok");

    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::ClearToken(ct) => {
            assert_eq!(ct.qty(), q);
            assert_eq!(ct.flv(), f);
        }
        _ => panic!("expected ClearToken"),
    }

    // Drain the batch and confirm both deferred openings verify.
    let batch = core::mem::replace(
        &mut delegate.batch,
        musig::BatchVerifier::new(rand::thread_rng()),
    );
    batch.verify().expect("batch verifies on correct witness");
}

#[test]
fn op_decrypt_rejects_wrong_witness() {
    // Mismatched blinding → the Pedersen-opening MSM doesn't sum to
    // identity. Under the deferred batch verification design, the
    // `decrypt` opcode itself succeeds (it just appends two
    // statements to the batch verifier); the rejection surfaces when
    // the batch is drained — same lane as Schnorr / MuSig / MSM
    // verification failures, mapped to
    // `BatchSignatureVerificationFailed` by `Verifier::verify`.
    let q = Int253::from(100u64);
    let f = Int253::from(7u64);
    let qty_commit =
        Commitment::blinded_with_factor(q, curve25519_dalek::scalar::Scalar::from(11u64));
    let flv_commit =
        Commitment::blinded_with_factor(f, curve25519_dalek::scalar::Scalar::from(13u64));
    let token = Token::new(qty_commit, flv_commit);
    let mut vm = vm_external_with_script(vec![0x9a]);
    vm.push_value(Value::Token(token));
    vm.push_value(Value::Int253(f));
    vm.push_value(Value::Int253(Int253::from(99u64))); // wrong f_blind
    vm.push_value(Value::Int253(q));
    vm.push_value(Value::Int253(Int253::from(11u64)));
    let mut delegate = StubDelegate::new();
    vm.step_external(&mut delegate)
        .expect("op_decrypt defers the check, so the step itself succeeds");

    // ClearToken got pushed (deferred design — the prover's claim is
    // taken at face value until the batch runs).
    assert!(matches!(vm.current_call.stack[0], Value::ClearToken(_)));

    // Drain the batch — wrong blinding means the deferred MSM doesn't
    // sum to identity. The verifier rejects.
    let batch = core::mem::replace(
        &mut delegate.batch,
        musig::BatchVerifier::new(rand::thread_rng()),
    );
    batch
        .verify()
        .expect_err("batch must reject mismatched blinding");
}

#[test]
fn scalar_in_internal_context_errors_external_only() {
    let mut vm = vm_with_script(ScriptBuilder::new().scalar().to_bytecode());
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn commit_in_internal_context_errors_external_only() {
    let mut vm = vm_with_script(ScriptBuilder::new().commit().to_bytecode());
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn decrypt_checks_matching_opening_immediately_in_internal_context() {
    let q = Int253::from(100u64);
    let f = Int253::from(7u64);
    let q_blind = Int253::from(11u64);
    let f_blind = Int253::from(13u64);
    let token = Token::new(
        Commitment::blinded_with_factor(q, Scalar::from(11u64)),
        Commitment::blinded_with_factor(f, Scalar::from(13u64)),
    );
    let mut vm = vm_with_script(ScriptBuilder::new().decrypt().to_bytecode());
    for value in [
        Value::Token(token),
        Value::Int253(f),
        Value::Int253(f_blind),
        Value::Int253(q),
        Value::Int253(q_blind),
    ] {
        vm.push_value(value);
    }

    vm.step_internal()
        .expect("matching opening succeeds immediately");
    assert!(matches!(
        vm.current_call.stack.as_slice(),
        [Value::ClearToken(_)]
    ));
}

#[test]
fn decrypt_rejects_wrong_opening_immediately_in_internal_context() {
    let q = Int253::from(100u64);
    let f = Int253::from(7u64);
    let token = Token::new(
        Commitment::blinded_with_factor(q, Scalar::from(11u64)),
        Commitment::blinded_with_factor(f, Scalar::from(13u64)),
    );
    let mut vm = vm_with_script(ScriptBuilder::new().decrypt().to_bytecode());
    for value in [
        Value::Token(token),
        Value::Int253(f),
        Value::Int253(Int253::from(99u64)),
        Value::Int253(q),
        Value::Int253(Int253::from(11u64)),
    ] {
        vm.push_value(value);
    }

    assert!(matches!(
        vm.step_internal(),
        Err(VMError::CommitmentOpeningMismatch)
    ));
}
