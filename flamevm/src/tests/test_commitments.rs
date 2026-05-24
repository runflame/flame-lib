//! Tests for commitments.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── rich String + scalar / commit / decrypt ────────

#[test]
fn string_witness_commitment_encodes_to_point() {
    // String::Commitment(witness) serializes to the 32-byte
    // compressed Pedersen point — identical to what an Opaque
    // String wrapping the same bytes would yield.
    let c = crate::Commitment::unblinded(Int253::from(42u64));
    let point_bytes = c.to_point().as_bytes().to_vec();
    let s = String::commitment(c);
    assert_eq!(s.to_bytes_vec(), point_bytes);
    assert_eq!(s.len(), 32);
}

#[test]
fn string_witness_commitment_downcasts() {
    let c = crate::Commitment::unblinded(Int253::from(42u64));
    let s = String::commitment(c.clone());
    let recovered = s.to_commitment().expect("downcast");
    // The Open commitment is preserved on the witness-bearing path
    // (not collapsed to Closed).
    assert!(matches!(recovered, crate::Commitment::Open(_)));
    assert_eq!(recovered.assignment(), Some(Int253::from(42u64)));
}

#[test]
fn string_opaque_downcast_to_commitment_gives_closed() {
    // 32 bytes of opaque data → Commitment::Closed(point).
    let c = crate::Commitment::unblinded(Int253::from(42u64));
    let opaque = String::from(c.to_point().as_bytes().to_vec());
    let recovered = opaque.to_commitment().expect("downcast");
    assert!(matches!(recovered, crate::Commitment::Closed(_)));
    assert_eq!(recovered.to_point(), c.to_point());
}

#[test]
fn string_scalar_downcast() {
    let i = Int253::from(123u64);
    let s = String::scalar(i);
    let recovered = s.to_scalar().expect("downcast");
    assert_eq!(recovered, i);
}

#[test]
fn string_script_encodes_to_compiled_bytecode() {
    // String::Script(instrs) serializes to the canonical bytecode
    // of those instructions — the wire form a verifier reading
    // pushstr would see. So an Opaque(bytes) string wrapping that
    // bytecode and the Script(instrs) string have identical
    // `to_bytes` / `bytes_view` / `len`.
    let inner = Program::new()
        .push_int(7u64)
        .push_int(3u64)
        .add();
    let bytecode = inner.to_bytecode();
    let script_str = String::script(inner.into_instructions());
    assert_eq!(script_str.to_bytes_vec(), bytecode);
    assert_eq!(script_str.len(), bytecode.len());
}

#[test]
fn string_script_downcasts_to_instructions() {
    // `to_instructions` returns the witness-bearing Vec verbatim.
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add();
    let original = inner.instructions().to_vec();
    let s = String::script(original.clone());
    let recovered = s.to_instructions().expect("downcast");
    // Witnesses on `Alloc` survive — checking the first instruction.
    match &recovered[0] {
        crate::Instruction::Alloc(Some(w)) => {
            assert_eq!(*w, Int253::from(7u64));
        }
        other => panic!("expected Alloc(Some(7)), got {:?}", other),
    }
    // And the whole stream is byte-equivalent to what we put in
    // (same compiled bytecode).
    let bytecode_in: Vec<u8> = {
        let mut p = Program::new();
        for i in &original { p.push_instr(i.clone()); }
        p.to_bytecode()
    };
    let bytecode_out: Vec<u8> = {
        let mut p = Program::new();
        for i in &recovered { p.push_instr(i.clone()); }
        p.to_bytecode()
    };
    assert_eq!(bytecode_in, bytecode_out);
}

#[test]
fn string_opaque_downcast_to_instructions_parses_bytes() {
    // The verifier-side path: `Opaque(bytes)` → parse via
    // `Program::parse`. Witness slots end up `None`.
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add();
    let bytes = inner.to_bytecode();
    let opaque = String::from(bytes);
    let recovered = opaque.to_instructions().expect("parse ok");
    // Same shape, but Alloc's witness is gone.
    assert!(matches!(
        recovered[0],
        crate::Instruction::Alloc(None),
    ));
}

#[test]
fn op_scalar_pushes_constant_expression() {
    // Pre-load a 32-byte String on the stack, dispatch `scalar`,
    // confirm the result is Expression::Constant.
    let mut vm = vm_external_with_script(vec![0x5a]); // scalar opcode
    let s = String::scalar(Int253::from(99u64));
    vm.push_value(Value::String(s));
    let mut delegate = StubDelegate::new();
    vm.step_external(&mut delegate).expect("scalar ok");
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Expression(crate::Expression::Constant(i)) => {
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
    let mut vm = vm_external_with_script(vec![0x5b]); // commit opcode
    let c = crate::Commitment::unblinded(Int253::from(42u64));
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
    // Program as a witness-bearing String.
    let blinding = curve25519_dalek::scalar::Scalar::from(7u64);
    let c = crate::Commitment::blinded_with_factor(witness_int, blinding);
    let program = Program::new()
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
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove succeeds");
    let crate::vm::TxResult { bytecode, proof, .. } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .expect("verify succeeds");
}

#[test]
fn op_decrypt_succeeds_on_matching_witness() {
    // Build a Token from cleartext (q, f); decrypt with the
    // correct (q, f, q', f') quartet succeeds and pushes
    // ClearToken(q, f).
    let q = Int253::from(100u64);
    let f = Int253::from(7u64);
    let q_blind = Int253::from(11u64);
    let f_blind = Int253::from(13u64);
    let qty_commit = crate::Commitment::blinded_with_factor(
        q,
        curve25519_dalek::scalar::Scalar::from(11u64),
    );
    let flv_commit = crate::Commitment::blinded_with_factor(
        f,
        curve25519_dalek::scalar::Scalar::from(13u64),
    );
    let token = crate::Token::new(qty_commit, flv_commit);

    let mut vm = vm_external_with_script(vec![0x77]); // decrypt
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
}

#[test]
fn op_decrypt_rejects_wrong_witness() {
    // Mismatched blinding → commitment opens to a different point
    // → CleartextConstraintFalse.
    let q = Int253::from(100u64);
    let f = Int253::from(7u64);
    let qty_commit = crate::Commitment::blinded_with_factor(
        q,
        curve25519_dalek::scalar::Scalar::from(11u64),
    );
    let flv_commit = crate::Commitment::blinded_with_factor(
        f,
        curve25519_dalek::scalar::Scalar::from(13u64),
    );
    let token = crate::Token::new(qty_commit, flv_commit);
    let mut vm = vm_external_with_script(vec![0x77]);
    vm.push_value(Value::Token(token));
    vm.push_value(Value::Int253(f));
    vm.push_value(Value::Int253(Int253::from(99u64))); // wrong f_blind
    vm.push_value(Value::Int253(q));
    vm.push_value(Value::Int253(Int253::from(11u64)));
    let mut delegate = StubDelegate::new();
    let err = vm.step_external(&mut delegate).unwrap_err();
    assert!(matches!(err, VMError::CleartextConstraintFalse));
}

#[test]
fn instruction_scalar_commit_decrypt_mix_roundtrip() {
    // Round-trip the new Phase-13 Instruction variants.
    use crate::ops::Instruction;
    for variant in [
        Instruction::Scalar,
        Instruction::Commit,
        Instruction::Decrypt,
        Instruction::Mix,
    ] {
        let mut buf = Vec::new();
        variant.encode(&mut buf);
        assert_eq!(buf.len(), 1);
        let mut r: &[u8] = &buf;
        let parsed = Instruction::parse(&mut r).expect("parses");
        assert_eq!(format!("{:?}", parsed), format!("{:?}", variant));
    }
}

#[test]
fn scalar_in_internal_context_errors_external_only() {
    let mut vm = vm_with_script(vec![0x5a]);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn commit_in_internal_context_errors_external_only() {
    let mut vm = vm_with_script(vec![0x5b]);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn decrypt_in_internal_context_errors_external_only() {
    let mut vm = vm_with_script(vec![0x77]);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

