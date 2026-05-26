//! Tests for op_call, re-entrancy guard, and call-frame semantics.

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

/// Helper: builds bytecode that prepares the call stack and invokes
/// `op_call` with `k=0` args. Built via the public `Program` builder.
/// Spec stack (bottom→top, popped top-first by `op_call`):
///   k   gas   bytes   method   addr   call
fn call_script(target: &ActorID, method: u64, gas: u64) -> Vec<u8> {
    Program::new()
        .push_int(0u64)                                // k = 0 args
        .push_int(gas)                                 // gas
        .push_int(0u64)                                // bytes
        .push_int(method)                              // method
        .push_str(String::from(target.to_hash().to_vec())) // addr (32-byte)
        .call()
        .to_bytecode()
}

/// `nop`-only recv — short fixture used when the test only cares
/// about call-frame mechanics, not the callee body.
fn nop_recv() -> Vec<u8> {
    Program::new().nop().to_bytecode()
}

#[test]
fn call_a_to_b_creates_new_frame_with_callee_identity() {
    let mut reg = MemRegistry::new();
    // B: just `nop` so it does nothing then frame exits clean.
    let b = deploy_recv(&mut reg, nop_recv(), 1_000);

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
fn call_does_not_emit_txlog_entry_by_itself() {
    // Calls are intra-tx control flow and emit no txlog entry on their
    // own. The callee's effects (Output / Send / ActorSave / etc.) are
    // what the state machine reads. Here B's recv is a nop, so the
    // txlog after the call is just the Header.
    let mut reg = MemRegistry::new();
    let b = deploy_recv(&mut reg, nop_recv(), 1_000);
    let a_script = call_script(&b, 0, 10_000);
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);

    let mut vm = vm_for_actor(a, a_script);
    while !matches!(vm.current_call.kind, CallKind::ActorCall { .. }) {
        vm.step_internal_with_registry(&mut reg).expect("step ok");
    }

    // The callee's anchor IS what `last_anchor` becomes — split-left of
    // the InternalRoot's zero anchor. Verifies the per-call anchor
    // split without needing a txlog entry to mirror it.
    let (expected_callee_anchor, _post) = Anchor([0u8; 32]).split();
    assert_eq!(vm.last_anchor.unwrap(), expected_callee_anchor);

    // Header at [0], nothing else — no Call entry exists in the txlog.
    assert_eq!(vm.txlog.len(), 1, "calls produce no txlog entries");
}

#[test]
fn direct_self_call_rejected_as_reentrancy() {
    let mut reg = MemRegistry::new();
    let mut state = ActorState::new();
    state.public.insert(
        RECV_METHOD,
        Value::String(String::from(nop_recv())),
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
            // Rejected reentry produces no side effects — txlog is
            // just the Header. (Calls themselves emit no txlog entry;
            // this assertion catches any accidental ActorSave / Send
            // from a half-entered frame.)
            assert_eq!(vm.txlog.len(), 1, "rejected reentry must not emit side effects");
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
        Value::String(String::from(nop_recv())),
    );
    reg.deploy(a_id.clone(), a_state, 100_000, 0).expect("deploy A");

    let mut b_state = ActorState::new();
    b_state.public.insert(
        RECV_METHOD,
        Value::String(String::from(nop_recv())),
    );
    reg.deploy(b_id.clone(), b_state, 100_000, 0).expect("deploy B");

    // B's recv: call A (re-entry — fails with marker 0), drop the
    // marker, return cleanly.
    let b_recv = {
        let mut p = Program::parse(&call_script(&a_id, 0, 1_000)).expect("parse");
        p.push_instr(crate::ops::Instruction::Drop);
        p.to_bytecode()
    };
    let mut new_b_state = ActorState::new();
    new_b_state.public.insert(
        RECV_METHOD,
        Value::String(String::from(b_recv)),
    );
    reg.save_state(&b_id, new_b_state).expect("update B");

    // A's recv: call B (B returns 0 results successfully), drop the
    // [count, success] markers.
    let a_script = {
        let mut p = Program::parse(&call_script(&b_id, 0, 50_000)).expect("parse");
        p.push_instr(crate::ops::Instruction::Drop);
        p.push_instr(crate::ops::Instruction::Drop);
        p.to_bytecode()
    };
    let mut vm = vm_for_actor(a_id, a_script);

    // Tx must complete without fatal error.
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    // Reentrancy is silently rejected with a `0` marker. Neither side
    // wrote actor state (no load/save), so txlog has only the Header.
    assert_eq!(vm.txlog.len(), 1,
        "no actor state mutations → only Header in txlog");
}

#[test]
fn sibling_calls_to_same_actor_allowed_after_return() {
    // A calls B, B returns 0; A calls B again — the second call is
    // not re-entrancy because B isn't on the live call stack at
    // the time of the second call.
    let mut reg = MemRegistry::new();
    // B: `push:0; return` — returns 0 results.
    let b_recv = Program::new().push_int(0u64).return_().to_bytecode();
    let b = deploy_recv(&mut reg, b_recv, 1_000);

    // A: call B; drop returned `[count=0, success=1]`; call B again;
    // drop those two markers — leaves an empty stack for the implicit
    // root finish_call.
    let a_script = {
        let mut p = Program::parse(&call_script(&b, 0, 5_000)).expect("parse");
        p.push_instr(crate::ops::Instruction::Drop);
        p.push_instr(crate::ops::Instruction::Drop);
        let second = Program::parse(&call_script(&b, 0, 5_000)).expect("parse");
        for i in second.into_instructions() {
            p.push_instr(i);
        }
        p.push_instr(crate::ops::Instruction::Drop);
        p.push_instr(crate::ops::Instruction::Drop);
        p.to_bytecode()
    };
    let a = deploy_recv(&mut reg, a_script.clone(), 100_000);

    let mut vm = vm_for_actor(a, a_script);
    // Step the whole way — no errors, no panic.
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}
}

#[test]
fn call_without_registry_errors() {
    let mut reg = MemRegistry::new();
    let b = deploy_recv(&mut reg, nop_recv(), 1_000);
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
fn save_emits_actorsave_with_post_state_root() {
    // op_save mutates the actor's persistent state and records a
    // structural effect: `TxEntry::ActorSave { actor, post_state_root }`.
    // A thin state machine consuming the TxLog can apply these in
    // order to mutate the registry without re-running the script.
    let mut reg = MemRegistry::new();
    // Recv: `load; save` — round-trip with no state change still
    // emits the ActorSave entry.
    let recv = Program::new().load().save().to_bytecode();
    let id = ActorID::Hash([0xab; 32]);
    let mut state = ActorState::new();
    state.public.insert(RECV_METHOD, Value::String(crate::String::from(recv.clone())));
    reg.deploy(id.clone(), state, 10_000, 0).expect("deploy");

    let mut vm = vm_for_actor(id.clone(), recv);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    let save = vm.txlog.iter().find_map(|e| match e {
        crate::tx::TxEntry::ActorSave { actor, post_state_root } =>
            Some((actor.clone(), *post_state_root)),
        _ => None,
    }).expect("ActorSave entry present");
    assert_eq!(save.0, id);
    assert_ne!(save.1, [0u8; 32], "state_root is a Merlin challenge, non-zero");
    let save_count = vm.txlog.iter()
        .filter(|e| matches!(e, crate::tx::TxEntry::ActorSave { .. }))
        .count();
    assert_eq!(save_count, 1);
}

#[test]
fn call_to_unknown_actor_rejected_with_marker() {
    let mut reg = MemRegistry::new();
    // Caller exists; target does not — pre-frame registry lookup
    // fails → marker `0` on caller's stack. No side effects emitted.
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
            // Header only — no side effects from a rejected call.
            assert_eq!(vm.txlog.len(), 1);
            return;
        }
    }
    panic!("call did not push a marker");
}
