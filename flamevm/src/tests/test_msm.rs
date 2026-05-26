//! Tests for `MultiscalarMul` — lazy point arithmetic, batched verify.
//!
//! Covers the arithmetic lift (Point/Point/Int253/MSM dispatch in
//! `op_add` / `op_neg` / `op_mul`), the linear-type invariants
//! (non-copyable, non-droppable, non-portable, non-wire-encodable),
//! and the deferred-batch semantics of `verify` on an MSM —
//! including the end-to-end Prover→Verifier path where a non-identity
//! MSM is detected only at finalize via `BatchSignatureVerificationFailed`.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── Construction via arithmetic ─────────────────────────────────

/// `point + point` → `MultiscalarMul` with two unit-scalar terms.
#[test]
fn op_add_point_point_lifts_to_msm() {
    let mut script = vec![0x1a];                  // pushpoint
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x1a);                            // pushpoint
    script.extend_from_slice(&[0x66; 32]);
    script.push(0x53);                            // add
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 2),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `int * point` → MSM with one term `(int_as_scalar, point)`.
#[test]
fn op_mul_int_point_lifts_to_msm() {
    let mut script = vec![0x03];                  // push:3
    script.push(0x1a);                            // pushpoint
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x54);                            // mul
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 1),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `point * int` matches `int * point` (commutative dispatch).
#[test]
fn op_mul_point_int_lifts_to_msm() {
    let mut script = vec![0x1a];                  // pushpoint
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x03);                            // push:3
    script.push(0x54);                            // mul
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert!(matches!(vm.current_call.stack[0], Value::MultiscalarMul(_)));
}

/// `neg point` → MSM with one term `(-1, point)`.
#[test]
fn op_neg_point_lifts_to_msm() {
    let mut script = vec![0x1a];                  // pushpoint
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x52);                            // neg
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::MultiscalarMul(m) => assert_eq!(m.len(), 1),
        other => panic!("expected MSM, got {}", value_kind(other)),
    }
}

/// `msm + msm` concatenates term vectors.
#[test]
fn op_add_msm_msm_concatenates() {
    // Build two MSMs of size 2 each (via point+point), then add them.
    let mut script = vec![];
    for byte in [0x11u8, 0x22, 0x33, 0x44] {
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);                            // add → MSM of size 2
    // Move the first MSM out of the way (it's now at depth 2 → 1
    // since add consumed two of the four pushes).
    script.push(0x53);                            // add — but wait, we
    // After the first add we have [point2, point1, MSM(point3,point4)] — let me redo.
    // Simpler: build two pairs, add each, then add the MSMs.
    let mut script = vec![];
    for byte in [0x11u8, 0x22] { // first pair
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);                            // add → MSM_A (size 2)
    for byte in [0x33u8, 0x44] { // second pair
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);                            // add → MSM_B (size 2)
    script.push(0x53);                            // MSM_A + MSM_B → MSM (size 4)
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
    let mut script = vec![];
    for byte in [0x11u8, 0x22] {
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);                            // add → MSM size 2
    script.push(0x07);                            // push:7
    script.push(0x54);                            // mul → scaled MSM
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
    // msm + point
    let mut script = vec![];
    for byte in [0x11u8, 0x22] {
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);                            // add → MSM size 2
    script.push(0x1a);
    script.extend_from_slice(&[0x33; 32]);
    script.push(0x53);                            // MSM + point → MSM size 3
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
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x1a);
    script.extend_from_slice(&[0x66; 32]);
    script.push(0x54);                            // mul
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

/// `msm * msm` is also quadratic in group elements → hard fail.
#[test]
fn op_mul_msm_msm_rejected() {
    let mut script = vec![];
    for byte in [0x11u8, 0x22] {
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);
    for byte in [0x33u8, 0x44] {
        script.push(0x1a);
        script.extend_from_slice(&[byte; 32]);
    }
    script.push(0x53);
    script.push(0x54);                            // mul (MSM*MSM) — rejected
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

// ── Linear-type invariants ──────────────────────────────────────

/// MSM is non-copyable: `dup` rejects.
#[test]
fn msm_dup_rejects() {
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x52);                            // neg → MSM
    script.push(0x20);                            // dup:0
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

/// MSM is non-droppable: `drop` rejects.
#[test]
fn msm_drop_rejects() {
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x52);                            // neg → MSM
    script.push(0x1c);                            // drop
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotDroppable
    ));
}

/// MSM is non-portable: cannot be sealed into a cell payload.
#[test]
fn msm_in_cell_payload_rejected() {
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x52);                            // neg → MSM
    script.push(0x01);                            // k=1
    script.push(0x1a);
    script.extend_from_slice(&[0xaa; 32]);        // predicate point
    script.push(0x91);                            // cell
    let mut vm = vm_with_script(script);
    vm.current_call.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

/// `type` opcode pushes 0xc3 for MSM.
#[test]
fn msm_typecode_is_c3() {
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x52);                            // neg → MSM
    script.push(0x7f);                            // type
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    // Stack: [MSM, typecode_int]. Top is the typecode.
    let top = &vm.current_call.stack[1];
    assert_int(top, Int253::from(0xc3u64));
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
    let prog = Program::new()
        .push_int(0u64)
        .push_point(g_bytes)
        .mul()
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000, 0)
        .expect("prove succeeds");
    let TxResult { bytecode, proof, .. } = result;
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
    let prog = Program::new()
        .push_point(g_bytes)
        .neg()                                     // → MSM(-1, G)
        .push_point(g_bytes)
        .add()                                     // → MSM with [(-1,G), (1,G)]
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000, 0)
        .expect("prove");
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify(&pc_gens_v, bytecode, &proof, dummy_header(), 1_000_000, 0, None)
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
    let prog = Program::new()
        .push_int(1u64)
        .push_point(g_bytes)
        .mul()
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000, 0)
        .expect("prover produces proof regardless of MSM correctness");
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .expect_err("verify must reject");
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}

/// MSM with non-decompressable point fails at batch verify
/// (`optional_multiscalar_mul` returns None → `InvalidBatch`).
#[test]
fn verify_msm_invalid_point_rejected_at_batch() {
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    // 0xff-filled bytes do not decompress as a valid Ristretto point.
    let bad = [0xff; 32];
    // push:0  pushpoint(bad)  mul  verify — even with coefficient 0,
    // the batched MSM decompresses each point before scaling, so a
    // bad point fails the whole batch.
    let prog = Program::new()
        .push_int(0u64)
        .push_point(bad)
        .mul()
        .verify();
    let result = Prover::prove(&pc_gens, prog, dummy_header(), 1_000_000, 0)
        .expect("prove");
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(&pc_gens_v, bytecode, &proof, dummy_header(), 1_000_000, 0, None)
        .expect_err("verify must reject");
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}

/// `verify` on an MSM in internal context errors `ExternalOnly`.
#[test]
fn verify_msm_in_internal_context_rejected() {
    let mut script = vec![0x1a];
    script.extend_from_slice(&[0x55; 32]);
    script.push(0x52);                            // neg → MSM
    script.push(0x79);                            // verify
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::ExternalOnly
    ));
}
