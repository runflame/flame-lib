//! Tests for op_call, re-entrancy guard, and TxEntry::Call.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::{ActorID, ActorRegistry, ActorState, MemRegistry, Int253, RECV_METHOD};

/// Helper: deploys an actor whose `recv` method runs `script`.
/// Derives the actor's id from the script bytes (treats `script`
/// as a stand-in for the constructor — real deployments would
/// hash the actual constructor that produces this state).
fn deploy_recv(reg: &mut MemRegistry, script: Vec<u8>, vbytes: u64) -> ActorID {
    let mut state = ActorState::new();
    state.public.insert(
        RECV_METHOD,
        Value::String(String::from(script.clone())),
    );
    let id = ActorID::Hash(ActorID::Constructor(script).to_hash());
    reg.deploy(id.clone(), state, vbytes, 0).expect("deploy");
    id
}

/// Helper: builds a VM running `script` under the given actor's
/// InternalRoot frame, with all the actor-context plumbing.
fn vm_for_actor(actor: ActorID, script: Vec<u8>) -> VM {
    let kind = CallKind::InternalRoot {
        actor,
        method: Int253::from(0u64),
        caller: None,
        anchor: Anchor([0u8; 32]),
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script)
                .expect("script parses")
                .into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        ),
    )
}

/// Helper: builds the bytecode that prepares the call stack and
/// invokes 0x95 (call). Spec stack (bottom→top, since op_call pops
/// from the top):
///   args…   k   gas   bytes   method   addr   call
/// With 0 args the push order becomes k, gas, bytes, method, addr.
fn call_script(target: &ActorID, method: u64, gas: u64) -> Vec<u8> {
    let mut s = Vec::new();
    // k (0 args) — deepest
    s.push(0x00);
    // gas
    s.extend(push_int_bytes(gas));
    // bytes (0)
    s.push(0x00);
    // method
    s.extend(push_int_bytes(method));
    // addr: 32-byte String — pushstr + sub-varint length + hash bytes.
    s.push(0x19); // pushstr
    s.push(0x00); // sub-varint tag U8
    s.push(32);   // length
    s.extend_from_slice(&target.to_hash());
    // call
    s.push(0x95);
    s
}

/// Push a small unsigned int via the narrowest encoding the
/// existing encoder uses. For values < 16, uses push:k (0x0k).
/// For others, falls back to pushint64 (0x14 + 8 bytes LE).
fn push_int_bytes(n: u64) -> Vec<u8> {
    if n < 16 {
        vec![n as u8]
    } else {
        let mut v = vec![0x14];
        v.extend_from_slice(&n.to_le_bytes());
        v
    }
}

#[test]
fn call_a_to_b_creates_new_frame_with_callee_identity() {
    let mut reg = MemRegistry::new();
    // B: just `nop` so it does nothing then frame exits clean.
    let b = deploy_recv(&mut reg, vec![0x1d], 1_000);

    // A: call B; expect 0 results.
    let a_script = call_script(&b, 0, 10_000);
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);

    let mut vm = vm_for_actor(a.clone(), a_script);

    // Step through every instruction in A up to and including the
    // `call`. After call, current frame should be B's.
    while !matches!(vm.current_call.kind, CallKind::ActorCall { .. }) {
        let cont = vm.step_internal_with_registry(&mut reg).expect("step ok");
        if !cont {
            panic!("script ended before reaching call");
        }
    }
    match &vm.current_call.kind {
        CallKind::ActorCall { actor, caller, .. } => {
            assert_eq!(actor, &b);
            assert_eq!(caller, &a);
        }
        _ => panic!("expected ActorCall frame after call"),
    }
}

#[test]
fn call_emits_txentry_call_with_pre_state_root_and_anchor() {
    let mut reg = MemRegistry::new();
    let b = deploy_recv(&mut reg, vec![0x1d], 1_000);
    let a_script = call_script(&b, 0, 10_000);
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);

    let mut vm = vm_for_actor(a, a_script);
    while !matches!(vm.current_call.kind, CallKind::ActorCall { .. }) {
        vm.step_internal_with_registry(&mut reg).expect("step ok");
    }

    // txlog: Header at [0], Call at [1].
    let call_entry = vm.txlog.iter().find(|e| matches!(e, crate::tx::TxEntry::Call { .. }));
    let (callee, _method, root, anchor) = match call_entry.expect("Call entry present") {
        crate::tx::TxEntry::Call {
            callee,
            method,
            pre_state_root,
            callee_anchor,
        } => (callee.clone(), *method, *pre_state_root, *callee_anchor),
        _ => unreachable!(),
    };
    assert_eq!(callee, b);
    // callee_anchor is the LEFT half of split(InternalRoot.anchor).
    // The fixture seeds with the zero anchor; production frames get
    // a Message-delivered (split-derived) value.
    let (expected_callee_anchor, _post) = Anchor([0u8; 32]).split();
    assert_eq!(anchor, expected_callee_anchor);
    // Pre-state root non-zero — Merlin challenge over actor state.
    assert_ne!(root, [0u8; 32]);
}

#[test]
fn direct_self_call_rejected_as_reentrancy() {
    let mut reg = MemRegistry::new();
    let mut state = ActorState::new();
    state.public.insert(
        RECV_METHOD,
        Value::String(String::from(b"\x1d".to_vec())),
    );
    let id = ActorID::Hash([0xa1; 32]);
    reg.deploy(id.clone(), state, 10_000, 0).expect("deploy");

    // Self-call from id's actor context: pre-frame reentrancy check
    // rejects with marker `0` on the caller's stack. The call simply
    // "did not happen" — no Call entry, no anchor split.
    let script = call_script(&id, 0, 10_000);
    let mut vm = vm_for_actor(id.clone(), script);

    // Step through the call op. Marker `0` is left on top of the
    // stack; subsequent steps may fail later (StackNotClean at root
    // finish) but the call-time error is the marker.
    while vm.step_internal_with_registry(&mut reg).is_ok() {
        if !vm.current_call.stack.is_empty()
            && matches!(vm.current_call.stack.last(), Some(Value::Int253(_)))
        {
            // After call: stack should be exactly [0].
            assert_eq!(vm.current_call.stack.len(), 1);
            match &vm.current_call.stack[0] {
                Value::Int253(i) => assert_eq!(*i, Int253::from(0u64), "marker"),
                _ => panic!("expected Int253 marker"),
            }
            // No TxEntry::Call emitted for the rejected self-call.
            let calls = vm.txlog.iter()
                .filter(|e| matches!(e, crate::tx::TxEntry::Call { .. }))
                .count();
            assert_eq!(calls, 0, "rejected reentry must not log a Call entry");
            return;
        }
    }
    panic!("call did not push a marker");
}

#[test]
fn indirect_cycle_rejected_as_reentrancy() {
    let mut reg = MemRegistry::new();
    // Build A and B. A → B; B re-calls A. The re-entrancy detection
    // fires inside B's call attempt and converts to a `0` failure
    // marker on B's stack (instead of a fatal error) per the new
    // call-return contract. B drops it and returns cleanly to A.
    let a_id = ActorID::Hash([0xaa; 32]);
    let b_id = ActorID::Hash([0xbb; 32]);

    let mut a_state = ActorState::new();
    a_state.public.insert(
        RECV_METHOD,
        Value::String(String::from(b"\x1d".to_vec())),
    );
    reg.deploy(a_id.clone(), a_state, 100_000, 0).expect("deploy A");

    let mut b_state = ActorState::new();
    b_state.public.insert(
        RECV_METHOD,
        Value::String(String::from(b"\x1d".to_vec())),
    );
    reg.deploy(b_id.clone(), b_state, 100_000, 0).expect("deploy B");

    // B's recv: call A (re-entry — fails with marker 0), drop the
    // marker, return cleanly.
    let mut b_recv = call_script(&a_id, 0, 1_000);
    b_recv.push(0x1c); // drop failure marker
    let mut new_b_state = ActorState::new();
    new_b_state.public.insert(
        RECV_METHOD,
        Value::String(String::from(b_recv)),
    );
    reg.save_state(&b_id, new_b_state).expect("update B");

    // A's recv: call B (B returns 0 results successfully), drop the
    // [count, success] markers.
    let mut a_script = call_script(&b_id, 0, 50_000);
    a_script.extend(vec![0x1c, 0x1c]);
    let mut vm = vm_for_actor(a_id, a_script);

    // Tx must complete without fatal error.
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    // Reentrancy is silently rejected before a Call entry is logged:
    // exactly one Call entry (A → B); B → A leaves no trace beyond
    // the failure marker B observed.
    let call_count = vm.txlog.iter()
        .filter(|e| matches!(e, crate::tx::TxEntry::Call { .. }))
        .count();
    assert_eq!(call_count, 1, "only A→B should be logged; reentrant B→A is rejected pre-log");
}

#[test]
fn sibling_calls_to_same_actor_allowed_after_return() {
    // A calls B, B returns 0; A calls B again — the second call is
    // not re-entrancy because B isn't on the live call stack at
    // the time of the second call.
    let mut reg = MemRegistry::new();
    // B: return 0 results — `push:0 return`.
    let b = deploy_recv(&mut reg, vec![0x00, 0x7e], 1_000);

    // A: call B; drop returned `[count=0, success=1]`; call B again;
    // drop those two markers — leaves an empty stack for the implicit
    // root finish_call.
    let mut a_script = call_script(&b, 0, 5_000);
    a_script.extend(vec![0x1c, 0x1c]);                 // drop, drop
    a_script.extend(call_script(&b, 0, 5_000));
    a_script.extend(vec![0x1c, 0x1c]);                 // drop, drop
    let a = deploy_recv(&mut reg, a_script.clone(), 100_000);

    let mut vm = vm_for_actor(a, a_script);
    // Step the whole way — no errors, no panic.
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}
}

#[test]
fn call_without_registry_errors() {
    let mut reg = MemRegistry::new();
    let b = deploy_recv(&mut reg, vec![0x1d], 1_000);
    let a_script = call_script(&b, 0, 10_000);
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);
    let mut vm = vm_for_actor(a, a_script);
    // step_internal (no registry) hits RegistryUnavailable at the
    // call.
    let err = loop {
        match vm.step_internal() {
            Ok(true) => continue,
            Ok(false) => panic!("ran out before reaching call"),
            Err(e) => break e,
        }
    };
    assert!(matches!(err, VMError::RegistryUnavailable), "got {:?}", err);
}

#[test]
fn call_to_unknown_actor_rejected_with_marker() {
    let mut reg = MemRegistry::new();
    // Caller exists; target does not — pre-frame registry lookup
    // fails → marker `0` on caller's stack, no Call entry.
    let ghost = ActorID::Hash([0xab; 32]);
    let a_script = call_script(&ghost, 0, 10_000);
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);
    let mut vm = vm_for_actor(a, a_script);
    while vm.step_internal_with_registry(&mut reg).is_ok() {
        if !vm.current_call.stack.is_empty() {
            assert_eq!(vm.current_call.stack.len(), 1);
            match &vm.current_call.stack[0] {
                Value::Int253(i) => assert_eq!(*i, Int253::from(0u64)),
                _ => panic!("expected Int253 marker"),
            }
            let calls = vm.txlog.iter()
                .filter(|e| matches!(e, crate::tx::TxEntry::Call { .. }))
                .count();
            assert_eq!(calls, 0);
            return;
        }
    }
    panic!("call did not push a marker");
}
