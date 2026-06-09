//! Tests for op_call, re-entrancy guard, and call-frame semantics.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::{empty_state, ActorID, ActorRegistry, MemRegistry, Int253, RECV_METHOD};

/// Helper: deploys an actor whose `recv` method runs `script`.
/// Derives the actor's id from the script bytes (treats `script`
/// as a stand-in for the constructor — real deployments would
/// hash the actual constructor that produces this state).
fn deploy_recv(reg: &mut MemRegistry, script: Vec<u8>, vbytes: u64) -> ActorID {
    let id = ActorID::Hash(ActorID::Constructor(script.clone()).to_hash());
    reg.deploy(id.clone(), script, empty_state(), vbytes, 0).expect("deploy");
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
fn call_to_checked_out_actor_blocked() {
    // The re-entrancy lock (ADR 0017): calling an actor whose state is
    // checked out — a live frame holds it mid-update — fails with the
    // `0` marker (the call "did not happen"). The state's absence *is*
    // the lock; `resolve_method` returns `ActorEmpty`.
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0xa1; 32]);
    reg.deploy(id.clone(), Program::new().nop().to_bytecode(), empty_state(), 10_000, 0)
        .expect("deploy");
    // Check the state out, as a live frame mid-update would.
    reg.actor_mut(&id).expect("present").state = None;

    let script = call_script(&id, 0, 10_000);
    let mut vm = vm_for_actor(id.clone(), script);
    while vm.step_internal_with_registry(&mut reg).is_ok() {
        if vm.current_call.stack.len() == 1 {
            match &vm.current_call.stack[0] {
                Value::Int253(i) => {
                    assert_eq!(*i, Int253::from(0u64), "block marker");
                    assert_eq!(vm.txlog.len(), 1, "blocked re-entry must not emit side effects");
                    return;
                }
                _ => panic!("expected Int253 marker"),
            }
        }
    }
    panic!("call did not push a marker");
}

#[test]
fn reentrant_call_succeeds_when_state_not_held() {
    // Re-entry is *permitted* when the target isn't holding its state
    // (ADR 0017 — strictly more expressive than the old blanket ban).
    // A.recv → B → A.method1: B re-enters A, and A.method1 actually
    // runs (it logs) because A never checked out its state. Under the
    // old call-stack guard this re-entry was rejected.
    let mut reg = MemRegistry::new();
    let a_id = ActorID::Hash([0xaa; 32]);
    let b_id = ActorID::Hash([0xbb; 32]);

    // A.method1: emit a Data log entry, then return cleanly.
    let a_method1 = Program::new()
        .push_str(String::from(b"reentered".to_vec()))
        .log()
        .to_bytecode();
    // A.recv (method 0): call B, drop B's [count, success].
    let a_recv = {
        let mut p = Program::parse(&call_script(&b_id, 0, 200_000)).expect("parse");
        p.push_instr(crate::ops::Instruction::Drop);
        p.push_instr(crate::ops::Instruction::Drop);
        p.to_bytecode()
    };
    // A's code: a dispatch blob — method 0 → a_recv, method 1 → a_method1.
    let a_code = dispatch_code(&[(0, a_recv.clone()), (1, a_method1)]);
    reg.deploy(a_id.clone(), a_code, empty_state(), 1_000_000, 0)
        .expect("deploy A");

    // B.recv: call A.method1, drop A's [count, success].
    let b_recv = {
        let mut p = Program::parse(&call_script(&a_id, 1, 100_000)).expect("parse");
        p.push_instr(crate::ops::Instruction::Drop);
        p.push_instr(crate::ops::Instruction::Drop);
        p.to_bytecode()
    };
    // B has only recv (method 0) → its code is the recv script directly.
    reg.deploy(b_id.clone(), b_recv, empty_state(), 1_000_000, 0)
        .expect("deploy B");

    let mut vm = vm_for_actor(a_id, a_recv);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    // A.method1 ran via the re-entry → its Data entry is in the txlog.
    let logged = vm.txlog.iter().any(|e| matches!(e, crate::tx::TxEntry::Data(_)));
    assert!(logged, "re-entrant A.method1 must have run (and logged)");
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

// ── F1 regression: actor-state rollback on call failure ─────────────

/// Audit F1: a failed sub-call's `op_save` (or unmatched `op_load`)
/// must not leak into the parent's registry view. Pre-fix, the
/// callee's mark + state mutation would persist across
/// `fail_current_call`, while the corresponding `TxEntry::ActorSave`
/// was truncated from the txlog — letting a caller smuggle state
/// mutations past the call boundary without an audit trail.
///
/// Test shape: an outer actor A calls inner X whose recv just `load`s
/// and then throws (no `save`). Without F1, X's mark survives →
/// tx-end self-destruct → X is destroyed. With F1, the failed
/// sub-call's mark is rolled back → X survives.
#[test]
fn f1_failed_subcall_load_does_not_destroy_actor() {
    let mut reg = MemRegistry::new();
    // X.recv: load (mark X), push:0, verify (throws after load,
    // before save). On failure, the rollback must clear X's mark.
    let evil_recv = Program::new()
        .load()
        .push_int(0u64)
        .verify()
        .to_bytecode();
    let x_id = deploy_recv(&mut reg, evil_recv, 10_000);

    // A.recv: call X (k=0, gas=5_000), drop the failure marker so
    // A's frame exits clean (stack empty → no StackNotClean).
    let a_recv = Program::new()
        .push_int(0u64)                              // k = 0 args
        .push_int(5_000u64)                          // gas
        .push_int(0u64)                              // bytes
        .push_int(RECV_METHOD)                       // method
        .push_str(String::from(x_id.to_hash().to_vec())) // addr
        .call()
        .drop_()                                     // drop failure marker (0)
        .to_bytecode();
    let a_id = deploy_recv(&mut reg, a_recv, 10_000);

    // Run via execute_internal targeting A.
    let block = BlockContext { height: 100 };
    let msg = Message {
        target: a_id,
        method: RECV_METHOD,
        caller: None,
        anchor: Anchor([0x42; 32]),
        payload: Vec::new(),
        gas: 100_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let _ = VM::execute_internal(dummy_header(), msg, &mut reg, &block)
        .expect("outer tx succeeds (A drops the inner failure marker)");

    // F1 invariant: X survives the failed sub-call. Pre-fix, X's
    // mark would have persisted past the rollback boundary, and
    // tx-end `commit_tx_destructions` would have destroyed X.
    assert!(reg.exists(&x_id), "X must still exist after the failed sub-call");
    assert!(
        !reg.actor(&x_id).map_or(false, |a| a.is_checked_out()),
        "X's load-mark must be cleared by the call-frame rollback",
    );
}

/// Audit F1 + F3: a sub-call that `save`s a mutated state and then
/// throws must roll back the state write. Pre-fix, the laundering
/// scenario was: evil's save mutates the registry; the failure
/// truncates the corresponding ActorSave entry; a follow-up read
/// observes the corrupted state without any txlog evidence of the
/// mutation. With F1, the rollback restores the pre-call state.
#[test]
fn f1_failed_subcall_save_rolls_back_state_mutation() {
    let mut reg = MemRegistry::new();
    // X.recv: load (mark X), save (write same state back, unmark),
    // then verify(0) (throw). The save+throw is the laundering
    // pattern from the audit — without F1, save's registry write
    // would persist after the rollback.
    //
    // We can't easily mutate the state via the script (Dict-manip
    // would dwarf the test), so we check the rollback by comparing
    // the state root before and after. The save writes the SAME
    // state, so the root doesn't change observably from that alone
    // — but the test verifies the ENTIRE path works, including the
    // F2-shaped TxEntry::ActorSave being truncated from the txlog
    // and the registry mark being rolled back.
    let evil_recv = Program::new()
        .load()
        .save()
        .push_int(0u64)
        .verify()
        .to_bytecode();
    let x_id = deploy_recv(&mut reg, evil_recv, 10_000);
    let root_before = crate::state_root(reg.actor(&x_id).expect("X exists").state.as_ref().expect("present"));

    let a_recv = Program::new()
        .push_int(0u64)
        .push_int(5_000u64)
        .push_int(0u64)
        .push_int(RECV_METHOD)
        .push_str(String::from(x_id.to_hash().to_vec()))
        .call()
        .drop_()
        .to_bytecode();
    let a_id = deploy_recv(&mut reg, a_recv, 10_000);

    let block = BlockContext { height: 100 };
    let msg = Message {
        target: a_id,
        method: RECV_METHOD,
        caller: None,
        anchor: Anchor([0x42; 32]),
        payload: Vec::new(),
        gas: 100_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let result = VM::execute_internal(dummy_header(), msg, &mut reg, &block)
        .expect("outer tx succeeds");

    // F1 invariants:
    let x = reg.actor(&x_id).expect("X must still exist after rollback");
    assert_eq!(
        crate::state_root(x.state.as_ref().expect("present")),
        root_before,
        "X's state must be the rolled-back pre-call root",
    );
    assert!(!reg.actor(&x_id).map_or(false, |a| a.is_checked_out()));

    // F2 / Phase-36 invariant: no ActorSave entry for X in the
    // txlog. The failed sub-call's ActorSave was truncated by
    // `fail_current_call`'s txlog rollback. A's recv didn't save
    // anything, so its txlog should contain no ActorSave at all
    // for either actor.
    let x_save_count = result
        .txlog
        .iter()
        .filter(|e| matches!(e,
            crate::tx::TxEntry::ActorSave { actor, .. } if actor == &x_id
        ))
        .count();
    assert_eq!(
        x_save_count, 0,
        "no ActorSave for X in the final txlog (the failed callee's entry was truncated)",
    );
}

/// Audit F3: a save-time failure (`MalformedActorState` from a
/// bad wrapper, etc.) must propagate as a call failure — and via
/// F1 the rollback restores the actor as if the save had never
/// been attempted. Pre-fix, a save error left the actor "stuck
/// loaded" until tx-end's Q6 self-destruct kicked in. Now: the
/// error propagates, the rollback runs, the actor survives.
///
/// Concrete trigger: evil's recv does `load; drop; push:0;
/// save`. The save sees `Int253(0)`, not a Dict, fails
/// `TypeNotDict`. Without F1, the actor was destroyed at tx end.
/// With F1, the failure rolls back and the actor survives.
#[test]
fn f3_save_failure_rolls_back_and_preserves_actor() {
    let mut reg = MemRegistry::new();
    // `save` now accepts any *portable* Value, so a non-Dict no longer
    // fails. Trigger the save failure with a non-portable value (a Merlin).
    let evil_recv = Program::new()
        .load()                                // check out state, [state]
        .drop_()                               // drop it (empty state is droppable)
        .push_str(String::from(b"x".to_vec())) // ["x"]
        .transcript()                          // [Merlin] — non-portable
        .save()                                // NonPortableInState → frame fails
        .to_bytecode();
    let x_id = deploy_recv(&mut reg, evil_recv, 10_000);
    let root_before = crate::state_root(reg.actor(&x_id).expect("X exists").state.as_ref().expect("present"));

    let a_recv = Program::new()
        .push_int(0u64)
        .push_int(5_000u64)
        .push_int(0u64)
        .push_int(RECV_METHOD)
        .push_str(String::from(x_id.to_hash().to_vec()))
        .call()
        .drop_()
        .to_bytecode();
    let a_id = deploy_recv(&mut reg, a_recv, 10_000);

    let block = BlockContext { height: 100 };
    let msg = Message {
        target: a_id,
        method: RECV_METHOD,
        caller: None,
        anchor: Anchor([0x42; 32]),
        payload: Vec::new(),
        gas: 100_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let _ = VM::execute_internal(dummy_header(), msg, &mut reg, &block)
        .expect("outer tx succeeds (failure swallowed into marker)");

    let x = reg
        .actor(&x_id)
        .expect("X must survive — save failure no longer destroys");
    assert_eq!(crate::state_root(x.state.as_ref().expect("present")), root_before);
    assert!(!reg.actor(&x_id).map_or(false, |a| a.is_checked_out()));
}

#[test]
fn save_emits_actorsave_with_full_state() {
    // op_save mutates the actor's persistent state and records a
    // structural effect: `TxEntry::ActorSave { actor, state }`.
    // The full state rides in the entry (symmetric with
    // `Output(Cell)` carrying the full cell); the merkle leaf
    // hashes `state.root()`. A thin state machine consuming the
    // TxLog can apply these in order to mutate the registry
    // without re-running the script.
    let mut reg = MemRegistry::new();
    // Recv: `load; save` — round-trip with no state change still
    // emits the ActorSave entry.
    let recv = Program::new().load().save().to_bytecode();
    let id = ActorID::Hash([0xab; 32]);
    let state = empty_state();
    let expected_root = crate::state_root(&state);
    reg.deploy(id.clone(), recv.clone(), state, 10_000, 0).expect("deploy");

    let mut vm = vm_for_actor(id.clone(), recv);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    let save = vm.txlog.iter().find_map(|e| match e {
        crate::tx::TxEntry::ActorSave { actor, state } =>
            Some((actor.clone(), crate::state_root(state))),
        _ => None,
    }).expect("ActorSave entry present");
    assert_eq!(save.0, id);
    // The state is whatever was loaded then re-saved — same root.
    assert_eq!(save.1, expected_root, "state.root() matches deployed state");
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
