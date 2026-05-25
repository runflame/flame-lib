//! Tests for authorization.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn signtx_pours_payload_and_records_txbound_sig() {
    // Build cell with payload [5, 7], then signtx.
    let mut script = vec![0x05, 0x07, 0x02];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell
    script.push(0x98); // signtx
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Stack now has [5, 7, count=2].
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Int253::from(2u64));
    // Exactly one TxBound deferred sig recorded. No message, no sig
    // bytes — those come from the tx envelope at finalize.
    assert_eq!(vm.deferred_sigs.len(), 1);
    match &vm.deferred_sigs[0] {
        DeferredSig::TxBound { verification_key, .. } => {
            assert_eq!(verification_key.as_bytes(), &[0xaa; 32]);
        }
        DeferredSig::Explicit { .. } => panic!("expected TxBound, got Explicit"),
    }
}

#[test]
fn signcall_records_explicit_sig_and_runs_program() {
    // Inner script `drop, push:0, return` drains the payload and
    // exits the isolated CellOpen frame (ADR 0013).
    let prog = vec![0x1c, 0x00, 0x7e];
    let sig_bytes = [0u8; 64];

    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell
    push_string_bytes(&mut script, &prog);
    push_string_bytes(&mut script, &sig_bytes);
    push_open_gas_bytes(&mut script);
    script.push(0x00); // m=0 args
    script.push(0x99); // signcall
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
    assert_eq!(vm.deferred_sigs.len(), 1);
    match &vm.deferred_sigs[0] {
        DeferredSig::Explicit {
            verification_key,
            message,
            signature,
        } => {
            assert_eq!(verification_key.as_bytes(), &[0xaa; 32]);
            assert_eq!(signature, &sig_bytes);
            // message must be the program-only Merlin transcript output.
            assert_eq!(message.len(), 32);
        }
        DeferredSig::TxBound { .. } => panic!("expected Explicit, got TxBound"),
    }
}

#[test]
fn signcall_message_binds_only_to_program_not_to_cell() {
    // Two different cells running the same program produce
    // identical deferred-sig messages — confirms architect's
    // intent that signcall binds only to the program.
    fn run_signcall(predicate_byte: u8) -> DeferredSig {
        let prog = vec![0x1c, 0x00, 0x7e];
        let sig = [0u8; 64];
        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, &[predicate_byte; 32]);
        script.push(0x91);
        push_string_bytes(&mut script, &prog);
        push_string_bytes(&mut script, &sig);
        push_open_gas_bytes(&mut script);
        script.push(0x00);
        script.push(0x99);
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        vm.deferred_sigs.into_iter().next().unwrap()
    }
    let s1 = run_signcall(0xaa);
    let s2 = run_signcall(0xbb);
    let (m1, m2) = match (&s1, &s2) {
        (
            DeferredSig::Explicit { message: m1, .. },
            DeferredSig::Explicit { message: m2, .. },
        ) => (m1.clone(), m2.clone()),
        _ => panic!("expected Explicit on both"),
    };
    assert_eq!(m1, m2, "signcall message must be program-only");
}

#[test]
fn signcall_rejects_wrong_signature_length() {
    // payload(5), count(1), predicate, cell, prog, bad-sig, gas/bytes,
    // m=0, signcall. signcall pops bad sig and errors before frame.
    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell
    push_string_bytes(&mut script, &[0x1c, 0x00, 0x7e]); // prog
    push_string_bytes(&mut script, &[0u8; 63]); // sig of wrong length
    push_open_gas_bytes(&mut script);
    script.push(0x00);
    script.push(0x99);
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::BadSignatureBytes
    ));
}

#[test]
fn signcall_explicit_sig_batch_verifies_correctly() {
    // Phase-14 unit test for the Explicit-deferred-sig batch path
    // that `Verifier::verify` uses. Builds a real signature over
    // the signcall-message transcript and runs it through the same
    // batch-verification logic as the verifier.
    //
    // (Full prove+verify-via-script path is covered by
    // `test_proof_pipeline`; this test isolates the batch check.)
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
    use curve25519_dalek::scalar::Scalar;
    let sk = Scalar::from(42u64);
    let vk_point = (&sk * &RISTRETTO_BASEPOINT_TABLE).compress();
    // Build the message exactly as `op_signcall`'s
    // `signcall_message(program)` helper does.
    let inner_prog = vec![0x1c]; // drop
    let mut prog_t = merlin::Transcript::new(b"flamevm.signcall.v1");
    prog_t.append_message(b"program", &inner_prog);
    let mut msg_bytes = vec![0u8; 32];
    prog_t.challenge_bytes(b"msg", &mut msg_bytes);
    // Sign over a transcript with that message appended (matches
    // `Verifier::verify`'s reconstruction).
    let mut sign_t = merlin::Transcript::new(b"flamevm.signcall.v1");
    sign_t.append_message(b"msg", &msg_bytes);
    let signature = musig::Signature::sign(&mut sign_t, sk);
    // Verifier-side batch check.
    let mut batch = musig::BatchVerifier::new(rand::thread_rng());
    let mut t = merlin::Transcript::new(b"flamevm.signcall.v1");
    t.append_message(b"msg", &msg_bytes);
    let vk = musig::VerificationKey::from_compressed(vk_point);
    signature.verify_batched(&mut t, vk, &mut batch);
    assert!(batch.verify().is_ok());
}

#[test]
fn signcall_tampered_sig_batch_rejects() {
    // Verify that a tampered sig fails the batch check (the same
    // path that Verifier::verify uses for Explicit sigs).
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
    use curve25519_dalek::scalar::Scalar;
    let sk = Scalar::from(42u64);
    let vk_point = (&sk * &RISTRETTO_BASEPOINT_TABLE).compress();
    let inner_prog = vec![0x1c];
    let mut prog_t = merlin::Transcript::new(b"flamevm.signcall.v1");
    prog_t.append_message(b"program", &inner_prog);
    let mut msg_bytes = vec![0u8; 32];
    prog_t.challenge_bytes(b"msg", &mut msg_bytes);
    let mut sign_t = merlin::Transcript::new(b"flamevm.signcall.v1");
    sign_t.append_message(b"msg", &msg_bytes);
    let sig = musig::Signature::sign(&mut sign_t, sk);
    // Tamper: flip a bit in the signature's `s` scalar.
    let mut bytes = sig.to_bytes();
    bytes[63] ^= 0x01;

    let mut batch = musig::BatchVerifier::new(rand::thread_rng());
    let tampered = musig::Signature::from_bytes(bytes)
        .or_else(|_| {
            // If from_bytes rejects (non-canonical scalar), build by hand.
            Ok::<_, ()>(musig::Signature {
                R: sig.R,
                s: sig.s + Scalar::one(),
            })
        })
        .unwrap();
    let mut t = merlin::Transcript::new(b"flamevm.signcall.v1");
    t.append_message(b"msg", &msg_bytes);
    let vk = musig::VerificationKey::from_compressed(vk_point);
    tampered.verify_batched(&mut t, vk, &mut batch);
    assert!(batch.verify().is_err());
}

/// Single-TxBound happy path: build a tx with one `signtx`,
/// externally sign the multi-message `(vk, cell_id)` against a
/// transcript bound to TxID, pass to Verifier::verify → accepts.
#[test]
fn phase20_single_txbound_verifies_with_multisig() {
    use musig::Multisignature;
    let pc_gens = PedersenGens::default();
    let (vk, sk) = signing_keypair(101);
    let (script, cell_id) = make_signtx_script_with_cell(vk);
    let program = crate::Program::parse(&script).expect("decode");
    let prover_result =
        Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
    let txid = prover_result.txid;
    // Sanity: deferred_sigs has one TxBound with the expected cell_id.
    assert_eq!(prover_result.deferred_sigs.len(), 1);
    match &prover_result.deferred_sigs[0] {
        DeferredSig::TxBound { verification_key, cell_id: cid } => {
            assert_eq!(verification_key, &vk);
            assert_eq!(*cid, cell_id);
        }
        _ => panic!("expected TxBound"),
    }
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");
    // Sign multi-message context bound to TxID.
    let mut t = merlin::Transcript::new(b"flamevm.signtx.v1");
    t.append_message(b"txid", &txid.0);
    let items =
        vec![(musig::VerificationKey::from_compressed(vk), cell_id)];
    let sig = musig::Signature::sign_multi(vec![sk], items, &mut t)
        .expect("sign_multi");
    // Verifier accepts.
    let pc_gens_v = PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        Some(sig),
    )
    .expect("verify ok");
}

/// Two-key multi-sig: build a tx that consumes TWO cells via
/// signtx, each under its own key. The aggregate `sign_multi`
/// over `[(vk1, cell1_id), (vk2, cell2_id)]` produces a single
/// signature accepted by Verifier::verify.
#[test]
fn phase20_two_txbound_verifies_with_multisig() {
    use musig::Multisignature;
    let pc_gens = PedersenGens::default();
    let (vk1, sk1) = signing_keypair(11);
    let (vk2, sk2) = signing_keypair(22);

    let cell1 = Cell::new(
        Predicate::Opaque(vk1),
        Anchor([0xa1; 32]),
        vec![Value::Int253(Int253::from(0u64))],
    );
    let cell1_id = cell1.id();
    let cell1_bytes = encode_cell_to_bytes(&cell1);

    let cell2 = Cell::new(
        Predicate::Opaque(vk2),
        Anchor([0xa2; 32]),
        vec![Value::Int253(Int253::from(0u64))],
    );
    let cell2_id = cell2.id();
    let cell2_bytes = encode_cell_to_bytes(&cell2);

    // Script: input cell1, signtx (drop payload+count), input cell2,
    // signtx (drop payload+count).
    let mut script = Vec::new();
    push_string_bytes(&mut script, &cell1_bytes);
    script.push(0x90);
    script.push(0x98);
    script.push(0x1c);
    script.push(0x1c);
    push_string_bytes(&mut script, &cell2_bytes);
    script.push(0x90);
    script.push(0x98);
    script.push(0x1c);
    script.push(0x1c);

    let program = crate::Program::parse(&script).expect("decode");
    let prover_result =
        Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
    assert_eq!(prover_result.deferred_sigs.len(), 2);
    let txid = prover_result.txid;
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");

    // Build the (vk, cell_id) list IN THE SAME ORDER the VM recorded
    // them — the multisig context order is consensus-fixed.
    let items = vec![
        (musig::VerificationKey::from_compressed(vk1), cell1_id),
        (musig::VerificationKey::from_compressed(vk2), cell2_id),
    ];
    let mut t = merlin::Transcript::new(b"flamevm.signtx.v1");
    t.append_message(b"txid", &txid.0);
    let sig = musig::Signature::sign_multi(vec![sk1, sk2], items, &mut t)
        .expect("sign_multi");

    let pc_gens_v = PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        Some(sig),
    )
    .expect("verify ok");
}

/// `signtx` items present but `txbound_signature = None` →
/// `MissingTxBoundSignature` before the proof is checked.
#[test]
fn phase20_missing_signature_when_txbound_present() {
    let pc_gens = PedersenGens::default();
    let (vk, _sk) = signing_keypair(7);
    let (script, _cell_id) = make_signtx_script_with_cell(vk);
    let program = crate::Program::parse(&script).expect("decode");
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
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
    assert!(matches!(err, VMError::MissingTxBoundSignature));
}

/// `txbound_signature` provided but VM emitted no `signtx` →
/// `SpuriousTxBoundSignature`. Guards against silently dropping
/// a passed signature on a tx that never required one.
#[test]
fn phase20_spurious_signature_when_no_txbound() {
    let pc_gens = PedersenGens::default();
    // Trivial program: alloc + alloc + add + alloc + eq + verify
    // — no input, no signtx, no TxBound deferred sigs.
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
    let crate::vm::TxResult { bytecode, proof, .. } = _pp;
    let proof = proof.expect("proof set");
    // Hand a real-looking signature anyway. Even a syntactically
    // valid signature must be rejected when the VM emitted no
    // TxBound items.
    let sig = musig::Signature {
        R: curve25519_dalek::ristretto::CompressedRistretto([0u8; 32]),
        s: Scalar::from(0u64),
    };
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        Some(sig),
    )
    .unwrap_err();
    assert!(matches!(err, VMError::SpuriousTxBoundSignature));
}

/// Wrong key signing → batch verification fails with
/// `BatchSignatureVerificationFailed` (NOT `InvalidR1CSProof` —
/// the proof still verifies). Confirms the multi-sig actually
/// participates in batch.verify().
#[test]
fn phase20_tampered_signature_rejected() {
    use musig::Multisignature;
    let pc_gens = PedersenGens::default();
    let (vk, _sk_real) = signing_keypair(101);
    // Sign with a *different* secret — vk doesn't correspond.
    let sk_wrong = Scalar::from(999u64);
    let (script, cell_id) = make_signtx_script_with_cell(vk);
    let program = crate::Program::parse(&script).expect("decode");
    let prover_result =
        Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
    let txid = prover_result.txid;
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");
    let items =
        vec![(musig::VerificationKey::from_compressed(vk), cell_id)];
    let mut t = merlin::Transcript::new(b"flamevm.signtx.v1");
    t.append_message(b"txid", &txid.0);
    let sig =
        musig::Signature::sign_multi(vec![sk_wrong], items, &mut t)
            .expect("sign_multi");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        Some(sig),
    )
    .unwrap_err();
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}

/// No-TxBound prove/verify round-trip with `None` signature
/// succeeds (regression: the new arg doesn't break the most
/// common path — non-signtx transactions). This is also what
/// every existing Phase-11 / Phase-18 test exercises.
#[test]
fn phase20_no_txbound_no_signature_roundtrip() {
    let pc_gens = PedersenGens::default();
    let program = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let _pp = Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");
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
    .expect("verify ok with None signature");
}

/// Signature over the *wrong TxID* (different header) → rejected.
/// Confirms TxID-binding in `flamevm.signtx.v1` is load-bearing:
/// a replay attack across headers fails.
#[test]
fn phase20_signature_over_wrong_txid_rejected() {
    use musig::Multisignature;
    let pc_gens = PedersenGens::default();
    let (vk, sk) = signing_keypair(101);
    let (script, cell_id) = make_signtx_script_with_cell(vk);
    let program = crate::Program::parse(&script).expect("decode");
    let header_prove = TxHeader { version: 1, locktime: 0 };
    let prover_result =
        Prover::prove(&pc_gens, program, header_prove, 1_000_000, 0)
            .expect("prove ok");
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");
    // Sign against a *different* TxID (some random 32 bytes).
    let wrong_txid = [0x99u8; 32];
    let items =
        vec![(musig::VerificationKey::from_compressed(vk), cell_id)];
    let mut t = merlin::Transcript::new(b"flamevm.signtx.v1");
    t.append_message(b"txid", &wrong_txid);
    let sig = musig::Signature::sign_multi(vec![sk], items, &mut t)
        .expect("sign_multi");
    let pc_gens_v = PedersenGens::default();
    let err = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        header_prove,
        1_000_000,
        0,
        Some(sig),
    )
    .unwrap_err();
    assert!(matches!(err, VMError::BatchSignatureVerificationFailed));
}

