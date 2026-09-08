//! Tests for `MultiscalarMul` — lazy point arithmetic, batched verify.
//!
//! Covers the arithmetic lift (Point/Point/Scalar/MSM dispatch in
//! `op_add` / `op_neg` / `op_mul`), the linear-type invariants
//! (non-copyable, droppable, non-portable, non-wire-encodable),
//! and the deferred-batch semantics of `verify` on an MSM —
//! including the end-to-end Prover→Verifier path where a non-identity
//! MSM is detected only at finalize via `BatchSignatureVerificationFailed`.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::MultiscalarMul;

// ── Construction via arithmetic ─────────────────────────────────

/// `point + point` → `MultiscalarMul` with two unit-scalar terms.
#[test]
fn op_add_point_point_lifts_to_msm() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .push_point([0x66; 32])
            .add()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 2),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `int * point` → MSM with one term `(int_as_scalar, point)`.
#[test]
fn op_mul_int_point_lifts_to_msm() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(3u64)
            .push_point([0x55; 32])
            .mul()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 1),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `point * int` matches `int * point` (commutative dispatch).
#[test]
fn op_mul_point_int_lifts_to_msm() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .push_int(3u64)
            .mul()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert!(matches!(vm.current_call.stack[0], Value::MultiscalarMul(_)));
}

/// `neg point` → MSM with one term `(-1, point)`.
#[test]
fn op_neg_point_lifts_to_msm() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .neg()
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 1),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `msm + msm` concatenates term vectors.
#[test]
fn op_add_msm_msm_concatenates() {
    // Build two MSMs (each of size 2 via point+point), then add them.
    let script = ScriptBuilder::new()
        .push_point([0x11; 32])
        .push_point([0x22; 32])
        .add() // MSM_A
        .push_point([0x33; 32])
        .push_point([0x44; 32])
        .add() // MSM_B
        .add() // MSM_A + MSM_B
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 4),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `msm * int` scales all coefficients (`len` unchanged).
#[test]
fn op_mul_msm_int_scales() {
    let script = ScriptBuilder::new()
        .push_point([0x11; 32])
        .push_point([0x22; 32])
        .add() // MSM size 2
        .push_int(7u64)
        .mul() // scaled MSM
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 2),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `point + msm` and `msm + point` both append (commutative).
#[test]
fn op_add_msm_point_appends_either_order() {
    let script = ScriptBuilder::new()
        .push_point([0x11; 32])
        .push_point([0x22; 32])
        .add() // MSM size 2
        .push_point([0x33; 32])
        .add() // MSM + point
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert!(matches!(&vm.current_call.stack[0],
        Value::MultiscalarMul(m) if m.len() == 3));
}

// ── Rejected operand combinations ───────────────────────────────

/// `point * point` is a quadratic group-element product — no
/// Sigma-protocol semantics → hard fail.
#[test]
fn op_mul_point_point_rejected() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .push_point([0x66; 32])
            .mul()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotScalar
    ));
}

/// `msm * msm` is also quadratic in group elements → hard fail.
#[test]
fn op_mul_msm_msm_rejected() {
    let script = ScriptBuilder::new()
        .push_point([0x11; 32])
        .push_point([0x22; 32])
        .add()
        .push_point([0x33; 32])
        .push_point([0x44; 32])
        .add()
        .mul()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotScalar
    ));
}

// ── Linear-type invariants ──────────────────────────────────────

/// MSM is non-copyable: `dup` rejects.
#[test]
fn msm_dup_rejects() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .neg()
            .dup_k(0)
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

/// MSM IS droppable: it's a pure-computation accumulator with no
/// embedded asset value. `drop` succeeds and the stack ends clean.
#[test]
fn msm_drop_succeeds() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .neg()
            .drop_()
            .to_bytecode(),
    );
    run_to_end(&mut vm).expect("MSM is droppable (pure computation)");
    assert!(vm.current_call.stack.is_empty());
}

/// MSM is non-portable: cannot be sealed into a contract payload.
#[test]
fn msm_in_contract_payload_rejected() {
    let script = ScriptBuilder::new()
        .push_point([0x55; 32])
        .neg() // → MSM
        .push_int(1u64) // count = 1
        .push_point([0xaa; 32]) // predicate
        .contract()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

// ── Deferred batch verify semantics ─────────────────────────────

/// Identity MSM via `0 * G` — verify succeeds end-to-end through
/// Prover → Verifier. The batched check `sum = identity` holds
/// because `0 * G = identity`. The point still has to *decompress*
/// (Dalek's `optional_multiscalar_mul` decompresses before scaling),
/// so we use the canonical basepoint here.
#[test]
fn verify_msm_identity_succeeds_end_to_end() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    let g_bytes = *curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED.as_bytes();
    // push:0  pushpoint(G)  mul  verify
    let prog = ScriptBuilder::new()
        .push_int(0u64)
        .push_point(g_bytes)
        .mul()
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000).expect("prove succeeds");
    let TxResult {
        bytecode, proof, ..
    } = result;
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
    .expect("verify succeeds (0*P = identity)");
}

/// Identity MSM via `P + (-P)` — also succeeds.
#[test]
fn verify_msm_negation_sum_identity_succeeds() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    // Use the basepoint (canonical valid Ristretto point) for both
    // terms so decompression succeeds on the verifier.
    let g = curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED;
    let g_bytes = *g.as_bytes();
    // pushpoint(G)  pushpoint(G)  neg  add  verify
    // Stack after neg: [G_msm(-1)] then push G → [MSM(-1,G), G]; add → MSM with terms [(-1,G), (1,G)].
    let prog = ScriptBuilder::new()
        .push_point(g_bytes)
        .neg() // → MSM(-1, G)
        .push_point(g_bytes)
        .add() // → MSM with [(-1,G), (1,G)]
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000).expect("prove");
    let TxResult {
        bytecode, proof, ..
    } = result;
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
    .expect("verify");
}

/// Non-identity MSM: `1 * G` where G is the basepoint — the sum is G
/// itself, not identity. Verifier's batch check fails.
#[test]
fn verify_msm_nonidentity_rejected_at_batch() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    let g = curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED;
    let g_bytes = *g.as_bytes();
    // push:1  pushpoint(G)  mul  verify
    let prog = ScriptBuilder::new()
        .push_int(1u64)
        .push_point(g_bytes)
        .mul()
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000)
        .expect("prover produces proof regardless of MSM correctness");
    let TxResult {
        bytecode, proof, ..
    } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect_err("verify must reject");
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}

/// `verify` attempts point decompression when scheduling the MSM but preserves
/// failure as `None`; final batch verification rejects that deferred failure.
#[test]
fn verify_msm_invalid_point_rejected_at_batch() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    // 0xff-filled bytes do not decompress as a valid Ristretto point.
    let bad = [0xff; 32];
    // push:0  pushpoint(bad)  mul  verify — even with coefficient 0,
    // `verify` records failed decompression as `None`, so scheduling succeeds
    // but final batch acceptance fails.
    let prog = ScriptBuilder::new()
        .push_int(0u64)
        .push_point(bad)
        .mul()
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000).expect("prove");
    let TxResult {
        bytecode, proof, ..
    } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect_err("verify must reject");
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}

/// `verify` on an MSM in internal context errors `ExternalOnly`.
#[test]
fn verify_msm_in_internal_context_rejected() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_point([0x55; 32])
            .neg()
            .verify()
            .to_bytecode(),
    );
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::ExternalOnly
    ));
}

#[test]
fn verify_msm_debits_decompression_and_finalization_work() {
    let point = curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED;
    let msm = MultiscalarMul::term(DalekScalar::ZERO, point);
    let expected = GAS_PER_INSTRUCTION
        + 1 // rollback/operand growth for the one-term MSM
        + linear_gas(GAS_MSM_VERIFY_BASE, GAS_MSM_VERIFY_TERM, 1).unwrap()
        + 2; // scalar and decompressed-point batch vectors

    let mut vm = vm_external_with_script(ScriptBuilder::new().verify().to_bytecode());
    vm.current_call.stack = vec![Value::MultiscalarMul(msm.clone())];
    vm.current_call.gas_limit = expected;
    let mut delegate = make_stub_delegate();
    assert!(vm
        .step_external(&mut delegate)
        .expect("exact budget succeeds"));
    assert_eq!(vm.current_call.gas_used, expected);

    let mut short = vm_external_with_script(ScriptBuilder::new().verify().to_bytecode());
    short.current_call.stack = vec![Value::MultiscalarMul(msm)];
    short.current_call.gas_limit = expected - 1;
    let mut delegate = make_stub_delegate();
    assert!(matches!(
        short.step_external(&mut delegate),
        Err(VMError::OutOfGas)
    ));
}

// ── Per-frame batch isolation ────────────────────────────────────

/// Builds an outer program that consumes a contract (to seed the anchor)
/// then opens it via the script-leaf path, with `inner` as the leaf
/// program. Returns a ScriptBuilder ready for `Prover::prove`.
fn open_with_inner(inner: ScriptBuilder, recover_failed_contract: bool) -> ScriptBuilder {
    let inner_bytes = inner.to_bytecode();
    let recovery = ScriptBuilder::new().push_int(0u64).return_().to_bytecode();
    let tree = PredicateTree::scripts_only(vec![inner_bytes.clone(), recovery], TEST_BLINDING_KEY)
        .expect("scripts_only tree");
    let cp = tree.taproot_proof_for(0).expect("cp");
    let recovery_cp = tree.taproot_proof_for(1).expect("recovery cp");
    let pred_point = tree.point;
    let contract = Contract::new(Predicate::opaque(pred_point), Anchor([0xa1; 32]), vec![])
        .expect("empty payload is portable");
    let contract_bytes = encode_contract_to_bytes(&contract);

    let mut outer = ScriptBuilder::new()
        .push_str(String::from(contract_bytes))
        .input()
        .push_point(*cp.internal_key.as_bytes());
    for (i, h) in cp.neighbors.iter().enumerate() {
        outer = outer.push_str(String::from(h.to_vec())).push_int(i as u64);
    }
    outer = outer
        .push_int(cp.neighbors.len() as u64)
        .dict()
        .push_str(String::from(cp.position.clone()))
        .push_script(inner)
        .push_int(1024u64)
        .push_int(0u64)
        .open();
    if recover_failed_contract {
        outer = push_taproot_proof_to_program(outer.drop_().drop_(), &recovery_cp)
            .push_int(1024u64)
            .push_int(0u64)
            .open()
            .verify()
            .drop_();
    }
    outer
}

/// A failed nested call that appended a *non-identity* MSM to its
/// pending batch must not pollute the parent's batch. After the
/// parent finishes cleanly (with no MSM of its own), the verifier
/// batch should sum to the identity — i.e. the proof verifies.
///
/// Without per-frame batching this would fail with
/// `BatchSignatureVerificationFailed` because the bogus 1·G ≠ 0
/// statement from the failed callee would survive in the global batch.
#[test]
fn failed_call_msm_does_not_pollute_parent_batch() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    let g_bytes = *curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED.as_bytes();

    // Inner contract-open script: append a NON-identity MSM (1·G) to the
    // child frame's batch, then deliberately fail via `verify(0)` so
    // the whole frame is rolled back into a `0` marker on the parent.
    let inner = ScriptBuilder::new()
        .push_int(1u64)
        .push_point(g_bytes)
        .mul()
        .verify() // appends 1·G to pending_batch
        .push_int(0u64)
        .verify() // VerifyFailed → frame unwinds
        .push_int(0u64)
        .return_();
    let outer = open_with_inner(inner, true)
        // Trivially-true constraint to give the proof something to check.
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(7u64)))
        .eq()
        .verify();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000).expect("prove ok");
    let TxResult {
        bytecode, proof, ..
    } = result;
    let proof = proof.expect("proof set");

    // Verifier-side must accept: the parent's batch contains only the
    // (identity) contribution from its own clean execution; the
    // child's polluting 1·G never reaches the global batch.
    let pc_gens_v = PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect("verify must accept — failed call's MSM was discarded");
}

/// Inverse: when the contract-open succeeds cleanly, its non-identity MSM
/// IS merged into the parent's batch and the verifier rejects.
/// Confirms the per-frame design isn't accidentally swallowing every
/// MSM, only those from failed frames.
#[test]
fn clean_call_msm_propagates_to_parent_batch() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    let g_bytes = *curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED.as_bytes();

    // Inner: append 1·G to batch, then return cleanly.
    let inner = ScriptBuilder::new()
        .push_int(1u64)
        .push_point(g_bytes)
        .mul()
        .verify()
        .push_int(0u64)
        .return_();
    let outer = open_with_inner(inner, false)
        .verify() // pop success marker
        .drop_(); // drop count

    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000)
        .expect("prove ok (prover always builds something)");
    let TxResult {
        bytecode, proof, ..
    } = result;
    let proof = proof.expect("proof set");

    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect_err("verify must reject — clean call merged the 1·G MSM into the parent batch");
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}
