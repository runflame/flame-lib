//! Tests for actorid / anchor / callerid / method.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::{ActorID, ActorRegistry, MethodKey};

/// Builds an InternalRoot VM with explicit identity fields.
fn vm_internal_with(
    actor: ActorID,
    method: MethodKey,
    caller: Option<ActorID>,
    anchor: Anchor,
    script: Vec<u8>,
) -> VM {
    let kind = CallKind::InternalRoot {
        actor,
        method,
        caller,
        anchor,
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        ),
    )
}

/// Builds an ExternalRoot VM (no actor context).
fn vm_external(script: Vec<u8>) -> VM {
    VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    )
}

#[test]
fn actorid_in_internal_root_pushes_hash_string() {
    let id = ActorID::Hash([0xab; 32]);
    let mut vm = vm_internal_with(
        id.clone(),
        MethodKey::from(0u64),
        None,
        Anchor([0u8; 32]),
        vec![0x9c],
    );
    vm.step_internal().expect("step ok");
    match vm.current_call.stack.last().expect("stack non-empty") {
        Value::String(s) => assert_eq!(s.bytes_view().as_ref(), &[0xab; 32]),
        _ => panic!("expected String"),
    }
}

#[test]
fn actorid_in_external_root_errors_actor_context() {
    let mut vm = vm_external(vec![0x9c]);
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

#[test]
fn actorid_in_constructor_form_uses_canonical_hash_seed() {
    // Constructor-form id surfaces as its deterministic hash seed
    // (per ActorID::to_hash) — the same byte sequence the
    // Hash-form variant would carry.
    let ctor = ActorID::Constructor(vec![0x01, 0x02]);
    let expected = ctor.to_hash();
    let mut vm = vm_internal_with(
        ctor,
        MethodKey::from(0u64),
        None,
        Anchor([0u8; 32]),
        vec![0x9c],
    );
    vm.step_internal().expect("step ok");
    match vm.current_call.stack.last().expect("stack non-empty") {
        Value::String(s) => assert_eq!(s.bytes_view().as_ref(), &expected[..]),
        _ => panic!("expected String"),
    }
}

#[test]
fn anchor_in_internal_root_pushes_frame_anchor() {
    let id = ActorID::Hash([0u8; 32]);
    let anc = Anchor([0xff; 32]);
    let mut vm = vm_internal_with(
        id,
        MethodKey::from(0u64),
        None,
        anc,
        vec![0x9d],
    );
    vm.step_internal().expect("step ok");
    match vm.current_call.stack.last().expect("stack non-empty") {
        Value::String(s) => assert_eq!(s.bytes_view().as_ref(), &[0xff; 32]),
        _ => panic!("expected String"),
    }
}

#[test]
fn anchor_in_external_root_errors_actor_context() {
    let mut vm = vm_external(vec![0x9d]);
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

#[test]
fn callerid_with_some_caller_pushes_hash_string() {
    let caller = ActorID::Hash([0x33; 32]);
    let mut vm = vm_internal_with(
        ActorID::Hash([0x44; 32]),
        MethodKey::from(0u64),
        Some(caller),
        Anchor([0u8; 32]),
        vec![0xa0],
    );
    vm.step_internal().expect("step ok");
    match vm.current_call.stack.last().expect("stack non-empty") {
        Value::String(s) => assert_eq!(s.bytes_view().as_ref(), &[0x33; 32]),
        _ => panic!("expected String"),
    }
}

#[test]
fn callerid_with_none_caller_pushes_zero_string() {
    // External originator → caller None → push zeros (not error).
    let mut vm = vm_internal_with(
        ActorID::Hash([0x44; 32]),
        MethodKey::from(0u64),
        None,
        Anchor([0u8; 32]),
        vec![0xa0],
    );
    vm.step_internal().expect("step ok");
    match vm.current_call.stack.last().expect("stack non-empty") {
        Value::String(s) => assert_eq!(s.bytes_view().as_ref(), &[0u8; 32]),
        _ => panic!("expected String"),
    }
}

#[test]
fn callerid_in_external_root_errors_actor_context() {
    let mut vm = vm_external(vec![0xa0]);
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

#[test]
fn method_pushes_int253_key() {
    let mut vm = vm_internal_with(
        ActorID::Hash([0u8; 32]),
        MethodKey::from(42u64),
        None,
        Anchor([0u8; 32]),
        vec![0xa1],
    );
    vm.step_internal().expect("step ok");
    assert_int(
        vm.current_call.stack.last().expect("stack non-empty"),
        Int253::from(42u64),
    );
}

#[test]
fn method_in_external_root_errors_actor_context() {
    let mut vm = vm_external(vec![0xa1]);
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}
