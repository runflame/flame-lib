//! Tests for constraints.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn range_proof_accepts_in_range_value() {
    // alloc(42) push:64 range — 42 fits in 64 bits.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(42u64)))
        .push_int(64u64)
        .range()
        // Constrain that the same alloc equals 42 to close the proof
        // with a non-trivial constraint (so verification has
        // something to check beyond the range gadget).
        .alloc(Some(Int253::from(42u64)))
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
fn range_proof_rejects_out_of_range_value() {
    // alloc(2^9) push:8 range — 512 does NOT fit in 8 bits, so the
    // prover-side range_proof gadget rejects the witness or the
    // verifier rejects the proof.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(512u64)))
        .push_int(8u64)
        .range()
        .alloc(Some(Int253::from(512u64)))
        .eq()
        .verify();
    let result = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    );
    // The prover may succeed (constructs a proof with bad witness)
    // and the verifier rejects, OR the prover errors directly.
    // Either way, the full pipeline must reject. Cover both
    // outcomes for robustness.
    match result {
        Err(_) => {
            // Prover refused — good.
        }
        Ok(_pp) => {
            let TxResult { bytecode, proof, .. } = _pp;
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
    let program = Program::new()
        .alloc(Some(Int253::from(0u64)))
        .push_int(0u64)
        .range()
        .alloc(Some(Int253::from(0u64)))
        .eq()
        .verify();
    let err = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, VMError::BitCountOutOfRange));
}

#[test]
fn range_bit_count_above_64_rejected() {
    // push:65 — bit count exceeds BitRange::max() (64).
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(1u64)))
        .push_int(65u64)
        .range()
        .alloc(Some(Int253::from(1u64)))
        .eq()
        .verify();
    let err = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, VMError::BitCountOutOfRange));
}

#[test]
fn constraint_and_overload_combines_two_constraints() {
    // (alloc(7) == alloc(7)) AND (alloc(3) == alloc(3))
    //   → Constraint composition — verify succeeds (both true).
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        // Constraint 1: alloc(7) == alloc(7) — pushes Constraint
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(7u64)))
        .eq()
        // Constraint 2: alloc(3) == alloc(3) — pushes Constraint
        .alloc(Some(Int253::from(3u64)))
        .alloc(Some(Int253::from(3u64)))
        .eq()
        // AND the two Constraints
        .and()
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
fn constraint_or_overload_combines_two_constraints() {
    // (alloc(7) == alloc(8)) OR (alloc(3) == alloc(3))
    //   → first is false, second is true; OR yields true. Verify ok.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(8u64)))
        .eq()
        .alloc(Some(Int253::from(3u64)))
        .alloc(Some(Int253::from(3u64)))
        .eq()
        .or()
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
fn constraint_not_overload_negates_constraint() {
    // NOT (alloc(7) == alloc(8))  → NOT false → true.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(8u64)))
        .eq()
        .not()
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
fn constraint_and_with_false_branch_rejected() {
    // (alloc(7) == alloc(7)) AND (alloc(3) == alloc(99))
    //   → first true, second false; AND is false. Verifier rejects.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(7u64)))
        .eq()
        .alloc(Some(Int253::from(3u64)))
        .alloc(Some(Int253::from(99u64)))
        .eq()
        .and()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove succeeds (constructs proof of unsatisfiable constraint)");
    let crate::vm::TxResult { bytecode, proof, .. } = _pp;
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
    .unwrap_err();
    assert!(matches!(err, VMError::InvalidR1CSProof));
}

#[test]
fn range_in_internal_context_errors_external_only() {
    // Internal context dispatches `range` to ExternalOnly.
    let mut vm = vm_with_script(
        Program::new().push_int(1u64).push_int(64u64).range().to_bytecode(),
    );
    // Push an Expression manually so dispatch_internal hits range.
    // Actually we can't construct an Expression in internal context
    // (alloc is ExternalOnly too). The simpler test: just step until
    // the `range` opcode is dispatched — it should error ExternalOnly
    // before consuming any stack operands.
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

// ── CS rollback on call failure ─────────────────────────────────

/// Wraps `inner` in an `open` of a single-leaf cell that consumes
/// itself (`input` then `open`) so the script runs under a real
/// `last_anchor`. The outer program returns the inner's failure
/// or success marker on the stack for the caller's continuation.
fn open_with_inner(inner: Program) -> Program {
    let inner_bytes = inner.to_bytecode();
    let tree = PredicateTree::scripts_only(
        vec![inner_bytes.clone()],
        TEST_BLINDING_KEY,
    )
    .expect("scripts_only tree");
    let cp = tree.callproof_for(0).expect("cp");
    let pred_point = tree.compute_point();
    let cell = Cell::new(Predicate::Opaque(pred_point), Anchor([0xa1; 32]), vec![]);
    let cell_bytes = encode_cell_to_bytes(&cell);

    let mut outer = Program::new()
        .push_str(String::from(cell_bytes))
        .input()
        .push_point(*cp.internal_key.as_bytes());
    for (i, h) in cp.neighbors.iter().enumerate() {
        outer = outer
            .push_str(String::from(h.to_vec()))
            .push_int(i as u64);
    }
    outer
        .push_int(cp.neighbors.len() as u64)
        .dict()
        .push_str(String::from(cp.position.clone()))
        .push_script(inner)
        .push_int(1024u64)
        .push_int(1024u64)
        .push_int(0u64)
        .open()
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
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(99u64)))
        .eq()
        .verify()                                  // unsat constraint into CS
        .push_int(0u64)
        .verify()                                  // VerifyFailed → unwind
        .push_int(0u64)
        .return_();
    let outer = open_with_inner(inner)
        .drop_()                                   // discard `0` failure marker
        // Parent's own constraint: 7 + 3 == 10 — satisfiable.
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .expect("prove ok");
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");

    // Verifier must accept: the child's `7+3==99` was rolled back
    // out of the CS; only the parent's satisfiable `7+3==10`
    // remains.
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
    .expect("verify must accept — failed call's CS contributions rolled back");
}

/// Inverse: when the cell-open succeeds cleanly, its allocations
/// and constraints stay in the CS. If the child's constraint is
/// unsatisfiable, the verifier rejects — confirming the rollback
/// only fires on the failure path.
#[test]
fn clean_call_cs_alloc_propagates_to_parent_proof() {
    let pc_gens = PedersenGens::default();
    // Child: adds an UNSATISFIABLE constraint and returns cleanly.
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(99u64)))
        .eq()
        .verify()
        .push_int(0u64)
        .return_();
    let outer = open_with_inner(inner)
        .verify()                                  // pop success marker (1)
        .drop_();                                  // drop count

    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .expect("prover always builds something");
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
    .expect_err("verify must reject — child's unsat constraint inherited cleanly");
    assert!(matches!(err, VMError::InvalidR1CSProof));
}

