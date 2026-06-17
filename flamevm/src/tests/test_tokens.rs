//! Tests for tokens.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn token_cleartext_constructor_packs_unblinded_commitments() {
    let t = make_cleartext_token(123, 7);
    assert_eq!(t.qty.assignment(), Some(Int253::from(123u64)));
    assert_eq!(t.flv.assignment(), Some(Int253::from(7u64)));
    // Witness uses zero blinding.
    let (_, b) = t.qty.witness().expect("open commitment");
    assert_eq!(b, Scalar::ZERO);
}

#[test]
fn token_is_noncopyable_and_nondroppable() {
    let v = Value::Token(make_cleartext_token(1, 2));
    assert!(!v.is_copyable(), "Token must not be copyable");
    assert!(!v.is_droppable(), "Token must not be droppable");
    assert!(v.is_portable(), "Token must be portable");
    assert!(matches!(v.try_clone(), Err(VMError::TypeNotCopyable)));
}

#[test]
fn cleartoken_zero_qty_is_droppable() {
    let v = Value::ClearToken(ClearToken::new(Int253::ZERO, Int253::from(7u64)));
    assert!(v.is_droppable());
}

#[test]
fn cleartoken_nonzero_qty_is_not_droppable() {
    let v = Value::ClearToken(ClearToken::new(Int253::from(1u64), Int253::from(7u64)));
    assert!(!v.is_droppable());
}

#[test]
fn cleartoken_negative_qty_is_non_portable() {
    let v = Value::ClearToken(ClearToken::new(Int253::from(-1i64), Int253::from(7u64)));
    assert!(!v.is_portable());
    // Still non-copyable.
    assert!(!v.is_copyable());
}

#[test]
fn cleartoken_positive_qty_is_portable() {
    let v = Value::ClearToken(ClearToken::new(Int253::from(5u64), Int253::from(7u64)));
    assert!(v.is_portable());
}

#[test]
fn cleartoken_merge_into_same_flavor_sums_qtys() {
    let a = ClearToken::new(Int253::from(3u64), Int253::from(7u64));
    let b = ClearToken::new(Int253::from(4u64), Int253::from(7u64));
    let c = a.merge_into(b).expect("same flavor merges");
    assert_eq!(c.qty(), Int253::from(7u64));
    assert_eq!(c.flv(), Int253::from(7u64));
}

#[test]
fn cleartoken_merge_into_mismatched_flavor_returns_originals() {
    let a = ClearToken::new(Int253::from(3u64), Int253::from(7u64));
    let b = ClearToken::new(Int253::from(4u64), Int253::from(8u64));
    let (a2, b2) = a.merge_into(b).expect_err("mismatch returns Err");
    assert_eq!(a2.qty(), Int253::from(3u64));
    assert_eq!(b2.qty(), Int253::from(4u64));
}

#[test]
fn cleartoken_split_within_qty() {
    let a = ClearToken::new(Int253::from(10u64), Int253::from(7u64));
    let (rem, b) = a.split(Int253::from(3u64)).expect("split ok");
    assert_eq!(rem.qty(), Int253::from(7u64));
    assert_eq!(rem.flv(), Int253::from(7u64));
    assert_eq!(b.qty(), Int253::from(3u64));
    assert_eq!(b.flv(), Int253::from(7u64));
}

#[test]
fn cleartoken_split_above_qty_returns_none() {
    let a = ClearToken::new(Int253::from(2u64), Int253::from(7u64));
    assert!(a.split(Int253::from(3u64)).is_none());
}

#[test]
fn cleartoken_split_negative_q_returns_none() {
    let a = ClearToken::new(Int253::from(5u64), Int253::from(7u64));
    assert!(a.split(Int253::from(-1i64)).is_none());
}

#[test]
fn cleartoken_negated_flips_qty_sign() {
    let a = ClearToken::new(Int253::from(5u64), Int253::from(7u64));
    let n = a.negated();
    assert_eq!(n.qty(), Int253::from(-5i64));
    assert_eq!(n.flv(), Int253::from(7u64));
}

#[test]
fn amount_on_cleartoken_pushes_qty_and_flv() {
    // Pre-load a ClearToken on the stack, run `amount`, verify the
    // shape `cleartoken(qty,flv) → cleartoken qty flv`.
    let mut vm = vm_with_script(ScriptBuilder::new().amount().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(11u64),
        Int253::from(22u64),
    )));
    vm.step_internal().expect("step ok");
    assert_eq!(vm.current_call.stack.len(), 3);
    // Bottom: the original cleartoken.
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => {
            assert_eq!(t.qty(), Int253::from(11u64));
            assert_eq!(t.flv(), Int253::from(22u64));
        }
        _ => panic!("bottom must be original ClearToken"),
    }
    // Middle: qty.
    assert_int(&vm.current_call.stack[1], Int253::from(11u64));
    // Top: flv.
    assert_int(&vm.current_call.stack[2], Int253::from(22u64));
}

#[test]
fn amount_on_token_pushes_points() {
    let mut vm = vm_with_script(ScriptBuilder::new().amount().to_bytecode());
    vm.push_value(Value::Token(make_cleartext_token(33, 44)));
    vm.step_internal().expect("step ok");
    assert_eq!(vm.current_call.stack.len(), 3);
    match &vm.current_call.stack[0] {
        Value::Token(_) => {}
        _ => panic!("bottom must be original Token"),
    }
    match &vm.current_call.stack[1] {
        Value::Point(_) => {}
        _ => panic!("middle must be Point (qty commitment)"),
    }
    match &vm.current_call.stack[2] {
        Value::Point(_) => {}
        _ => panic!("top must be Point (flv commitment)"),
    }
}

#[test]
fn amount_on_non_token_errors_typenottoken() {
    let mut vm = vm_with_script(ScriptBuilder::new().amount().to_bytecode());
    vm.push_value(Value::Int253(Int253::from(5u64)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::TypeNotToken));
    // Original value is restored on error.
    assert_eq!(vm.current_call.stack.len(), 1);
}

#[test]
fn issuepub_clear_path_emits_txlog_and_returns_cleartoken() {
    // Script: push:7, pushstr "gold", issuepub.
    // Run under InternalRoot with a known actor identity so
    // `op_issuepub` can resolve a flavor.
    let actor = ActorID::Hash([0x55; 32]);
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_str(String::from(b"gold".to_vec()))
        .issuepub()
        .to_bytecode();
    let mut vm = vm_internal_with_actor(script, actor.clone());
    run_to_end(&mut vm).expect("issuepub ok");

    // Stack: [ClearToken(7, flavor)].
    assert_eq!(vm.current_call.stack.len(), 1);
    let expected_flv =
        test_flavor_from_actor(&actor, &String::from(b"gold".to_vec()));
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => {
            assert_eq!(t.qty(), Int253::from(7u64));
            assert_eq!(t.flv(), expected_flv);
        }
        _ => panic!("expected ClearToken"),
    }

    // Txlog has Header + IssuePub entry with cleartext (qty, flv).
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::IssuePub(q, f) => {
            assert_eq!(*q, Int253::from(7u64));
            assert_eq!(*f, expected_flv);
        }
        _ => panic!("expected TxEntry::IssuePub"),
    }
}

#[test]
fn issuepub_with_point_qty_errors_typenotint253() {
    // `issuepub` is cleartext-only; non-Int253 qty hard-fails. Under
    // the new design (spec.md §issuepub), the confidential variant
    // lives in `issuepriv`, not in dispatch-peek on this opcode.
    let actor = ActorID::Hash([0x55; 32]);
    let script = ScriptBuilder::new()
        .push_point([0u8; 32])
        .push_str(String::from(b"gold".to_vec()))
        .issuepub()
        .to_bytecode();
    let mut vm = vm_internal_with_actor(script, actor);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TypeNotInt253));
}

#[test]
fn issuepub_at_external_root_errors_actor_context() {
    // ExternalRoot has no actor identity.
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_str(String::from(b"gold".to_vec()))
        .issuepub()
        .to_bytecode();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot, 1_000_000, 0, 0,
        ),
    );
    let mut delegate = make_stub_delegate();
    let err = drive_external(&mut vm, &mut delegate).unwrap_err();
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

// ── issuepriv tests ─────────────────────────────────────────────────

#[test]
fn issuepriv_emits_token_with_predicate_bound_flavor() {
    // Inside a CellOpen frame in external context: push a blinded
    // commitment as a witness-bearing String, lift it to a Variable
    // via `commit`, push a tag, run `issuepriv`. Then `push:1 return`
    // so the Token returns to the parent frame cleanly. The opcode
    // does:
    //   1. Register the qty commitment with the CS.
    //   2. Allocate a 64-bit range proof.
    //   3. Compute flavor = flavor_from_predicate(current_predicate, tag).
    //   4. Emit TxEntry::IssuePriv(qty_point, unblinded_flv_point).
    //   5. Push Token { qty, flv: unblinded(flv) }.
    use bulletproofs::PedersenGens;
    let pc_gens = PedersenGens::default();
    let qty_int = Int253::from(42u64);
    let qty_blind = curve25519_dalek::scalar::Scalar::from(11u64);
    let qty_commit = Commitment::blinded_with_factor(qty_int, qty_blind);
    let tag_str = String::from(b"gold".to_vec());

    // Build the VM by hand instead of going through bytecode — the
    // bytecode roundtrip would strip the prover-side commitment
    // witness from `String::commitment(...)`, leaving a `Closed`
    // commitment that `Prover::commit_variable` rejects with
    // `WitnessMissing`. Same pattern as the encrypted-borrow test.
    use curve25519_dalek::ristretto::CompressedRistretto;
    let program = ScriptBuilder::new()
        .push_str(String::commitment(qty_commit.clone()))
        .commit()
        .push_str(tag_str.clone())
        .issuepriv()
        .push_int(1u64)
        .return_();
    let parent = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500, 0, 0);
    let child_kind = CallKind::CellOpen {
        predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
        external_context: true,
    };
    let child = CallFrame::new(program.into_instructions(), child_kind, 500, 0, 0);
    let mut vm = VM::new(dummy_header(), parent);
    let p = core::mem::replace(&mut vm.current_call, child);
    vm.call_stack.push(p);

    let mut prover = Prover::new(&pc_gens);
    while !vm.current_call.is_finished() {
        vm.step_external(&mut prover).expect("step ok");
    }

    // After `return k=1`: parent stack = [Token, k=1, success=1].
    let predicate = Predicate::opaque(
        curve25519_dalek::ristretto::CompressedRistretto([0u8; 32]),
    );
    let expected_flv = flavor_from_predicate(&predicate, &tag_str);
    assert_eq!(vm.current_call.stack.len(), 3);
    match &vm.current_call.stack[0] {
        Value::Token(t) => {
            assert_eq!(t.qty.assignment(), Some(qty_int), "qty witness preserved");
            assert_eq!(
                t.flv.assignment(),
                Some(expected_flv),
                "flavor binds to predicate+tag",
            );
            // Sanity: the flv commitment is unblinded → matches the
            // canonical unblinded point for the flavor scalar.
            assert_eq!(
                t.flv.to_point(),
                Commitment::unblinded(expected_flv).to_point(),
            );
        }
        other => panic!("expected Token, got {}", value_kind(other)),
    }
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));

    // Txlog: Header + IssuePriv(qty_point, unblinded_flv_point).
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    let expected_flv_pt = Commitment::unblinded(expected_flv).to_point();
    match &vm.txlog[1] {
        TxEntry::IssuePriv(q, f) => {
            assert_eq!(*q, qty_commit.to_point());
            assert_eq!(*f, expected_flv_pt);
        }
        _ => panic!("expected TxEntry::IssuePriv"),
    }
}

#[test]
fn issuepriv_at_external_root_errors_predicate_context() {
    // ExternalRoot has no enclosing predicate — error propagates at
    // root frame (call_stack is empty).
    let script = ScriptBuilder::new().issuepriv().to_bytecode();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot, 1_000_000, 0, 0,
        ),
    );
    let mut delegate = make_stub_delegate();
    let err = drive_external(&mut vm, &mut delegate).unwrap_err();
    assert!(matches!(err, VMError::OpcodeRequiresPredicateContext));
}

#[test]
fn issuepriv_in_internal_context_yields_failure_marker() {
    // CellOpen with external_context: false: `require_external()` in
    // op_issuepriv errors `ExternalOnly`. Since it's inside a nested
    // frame, `fail_current_call` swallows the error and pushes `0`
    // onto the parent's stack — mirrors the
    // `op_open_cs_blocked_when_external_context_false` pattern in
    // test_cells.rs.
    use crate::vm::{Anchor, CallFrame, CallKind, VM};
    use curve25519_dalek::ristretto::CompressedRistretto;
    let parent = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500, 0, 0);
    let script = ScriptBuilder::new().issuepriv().to_bytecode();
    let child_kind = CallKind::CellOpen {
        predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
        external_context: false, // → require_external() will error
    };
    let child = CallFrame::new(
        ScriptBuilder::parse(&script).expect("parse").into_instructions(),
        child_kind,
        500,
        0,
        0,
    );
    let mut vm = VM::new(dummy_header(), parent);
    let p = core::mem::replace(&mut vm.current_call, child);
    vm.call_stack.push(p);
    vm.step_internal().expect("step ok — error swallowed into marker");
    // Child frame unwound back into parent (ExternalRoot).
    assert!(vm.call_stack.is_empty());
    // Parent now carries the `0` failure marker.
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn issuepriv_prove_then_verify_end_to_end() {
    // End-to-end: outer external script opens a cell whose leaf
    // contains `commit ; pushstr(tag) ; issuepriv ; retire ; push:0 ;
    // return`. The leaf mints a confidential token under the cell's
    // predicate identity, then retires it (so the child stack is
    // clean at frame exit). Prover produces a proof; Verifier
    // verifies it against the same bytecode. The full pipeline —
    // CS variable commitment, 64-bit range proof, batch verifier,
    // TxID-bound finalize — is exercised.
    use bulletproofs::PedersenGens;
    use curve25519_dalek::ristretto::CompressedRistretto;

    let pc_gens = PedersenGens::default();
    let qty_int = Int253::from(42u64);
    let qty_blind = curve25519_dalek::scalar::Scalar::from(11u64);
    let qty_commit = Commitment::blinded_with_factor(qty_int, qty_blind);
    let tag_str = String::from(b"gold".to_vec());

    // Leaf script — runs inside the isolated CellOpen frame on open:
    //   stack at entry: [qty_witness_string]  (k=1 arg from `open`)
    //   commit:    pop String → Variable
    //   pushstr:   tag
    //   issuepriv: Variable + tag → Token  (TxEntry::IssuePriv)
    //   retire:    Token → ø                 (TxEntry::Retire)
    //   push:0 return: exit with 0 results
    let inner = ScriptBuilder::new()
        .commit()
        .push_str(tag_str.clone())
        .issuepriv()
        .retire()
        .push_int(0u64)
        .return_();
    let inner_bytes = inner.to_bytecode();

    // Single-leaf NUMS-only predicate tree.
    let tree = PredicateTree::scripts_only(
        vec![inner_bytes.clone()],
        TEST_BLINDING_KEY,
    )
    .expect("scripts_only tree");
    let cp = tree.taproot_proof_for(0).expect("taproot_proof for leaf 0");
    let pred_point = tree.point;

    // Cell with empty payload — the witness rides on the open arg.
    let cell = Cell::new(
        Predicate::opaque(pred_point),
        Anchor([0xa1; 32]),
        vec![],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);

    // Outer:
    //   pushstr(cell_bytes); input;
    //   pushpoint(internal_key);
    //   neighbors-dict; position;
    //   push_script(inner) (witness-preserving);
    //   gas=1024; bytes=1024;
    //   pushstr(qty_witness); k=1; open;
    //   verify; drop  (consume the success + count markers)
    let mut outer = ScriptBuilder::new()
        .push_str(String::from(cell_bytes))
        .input()
        .push_point(*cp.internal_key.as_bytes());
    for (i, h) in cp.neighbors.iter().enumerate() {
        outer = outer
            .push_str(String::from(h.to_vec()))
            .push_int(i as u64);
    }
    let outer = outer
        .push_int(cp.neighbors.len() as u64)
        .dict()
        .push_str(String::from(cp.position.clone()))
        .push_script(inner)                                  // witness-bearing
        .push_int(1024u64)                                   // gas
        .push_int(1024u64)                                   // bytes
        .push_str(String::commitment(qty_commit.clone())) // qty witness arg
        .push_int(1u64)                                      // k = 1 arg
        .open()
        .verify()                                            // pops success marker (1)
        .drop_();                                            // pops count (0)

    // Prove.
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .expect("prove ok");
    let txid_p = result.txid;
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");

    // Verify against the same bytecode — the verifier sees no
    // witnesses on the wire (the leaf bytes encode just the opaque
    // commitment point and the bare opcodes).
    let pc_gens_v = PedersenGens::default();
    let verified = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .expect("verify ok");
    assert_eq!(verified.txid, txid_p, "txid round-trips");

    // The verifier's txlog must contain a matching Issue entry
    // (predicate-bound flavor) and a Retire entry. Use the prover's
    // result.txid as ground truth — the verifier reconstructs the
    // identical txlog by replaying the bytecode.
    let predicate = Predicate::opaque(pred_point);
    let expected_flv = flavor_from_predicate(&predicate, &tag_str);
    let expected_flv_pt = Commitment::unblinded(expected_flv).to_point();
    let expected_qty_pt = qty_commit.to_point();
    let has_issue = verified.txlog.iter().any(|e| matches!(
        e,
        TxEntry::IssuePriv(q, f) if *q == expected_qty_pt && *f == expected_flv_pt,
    ));
    assert!(has_issue, "verifier's txlog must contain IssuePriv(qty, flv)");
    let has_retire = verified.txlog.iter().any(|e| matches!(
        e,
        TxEntry::Retire(q, f) if *q == expected_qty_pt && *f == expected_flv_pt,
    ));
    assert!(has_retire, "verifier's txlog must contain matching Retire");
}

#[test]
fn issuepriv_with_int_qty_yields_failure_marker() {
    // issuepriv requires a Variable; an Int253 qty errors
    // `TypeNotVariable`. Failure-marker pattern (same reason as
    // `issuepriv_in_internal_context_yields_failure_marker`).
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_str(String::from(b"gold".to_vec()))
        .issuepriv()
        .to_bytecode();
    let mut vm = vm_with_nested_child_script(script);
    vm.step_internal().expect("step push:7");
    vm.step_internal().expect("step pushstr");
    vm.step_internal().expect("step issuepriv — error swallowed into marker");
    assert!(vm.call_stack.is_empty());
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn retire_cleartoken_emits_txlog() {
    // Pre-load a ClearToken, run `retire`.
    let mut vm = vm_with_script(ScriptBuilder::new().retire().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(11u64),
        Int253::from(22u64),
    )));
    vm.step_internal().expect("retire ok");
    assert!(vm.current_call.stack.is_empty());
    // Header + Retire.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    let q_pt = Commitment::unblinded(Int253::from(11u64)).to_point();
    let f_pt = Commitment::unblinded(Int253::from(22u64)).to_point();
    match &vm.txlog[1] {
        TxEntry::Retire(q, f) => {
            assert_eq!(*q, q_pt);
            assert_eq!(*f, f_pt);
        }
        _ => panic!("expected TxEntry::Retire"),
    }
}

#[test]
fn retire_token_emits_txlog_with_commitment_points() {
    let token = make_cleartext_token(11, 22);
    let q_pt = token.qty.to_point();
    let f_pt = token.flv.to_point();
    let mut vm = vm_with_script(ScriptBuilder::new().retire().to_bytecode());
    vm.push_value(Value::Token(token));
    vm.step_internal().expect("retire ok");
    // Header at index 0, Retire at index 1.
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Retire(q, f) => {
            assert_eq!(*q, q_pt);
            assert_eq!(*f, f_pt);
        }
        _ => panic!("expected TxEntry::Retire"),
    }
}

#[test]
fn retire_non_token_errors_typenottoken() {
    let mut vm = vm_with_script(ScriptBuilder::new().retire().to_bytecode());
    vm.push_value(Value::Int253(Int253::from(5u64)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::TypeNotToken));
}

#[test]
fn borrow_clear_path_returns_neg_pos_pair() {
    // Stack: [qty=5, flv=7] then `borrow` → [neg5, pos5].
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(5u64).push_int(7u64).borrow().to_bytecode(),
    );
    run_to_end(&mut vm).expect("borrow ok");
    assert_eq!(vm.current_call.stack.len(), 2);
    // Bottom: negative qty.
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => {
            assert_eq!(t.qty(), Int253::from(-5i64));
            assert_eq!(t.flv(), Int253::from(7u64));
        }
        _ => panic!("bottom must be -ClearToken"),
    }
    // Top: positive qty.
    match &vm.current_call.stack[1] {
        Value::ClearToken(t) => {
            assert_eq!(t.qty(), Int253::from(5u64));
            assert_eq!(t.flv(), Int253::from(7u64));
        }
        _ => panic!("top must be +ClearToken"),
    }
}

#[test]
fn borrow_with_point_errors_tokenrequirescs() {
    // pushpoint, push:7, borrow → Point qty → CS required.
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_point([0u8; 32]).push_int(7u64).borrow().to_bytecode(),
    );
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TokenRequiresCS));
}

#[test]
fn merge_same_flavor_combines_qtys() {
    // Push two cleartokens with same flavor, merge → (merged, 1).
    let mut vm = vm_with_script(ScriptBuilder::new().merge().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(3u64),
        Int253::from(7u64),
    )));
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(4u64),
        Int253::from(7u64),
    )));
    vm.step_internal().expect("merge ok");
    // Stack: [merged_cleartoken, 1].
    assert_eq!(vm.current_call.stack.len(), 2);
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => assert_eq!(t.qty(), Int253::from(7u64)),
        _ => panic!("bottom must be merged ClearToken"),
    }
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
}

#[test]
fn merge_flavor_mismatch_soft_fails() {
    let mut vm = vm_with_script(ScriptBuilder::new().merge().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(3u64),
        Int253::from(7u64),
    )));
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(4u64),
        Int253::from(8u64),
    )));
    vm.step_internal().expect("merge ok (soft-fail)");
    // Stack: [a, b, 0].
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[2], Int253::ZERO);
}

#[test]
fn split_within_qty_returns_two_cleartokens() {
    // ClearToken(10, 7), push:3, split.
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(3u64).split().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(10u64),
        Int253::from(7u64),
    )));
    // Need to move stack so the cleartoken is below the int. The
    // pushint8 runs first, pushing 3 on top, then split pops 3 and
    // the cleartoken below.
    //
    // Reorder: push cleartoken first, then run the script.
    run_to_end(&mut vm).expect("split ok");
    assert_eq!(vm.current_call.stack.len(), 2);
    match &vm.current_call.stack[0] {
        Value::ClearToken(t) => assert_eq!(t.qty(), Int253::from(7u64)),
        _ => panic!("bottom must be remainder"),
    }
    match &vm.current_call.stack[1] {
        Value::ClearToken(t) => assert_eq!(t.qty(), Int253::from(3u64)),
        _ => panic!("top must be new ClearToken"),
    }
}

#[test]
fn split_above_qty_hard_fails() {
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(9u64).split().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(2u64),
        Int253::from(7u64),
    )));
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TokenSplitOutOfRange));
}

#[test]
fn issuepubflv_pushes_correct_flavor() {
    // pushstr <32-byte cid>, pushstr "gold", issuepubflv.
    let actor_bytes = [0xab; 32];
    let script = ScriptBuilder::new()
        .push_str(String::from(actor_bytes.to_vec()))
        .push_str(String::from(b"gold".to_vec()))
        .issuepubflv()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).expect("issuepubflv ok");
    let expected = test_flavor_from_actor(
        &ActorID::Hash(actor_bytes),
        &String::from(b"gold".to_vec()),
    );
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn issuepubflv_rejects_non_32_byte_cid() {
    let script = ScriptBuilder::new()
        .push_str(String::from(vec![0xab; 16]))     // bad 16-byte cid
        .push_str(String::from(b"gold".to_vec()))
        .issuepubflv()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::IndexOutOfRange));
}

#[test]
fn issueprivflv_pushes_correct_flavor() {
    // pushstr <32-byte predicate point>, pushstr "gold", issueprivflv.
    let pred_bytes = [0xcd; 32];
    let script = ScriptBuilder::new()
        .push_str(String::from(pred_bytes.to_vec()))
        .push_str(String::from(b"gold".to_vec()))
        .issueprivflv()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).expect("issueprivflv ok");
    let predicate = Predicate::opaque(
        curve25519_dalek::ristretto::CompressedRistretto(pred_bytes),
    );
    let expected = flavor_from_predicate(
        &predicate,
        &String::from(b"gold".to_vec()),
    );
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn issueprivflv_rejects_non_32_byte_predicate() {
    let script = ScriptBuilder::new()
        .push_str(String::from(vec![0xcd; 16]))     // bad 16-byte predicate
        .push_str(String::from(b"gold".to_vec()))
        .issueprivflv()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::IndexOutOfRange));
}

