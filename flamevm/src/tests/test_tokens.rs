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
    let v = Value::ClearToken(ClearToken::new(Int253::zero(), Int253::from(7u64)));
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
fn flavor_from_actor_is_deterministic_and_diverges_on_inputs() {
    let actor1 = ActorID::Hash([0x11; 32]);
    let actor2 = ActorID::Hash([0x22; 32]);
    let tag_a = String::from(b"gold".to_vec());
    let tag_b = String::from(b"silver".to_vec());

    let f_aa = test_flavor_from_actor(&actor1, &tag_a);
    let f_aa_2 = test_flavor_from_actor(&actor1, &tag_a);
    assert_eq!(f_aa, f_aa_2, "deterministic for identical inputs");

    let f_ab = test_flavor_from_actor(&actor1, &tag_b);
    let f_ba = test_flavor_from_actor(&actor2, &tag_a);
    assert_ne!(f_aa, f_ab, "tag change must change flavor");
    assert_ne!(f_aa, f_ba, "actor change must change flavor");

    // Non-negative by construction (mod-order wide reduction).
    assert!(!f_aa.is_negative());
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
    let mut vm = vm_with_script(Program::new().amount().to_bytecode());
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
    let mut vm = vm_with_script(Program::new().amount().to_bytecode());
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
    let mut vm = vm_with_script(Program::new().amount().to_bytecode());
    vm.push_value(Value::Int253(Int253::from(5u64)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::TypeNotToken));
    // Original value is restored on error.
    assert_eq!(vm.current_call.stack.len(), 1);
}

#[test]
fn issue_clear_path_emits_txlog_and_returns_cleartoken() {
    // Script: push:7, pushstr "gold", issue.
    // Run under InternalRoot with a known actor identity so
    // `op_issue` can resolve a flavor.
    let actor = ActorID::Hash([0x55; 32]);
    let script = Program::new()
        .push_int(7u64)
        .push_str(String::from(b"gold".to_vec()))
        .issue()
        .to_bytecode();
    let mut vm = vm_internal_with_actor(script, actor.clone());
    run_to_end(&mut vm).expect("issue ok");

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

    // Txlog has Header + Issue entry with unblinded commitments.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    let expected_qty_pt = Commitment::unblinded(Int253::from(7u64)).to_point();
    let expected_flv_pt = Commitment::unblinded(expected_flv).to_point();
    match &vm.txlog[1] {
        crate::tx::TxEntry::Issue(q, f) => {
            assert_eq!(*q, expected_qty_pt);
            assert_eq!(*f, expected_flv_pt);
        }
        _ => panic!("expected TxEntry::Issue"),
    }
}

#[test]
fn issue_with_point_qty_errors_tokenrequirescs() {
    // Pushpoint then pushstr then issue → encrypted branch (deferred).
    let actor = ActorID::Hash([0x55; 32]);
    let script = Program::new()
        .push_point([0u8; 32])
        .push_str(String::from(b"gold".to_vec()))
        .issue()
        .to_bytecode();
    let mut vm = vm_internal_with_actor(script, actor);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TokenRequiresCS));
}

#[test]
fn issue_at_external_root_errors_actor_context() {
    // ExternalRoot has no actor identity.
    let script = Program::new()
        .push_int(7u64)
        .push_str(String::from(b"gold".to_vec()))
        .issue()
        .to_bytecode();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot, 1_000_000, 0, 0,
        ),
    );
    let mut delegate = make_stub_delegate();
    let err = drive_external(&mut vm, &mut delegate).unwrap_err();
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

#[test]
fn retire_cleartoken_emits_txlog() {
    // Pre-load a ClearToken, run `retire`.
    let mut vm = vm_with_script(Program::new().retire().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(11u64),
        Int253::from(22u64),
    )));
    vm.step_internal().expect("retire ok");
    assert!(vm.current_call.stack.is_empty());
    // Header + Retire.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    let q_pt = Commitment::unblinded(Int253::from(11u64)).to_point();
    let f_pt = Commitment::unblinded(Int253::from(22u64)).to_point();
    match &vm.txlog[1] {
        crate::tx::TxEntry::Retire(q, f) => {
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
    let mut vm = vm_with_script(Program::new().retire().to_bytecode());
    vm.push_value(Value::Token(token));
    vm.step_internal().expect("retire ok");
    // Header at index 0, Retire at index 1.
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Retire(q, f) => {
            assert_eq!(*q, q_pt);
            assert_eq!(*f, f_pt);
        }
        _ => panic!("expected TxEntry::Retire"),
    }
}

#[test]
fn retire_non_token_errors_typenottoken() {
    let mut vm = vm_with_script(Program::new().retire().to_bytecode());
    vm.push_value(Value::Int253(Int253::from(5u64)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::TypeNotToken));
}

#[test]
fn borrow_clear_path_returns_neg_pos_pair() {
    // Stack: [qty=5, flv=7] then `borrow` → [neg5, pos5].
    let mut vm = vm_with_script(
        Program::new().push_int(5u64).push_int(7u64).borrow().to_bytecode(),
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
        Program::new().push_point([0u8; 32]).push_int(7u64).borrow().to_bytecode(),
    );
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TokenRequiresCS));
}

#[test]
fn merge_same_flavor_combines_qtys() {
    // Push two cleartokens with same flavor, merge → (merged, 1).
    let mut vm = vm_with_script(Program::new().merge().to_bytecode());
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
    let mut vm = vm_with_script(Program::new().merge().to_bytecode());
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
    assert_int(&vm.current_call.stack[2], Int253::zero());
}

#[test]
fn split_within_qty_returns_two_cleartokens() {
    // ClearToken(10, 7), push:3, split.
    let mut vm = vm_with_script(Program::new().push_int(3u64).split().to_bytecode());
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
    let mut vm = vm_with_script(Program::new().push_int(9u64).split().to_bytecode());
    vm.push_value(Value::ClearToken(ClearToken::new(
        Int253::from(2u64),
        Int253::from(7u64),
    )));
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TokenSplitOutOfRange));
}

#[test]
fn issueflv_pushes_correct_flavor() {
    // pushstr <32-byte cid>, pushstr "gold", issueflv.
    let actor_bytes = [0xab; 32];
    let script = Program::new()
        .push_str(String::from(actor_bytes.to_vec()))
        .push_str(String::from(b"gold".to_vec()))
        .issue_flv()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).expect("issueflv ok");
    let expected = test_flavor_from_actor(
        &ActorID::Hash(actor_bytes),
        &String::from(b"gold".to_vec()),
    );
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], expected);
}

#[test]
fn issueflv_rejects_non_32_byte_cid() {
    let script = Program::new()
        .push_str(String::from(vec![0xab; 16]))     // bad 16-byte cid
        .push_str(String::from(b"gold".to_vec()))
        .issue_flv()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::IndexOutOfRange));
}

