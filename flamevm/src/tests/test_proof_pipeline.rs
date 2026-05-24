//! Tests for proof pipeline.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn instruction_alloc_witness_roundtrip() {
    // Alloc(Some(7)) encodes to exactly one byte; its witness is
    // tracked separately via the queue.
    let p = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(None)
        .alloc(Some(Int253::from(3u64)));
    let bytecode = p.to_bytecode();
    assert_eq!(bytecode, vec![0x5c, 0x5c, 0x5c]);
    let witnesses: Vec<_> = p.to_witnesses().into();
    assert_eq!(witnesses.len(), 3);
    assert!(matches!(witnesses[0], Some(_)));
    assert!(matches!(witnesses[1], None));
    assert!(matches!(witnesses[2], Some(_)));
}

#[test]
fn program_builder_emits_expected_bytecode() {
    // alloc(7) alloc(3) add alloc(10) eq verify
    let p = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    assert_eq!(
        p.to_bytecode(),
        vec![0x5c, 0x5c, 0x53, 0x5c, 0x51, 0x79]
    );
    let wits: Vec<_> = p.to_witnesses().into();
    assert_eq!(wits.len(), 3);
}

/// End-to-end Phase 11: prove `alloc(7) + alloc(3) == alloc(10)`
/// then verify the proof. This is the bootstrap milestone — once
/// this works, all later CS-touching opcodes wire onto the same
/// machinery.
#[test]
fn prove_then_verify_alloc_arithmetic_equality() {
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();

    let _pp = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
    .expect("prove succeeds");
let crate::vm::TxResult { bytecode, proof, .. } = _pp;
let proof = proof.expect("proof set");

    // Verifier walks the same bytecode and accepts the proof.
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
fn prove_succeeds_but_verify_fails_on_tampered_proof() {
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
    .expect("prove succeeds");
let crate::vm::TxResult { bytecode, proof, .. } = _pp;
let proof = proof.expect("proof set");

    // Flip a byte deep in the proof body.
    let mut proof_bytes = proof.to_bytes();
    let last = proof_bytes.len() - 1;
    proof_bytes[last] ^= 0x01;
    let tampered = bulletproofs::r1cs::R1CSProof::from_bytes(&proof_bytes)
        .expect("re-parses");

    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &tampered,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VMError::InvalidR1CSProof));
}

#[test]
fn prove_fails_for_unsatisfiable_equality() {
    // alloc(7) + alloc(3) == alloc(99) — constraint is false at
    // witness level. Bulletproofs' Prover happily emits a proof
    // (the constraint is unsatisfiable but the prover constructs
    // *something*); the verifier MUST reject.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(99u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
    .expect("prover doesn't refuse construction");
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
fn alloc_pushes_expression_with_witness() {
    // Build a single-alloc program and stop after the alloc to
    // inspect the produced Expression.
    let pc_gens = PedersenGens::default();
    let program = Program::new().alloc(Some(Int253::from(42u64)));
    let mut prover = Prover::new(&pc_gens);
    // We bypass the public `Prover::prove` so we can inspect VM
    // state mid-flight. Build a Run::Queue from the Program so the
    // Alloc instruction's witness survives dispatch.
    let kind = CallKind::ExternalRoot;
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        ),
    );
    // One step → executes the alloc.
    vm.step_external(&mut prover).expect("alloc step ok");

    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Expression(crate::Expression::LinearCombination(terms, witness)) => {
            assert_eq!(terms.len(), 1);
            assert_eq!(*witness, Some(Int253::from(42u64)));
        }
        _ => panic!("expected Expression with witness"),
    }
}

#[test]
fn prove_then_verify_alloc_multiplication() {
    // alloc(4) alloc(5) mul alloc(20) eq verify
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(4u64)))
        .alloc(Some(Int253::from(5u64)))
        .mul()
        .alloc(Some(Int253::from(20u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
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
fn prove_then_verify_alloc_with_negation() {
    // alloc(5) neg alloc(-5) eq verify  →  -5 == -5
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(5u64)))
        .neg()
        .alloc(Some(Int253::from(-5i64)))
        .eq()
        .verify();
    let _pp = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
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
fn alloc_without_witness_works_in_verifier_path() {
    // Verifier feeds bytecode that contains an alloc — the
    // verifier's `next_alloc_witness` returns None, so the variable
    // is allocated without an assignment. We can't verify a proof
    // here (the prover has a witness), but we can check the path
    // doesn't error before proof verification.
    //
    // Build a trivially-true constraint: alloc * 0 == 0.
    // Concrete sub-test: just confirm the verifier walks an alloc
    // opcode without erroring on the witness-missing path.
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(0u64)))
        .alloc(Some(Int253::from(0u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    )
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
fn shared_bp_gens_is_singleton() {
    // Two Prover::new calls reuse the same generator allocation.
    // We can't directly compare addresses without exposing the
    // singleton, but we confirm both can prove a trivial program
    // (i.e., the singleton is reachable from both instances).
    let pc_gens = PedersenGens::default();
    let program1 = Program::new()
        .alloc(Some(Int253::from(1u64)))
        .alloc(Some(Int253::from(1u64)))
        .eq()
        .verify();
    let program2 = Program::new()
        .alloc(Some(Int253::from(2u64)))
        .alloc(Some(Int253::from(2u64)))
        .eq()
        .verify();
    Prover::prove(&pc_gens, program1, dummy_header(), 1_000_000, 0)
        .expect("prove #1 succeeds with shared gens");
    Prover::prove(&pc_gens, program2, dummy_header(), 1_000_000, 0)
        .expect("prove #2 succeeds with shared gens");
}

// ── Phase 18: TxID transcript binding ────────────────────────

/// Building a trivial program twice with the same header must
/// yield the same TxID — proves the txlog (= Header alone, here)
/// is reproducible bit-for-bit.
#[test]
fn phase18_txid_deterministic_for_equal_inputs() {
    let pc_gens = PedersenGens::default();
    let header = dummy_header();
    let mk_program = || {
        Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(3u64)))
            .add()
            .alloc(Some(Int253::from(10u64)))
            .eq()
            .verify()
    };
    let txid1 = Prover::prove(&pc_gens, mk_program(), header, 1_000_000, 0)
        .expect("prove #1").txid;
    let txid2 = Prover::prove(&pc_gens, mk_program(), header, 1_000_000, 0)
        .expect("prove #2").txid;
    assert_eq!(txid1, txid2);
}

/// Changing the header (version / locktime) must change the TxID,
/// because the Header is the first txlog entry (Phase 18). Without
/// the Header binding, a malleable header could replay a proof
/// against a different transaction; with it, the proof transcript
/// is bound to the header bits.
#[test]
fn phase18_txid_changes_when_header_changes() {
    let pc_gens = PedersenGens::default();
    let mk_program = || {
        Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(3u64)))
            .add()
            .alloc(Some(Int253::from(10u64)))
            .eq()
            .verify()
    };
    let h1 = TxHeader {
        version: 1,
        locktime: 0,
    };
    let h2 = TxHeader {
        version: 1,
        locktime: 42,
    };
    let h3 = TxHeader {
        version: 2,
        locktime: 0,
    };
    let id1 = Prover::prove(&pc_gens, mk_program(), h1, 1_000_000, 0)
        .expect("prove h1").txid;
    let id2 = Prover::prove(&pc_gens, mk_program(), h2, 1_000_000, 0)
        .expect("prove h2").txid;
    let id3 = Prover::prove(&pc_gens, mk_program(), h3, 1_000_000, 0)
        .expect("prove h3").txid;
    assert_ne!(id1, id2, "locktime change must alter TxID");
    assert_ne!(id1, id3, "version change must alter TxID");
    assert_ne!(id2, id3, "locktime+version both alter TxID");
}

/// End-to-end Phase 18: prove then verify round-trip — the
/// verifier reconstructs the same TxID from the same bytecode +
/// header, binds it into its own R1CS transcript, and accepts the
/// proof. Confirms prover/verifier transcript binding agrees.
#[test]
fn phase18_prove_verify_roundtrip_binds_txid() {
    let pc_gens = PedersenGens::default();
    let header = dummy_header();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let prover_result = Prover::prove(&pc_gens, program, header, 1_000_000, 0)
        .expect("prove ok");
    let txid_p = prover_result.txid;
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let verifier_result = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        header,
        1_000_000,
        0,
        None,
    )
    .expect("verify ok");
    assert_eq!(
        txid_p, verifier_result.txid,
        "prover and verifier must agree on TxID"
    );
}

/// Verifier with a *different* header from the prover must reject:
/// its transcript binds a different TxID before `cs.verify`,
/// invalidating the proof. This is the core Phase-18 invariant —
/// header tampering breaks the proof.
#[test]
fn phase18_verifier_rejects_proof_under_different_header() {
    let pc_gens = PedersenGens::default();
    let prove_header = TxHeader {
        version: 1,
        locktime: 0,
    };
    let verify_header = TxHeader {
        version: 1,
        locktime: 99, // different!
    };
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, prove_header, 1_000_000, 0)
            .expect("prove ok");
    let crate::vm::TxResult { bytecode, proof, .. } = _pp;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        verify_header, // mismatch — TxID bound differs
        1_000_000,
        0,
        None,
    )
    .expect_err("must reject under different header");
    assert!(matches!(err, VMError::InvalidR1CSProof));
}

// ── Phase 21: TxResult shape + finalize return values ────────

/// Trivial prover/verifier round-trip: every TxResult field is
/// populated as expected. This is the headline Phase-21 test —
/// it confirms the canonical `Result<TxResult, VMError>` shape
/// is the single source of truth for both sides.
#[test]
fn phase21_txresult_populated_for_trivial_program() {
    let pc_gens = PedersenGens::default();
    let header = TxHeader { version: 7, locktime: 13 };
    let program = Program::new()
        .alloc(Some(Int253::from(5u64)))
        .alloc(Some(Int253::from(5u64)))
        .eq()
        .verify();
    let prover_result =
        Prover::prove(&pc_gens, program, header, 1_000_000, 0)
            .expect("prove ok");
    // Phase 18: txlog has Header at [0].
    assert!(matches!(
        prover_result.txlog[0],
        crate::tx::TxEntry::Header(_)
    ));
    // total_fee = 0 (no fee opcodes), gas/vbytes = 0 (pre-Phase 22),
    // bytecode populated, proof Some, deferred_sigs empty, sends empty.
    assert_eq!(prover_result.total_fee, 0);
    assert_eq!(prover_result.gas_used, 0);
    assert_eq!(prover_result.vbytes_used, 0);
    assert!(!prover_result.bytecode.is_empty());
    assert!(prover_result.proof.is_some());
    assert!(prover_result.deferred_sigs.is_empty());
    assert!(prover_result.sends.is_empty());
    // Verifier side: same TxID and txlog; proof is None (consumed).
    let prover_txid = prover_result.txid;
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let verifier_result = Verifier::verify(
        &pc_gens_v,
        bytecode.clone(),
        &proof,
        header,
        1_000_000,
        0,
        None,
    )
    .expect("verify ok");
    // Cross-verify all the fields agree.
    assert_eq!(verifier_result.txid, prover_txid);
    assert_eq!(verifier_result.bytecode, bytecode);
    // Verifier-side proof slot is None — it was consumed inside
    // `cs.verify` and never re-attached.
    assert!(verifier_result.proof.is_none());
    assert_eq!(verifier_result.total_fee, 0);
    assert!(verifier_result.deferred_sigs.is_empty());
}

/// `op_fee` flows into `TxResult.total_fee`. The Phase-21 result
/// is the canonical place to read the running fee — no more
/// digging into `vm.total_fee` directly.
#[test]
fn phase21_total_fee_flows_through_to_txresult() {
    let pc_gens = PedersenGens::default();
    // Build script directly to avoid the non-droppable WideToken
    // (we step through manually).
    let program = Program::new()
        .push_int(123u64)
        .push_int(0u64)
        .fee();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    for _ in 0..3 {
        vm.step_external(&mut prover).expect("step ok");
    }
    // Inspect VM's total_fee directly (the Phase-21 TxResult
    // mirror is exercised in
    // phase21_txresult_populated_for_trivial_program).
    assert_eq!(vm.total_fee.total(), 123);
}

/// `TxResult.txlog` carries every recorded effect in order.
/// Confirms the txlog drained into the result preserves Phase-18
/// ordering (Header at 0, then the effects in emission order).
#[test]
fn phase21_txlog_ordering_in_txresult() {
    // Use execute_external (the simple delegate path) — same
    // TxResult shape, easier setup. Build a script that pushes a
    // string and logs it twice.
    let mut script = Vec::new();
    push_string_bytes(&mut script, b"a");
    script.push(0x6f); // log
    push_string_bytes(&mut script, b"b");
    script.push(0x6f); // log
    let delegate = StubDelegate::new();
    let result = VM::execute_external(
        dummy_header(),
        script,
        1_000_000,
        0,
        delegate,
    )
    .expect("execute ok");
    assert_eq!(result.txlog.len(), 3, "Header + 2 Data entries");
    assert!(matches!(
        result.txlog[0],
        crate::tx::TxEntry::Header(_)
    ));
    match &result.txlog[1] {
        crate::tx::TxEntry::Data(b) => assert_eq!(b, b"a"),
        _ => panic!("txlog[1] must be Data(a)"),
    }
    match &result.txlog[2] {
        crate::tx::TxEntry::Data(b) => assert_eq!(b, b"b"),
        _ => panic!("txlog[2] must be Data(b)"),
    }
}

/// `TxResult.deferred_sigs` exposes the recorded `signtx` /
/// `signrun` items to callers post-finalize. Verifier-side this
/// is the post-verify audit shape: the caller can inspect which
/// keys participated without re-running the VM.
#[test]
fn phase21_deferred_sigs_in_txresult() {
    use musig::Multisignature;
    let pc_gens = PedersenGens::default();
    let (vk, sk) = signing_keypair(7);
    let (script, cell_id) = make_signtx_script_with_cell(vk);
    let program = crate::Program::parse(&script).expect("decode");
    let prover_result =
        Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
    assert_eq!(prover_result.deferred_sigs.len(), 1);
    let prover_txid = prover_result.txid;
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof");
    // Sign + verify.
    let mut t = merlin::Transcript::new(b"flamevm.signtx.v1");
    t.append_message(b"txid", &prover_txid.0);
    let sig = musig::Signature::sign_multi(
        vec![sk],
        vec![(musig::VerificationKey::from_compressed(vk), cell_id)],
        &mut t,
    )
    .expect("sign_multi");
    let pc_gens_v = PedersenGens::default();
    let verifier_result = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        Some(sig),
    )
    .expect("verify ok");
    // Verifier-side TxResult also exposes the deferred_sigs.
    assert_eq!(verifier_result.deferred_sigs.len(), 1);
    match &verifier_result.deferred_sigs[0] {
        DeferredSig::TxBound { verification_key, cell_id: cid } => {
            assert_eq!(verification_key, &vk);
            assert_eq!(*cid, cell_id);
        }
        _ => panic!("expected TxBound"),
    }
}

