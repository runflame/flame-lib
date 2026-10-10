//! Tests for constraints.

#![allow(unused_imports)]

use super::test_helpers::*;

/// Regression: in external context, `eq` of two Strings (or Points) must
/// cleartext peek-compare, not route into the CS branch and hard-fail
/// `to_expression()`. Guards the spec's `anchor … eq verify` binding idiom.
#[test]
fn eq_external_strings_peek_compare_not_cs_lift() {
    let pc_gens = PedersenGens::default();
    let prog = ScriptBuilder::new()
        .push_str(String::from(b"abcd".to_vec()))
        .push_str(String::from(b"abcd".to_vec()))
        .eq();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(prog.into_instructions(), CallKind::ExternalRoot, 1_000_000),
    );
    let mut prover = Prover::new(&pc_gens);
    vm.step_external(&mut prover).expect("push a");
    vm.step_external(&mut prover).expect("push b");
    vm.step_external(&mut prover)
        .expect("eq must not error in external context");
    // Non-consuming cleartext eq leaves [a, b, 1].
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Scalar::from(1u64));
}

#[test]
fn range_proof_accepts_in_range_value() {
    // alloc(42) push:64 range — 42 fits in 64 bits.
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(42u64)))
        .push_int(64u64)
        .range()
        // Constrain that the same alloc equals 42 to close the proof
        // with a non-trivial constraint (so verification has
        // something to check beyond the range gadget).
        .alloc(Some(Scalar::from(42u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).expect("prove succeeds");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .expect("verify succeeds");
}

#[test]
fn range_proof_rejects_out_of_range_value() {
    // alloc(2^9) push:8 range — 512 does NOT fit in 8 bits, so the
    // prover-side range_proof gadget rejects the witness or the
    // verifier rejects the proof.
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(512u64)))
        .push_int(8u64)
        .range()
        .alloc(Some(Scalar::from(512u64)))
        .eq()
        .verify();
    let result = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000);
    // The prover may succeed (constructs a proof with bad witness)
    // and the verifier rejects, OR the prover errors directly.
    // Either way, the full pipeline must reject. Cover both
    // outcomes for robustness.
    match result {
        Err(_) => {
            // Prover refused — good.
        }
        Ok(_pp) => {
            let TxResult {
                bytecode,
                proof,
                cells,
                ..
            } = _pp;
            let proof = proof.expect("proof set");
            let pc_gens_v = PedersenGens::default();
            let err = Verifier::verify_with_cells(
                &pc_gens_v,
                bytecode,
                &proof,
                dummy_header(),
                1_000_000,
                None,
                &cells,
            )
            .expect_err("verifier must reject out-of-range proof");
            assert!(matches!(err, VMError::InvalidR1CSProof));
        }
    }
}

#[test]
fn range_bit_count_zero_rejected() {
    // push:0 — zero-bit range proof is degenerate, rejected at the
    // opcode level.
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(0u64)))
        .push_int(0u64)
        .range()
        .alloc(Some(Scalar::from(0u64)))
        .eq()
        .verify();
    let err = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).unwrap_err();
    assert!(matches!(err, VMError::BitCountOutOfRange));
}

#[test]
fn range_bit_count_above_64_rejected() {
    // push:65 — bit count exceeds BitRange::max() (64).
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(1u64)))
        .push_int(65u64)
        .range()
        .alloc(Some(Scalar::from(1u64)))
        .eq()
        .verify();
    let err = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).unwrap_err();
    assert!(matches!(err, VMError::BitCountOutOfRange));
}

#[test]
fn constraint_and_overload_combines_two_constraints() {
    // (alloc(7) == alloc(7)) AND (alloc(3) == alloc(3))
    //   → Constraint composition — verify succeeds (both true).
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        // Constraint 1: alloc(7) == alloc(7) — pushes Constraint
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(7u64)))
        .eq()
        // Constraint 2: alloc(3) == alloc(3) — pushes Constraint
        .alloc(Some(Scalar::from(3u64)))
        .alloc(Some(Scalar::from(3u64)))
        .eq()
        // AND the two Constraints
        .and()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).expect("prove succeeds");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .expect("verify succeeds");
}

#[test]
fn constraint_or_overload_combines_two_constraints() {
    // (alloc(7) == alloc(8)) OR (alloc(3) == alloc(3))
    //   → first is false, second is true; OR yields true. Verify ok.
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(8u64)))
        .eq()
        .alloc(Some(Scalar::from(3u64)))
        .alloc(Some(Scalar::from(3u64)))
        .eq()
        .or()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).expect("prove succeeds");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .expect("verify succeeds");
}

#[test]
fn constraint_not_overload_negates_constraint() {
    // NOT (alloc(7) == alloc(8))  → NOT false → true.
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(8u64)))
        .eq()
        .not()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).expect("prove succeeds");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .expect("verify succeeds");
}

#[test]
fn constraint_and_with_false_branch_rejected() {
    // (alloc(7) == alloc(7)) AND (alloc(3) == alloc(99))
    //   → first true, second false; AND is false. Verifier rejects.
    let pc_gens = PedersenGens::default();
    let program = ScriptBuilder::new()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(7u64)))
        .eq()
        .alloc(Some(Scalar::from(3u64)))
        .alloc(Some(Scalar::from(99u64)))
        .eq()
        .and()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000)
        .expect("prove succeeds (constructs proof of unsatisfiable constraint)");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .unwrap_err();
    assert!(matches!(err, VMError::InvalidR1CSProof));
}

#[test]
fn range_expression_in_internal_context_errors_external_only() {
    // Even a public Expression constant remains external-only. Raw scalars
    // are admitted separately, without exposing internal execution to R1CS.
    let mut vm = vm_with_script(ScriptBuilder::new().range().to_bytecode());
    vm.push_value(Value::Expression(Expression::Constant(Scalar::ONE)));
    vm.push_value(Value::Scalar(Scalar::from(64u64)));
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

// ── CS rollback on call failure ─────────────────────────────────

/// Wraps `inner` in an `open` of a single-leaf contract that consumes
/// itself (`input` then `open`) so the script runs under a real
/// `last_anchor`. The outer program returns the inner's failure
/// or success marker on the stack for the caller's continuation.
fn open_with_inner(inner: ScriptBuilder, recover_failed_contract: bool) -> ScriptBuilder {
    open_with_test_inner(inner, recover_failed_contract)
}

/// A failed `open` whose child allocated an *unsatisfiable* R1CS
/// constraint must not pollute the caller's CS. With CS rollback,
/// the child's allocations and constraints are dropped from the
/// Prover/Verifier's R1CS at failure time — so the caller's proof
/// over its own (satisfiable) constraints verifies cleanly.
///
/// Without CS rollback, the verifier sees `7 + 3 == 99` in the
/// constraint set and rejects with `InvalidR1CSProof`.
#[test]
fn failed_call_unsat_cs_does_not_pollute_parent_proof() {
    let pc_gens = PedersenGens::default();
    // Child: introduces an UNSATISFIABLE constraint (7 + 3 == 99),
    // then deliberately fails via `verify(0)` so the whole frame
    // is rolled back into a `0` marker on the parent.
    let inner = ScriptBuilder::new()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(99u64)))
        .eq()
        .verify() // unsat constraint into CS
        .push_int(0u64)
        .verify() // VerifyFailed → unwind
        .push_int(0u64)
        .return_();
    let outer = open_with_inner(inner, true)
        // Parent's own constraint: 7 + 3 == 10 — satisfiable.
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(10u64)))
        .eq()
        .verify();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000).expect("prove ok");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = result;
    let proof = proof.expect("proof set");

    // Verifier must accept: the child's `7+3==99` was rolled back
    // out of the CS; only the parent's satisfiable `7+3==10`
    // remains.
    let pc_gens_v = PedersenGens::default();
    Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .expect("verify must accept — failed call's CS contributions rolled back");
}

/// Inverse: when the contract-open succeeds cleanly, its allocations
/// and constraints stay in the CS. If the child's constraint is
/// unsatisfiable, the verifier rejects — confirming the rollback
/// only fires on the failure path.
#[test]
fn clean_call_cs_alloc_propagates_to_parent_proof() {
    let pc_gens = PedersenGens::default();
    // Child: adds an UNSATISFIABLE constraint and returns cleanly.
    let inner = ScriptBuilder::new()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(99u64)))
        .eq()
        .verify()
        .push_int(0u64)
        .return_();
    let outer = open_with_inner(inner, false)
        .verify() // pop success marker (1)
        .drop_(); // drop count

    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000)
        .expect("prover always builds something");
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = result;
    let proof = proof.expect("proof set");

    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify_with_cells(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
        &cells,
    )
    .expect_err("verify must reject — child's unsat constraint inherited cleanly");
    assert!(matches!(err, VMError::InvalidR1CSProof));
}

/// **Full-coverage rollback canary.** A failed `open` whose child
/// touched *every* rollback-tracked lane must leave the caller
/// observably untouched. The child does:
///
///   1. `output` — emits `TxEntry::Output` (lane: TxLog truncate).
///   2. `send`   — emits `TxEntry::Send`   (lane: TxLog truncate).
///   3. `contract` + `signtx` — records a `DeferredSig::TxBound`
///      (lane: deferred_sigs truncate).
///   4. MSM `verify` with a non-identity statement (lane: batch
///      rollback via `BatchCheckpoint::restore`).
///   5. R1CS `alloc / eq / verify` with an unsatisfiable equality
///      (lane: CS rollback via `r1cs::CheckpointableConstraintSystem::
///      rollback`).
///   6. `verify(0)` — forces the frame to unwind into a `0` marker.
///
/// After the failure rolls back, the outer script does its own
/// satisfiable proof. Prover→Verifier must accept; the TxLog must
/// contain only `Header + Input` (the `Input` is `open_with_inner`'s
/// own anchor-seeding step, *before* the failed `open`); no
/// `TxBound` deferred sig must survive (so we pass `None` as the
/// envelope signature).
#[test]
fn failed_call_rolls_back_every_state_lane() {
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED;
    let pc_gens = PedersenGens::default();
    let g_bytes = *RISTRETTO_BASEPOINT_COMPRESSED.as_bytes();

    let inner = ScriptBuilder::new()
        // ── lane 1: TxLog (Output) ──────────────────────────────
        .push_int(42u64)
        .push_point([0xbb; 32])
        .output()
        // ── lane 2: TxLog (Send) ────────────────────────────────
        .push_int(0u64) // k=0 args
        .push_str(String::from(vec![0u8; 32])) // refund (32 B)
        .push_int(1u64) // gas
        .push_str(String::from(vec![0xcc; 32])) // addr (32 B)
        .send()
        // ── lane 3: deferred_sigs (signtx records TxBound) ──────
        .push_int(0u64)
        .dict() // one empty Dict payload
        .push_point([0xdd; 32]) // predicate
        .contract() // → Contract on stack
        .signtx() // returns the single payload; records TxBound
        .drop_()
        // ── lane 4: MSM/sig batch (non-identity 1·G) ────────────
        .push_int(1u64)
        .push_point(g_bytes)
        .mul()
        .verify() // appends 1·G to batch
        // ── lane 5: R1CS (unsatisfiable 7+3==99) ────────────────
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(99u64)))
        .eq()
        .verify()
        // ── deliberately fail ───────────────────────────────────
        .push_int(0u64)
        .verify() // VerifyFailed → frame unwinds
        .push_int(0u64)
        .return_(); // unreachable

    let outer = open_with_inner(inner, true)
        // Parent's own satisfiable constraint: 7 + 3 == 10.
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(10u64)))
        .eq()
        .verify();

    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000).expect("prove ok");
    let txid_p = result.txid;
    assert_eq!(
        result.multiplications, 2,
        "failed child's multiplication gates must be rolled back"
    );

    // ── TxLog assertion: rollback truncated all child entries ──
    // After the failure, the only entries left are Header (always)
    // and Input (emitted by `open_with_inner`'s anchor-seeding
    // `input` call BEFORE the failing open). The child's Output +
    // Send would have made this length 4.
    assert_eq!(
        result.txlog.len(),
        2,
        "txlog must be [Header, Input] after rollback (got len={}, entries={:?})",
        result.txlog.len(),
        result.txlog,
    );
    assert!(matches!(result.txlog[0], TxEntry::Header(_)));
    assert!(matches!(result.txlog[1], TxEntry::Input(_)));

    // ── deferred_sigs assertion: signtx's TxBound was rolled back ──
    assert!(
        result.deferred_sigs.is_empty(),
        "deferred_sigs must be empty after rollback (got {} entries)",
        result.deferred_sigs.len(),
    );

    // ── end-to-end roundtrip: verifier must accept ──
    // If ANY lane wasn't rolled back:
    //   - txlog: TxID changes (both sides see the entries, so this
    //     wouldn't fail via TxID mismatch — but the test above
    //     catches the leak directly).
    //   - deferred_sigs: verifier wants a TxBound signature for the
    //     leftover signtx → `MissingTxBoundSignature` since we pass
    //     `None`.
    //   - batch: 1·G != identity → `BatchSignatureVerificationFailed`.
    //   - CS: 7+3==99 unsatisfiable → `InvalidR1CSProof`.
    let TxResult {
        bytecode,
        proof,
        cells,
        ..
    } = result;
    let proof = proof.expect("proof set");
    let verifier_result = Verifier::verify_with_cells(
        &pc_gens,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None, // no txbound sig — if a TxBound leaked, verify rejects with MissingTxBoundSignature,
        &cells,
    )
    .expect("verify must accept — every rollback lane fired");
    assert_eq!(verifier_result.multiplications, 2);

    // TxID must match between prover and verifier (both ran the
    // same script through the same rollback sites).
    assert_eq!(verifier_result.txid, txid_p);
}
