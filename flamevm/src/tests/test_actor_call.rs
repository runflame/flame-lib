//! Tests for op_call, re-entrancy guard, and call-frame semantics.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::{empty_state, state_root, ActorID, ActorRegistry, Int253};

/// Helper: deploys an actor whose `recv` method runs `script`.
/// Derives the actor's id from the script bytes (treats `script`
/// as a stand-in for the constructor — real deployments would
/// hash the actual constructor that produces this state).
fn deploy_recv(reg: &mut MemRegistry, script: Vec<u8>, capacity: u64) -> ActorID {
    let id = ActorID::Hash(ActorID::Constructor(script.clone()).to_hash());
    reg.deploy(id.clone(), script, empty_state(), capacity)
        .expect("deploy");
    id
}

/// Helper: builds a VM running `script` under the given actor's
/// InternalRoot frame, with all the actor-context plumbing.
fn vm_for_actor(actor: ActorID, script: Vec<u8>) -> VM {
    let kind = CallKind::InternalRoot {
        actor,
        caller: None,
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script)
                .expect("script parses")
                .into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        )
        .with_anchor(Anchor([0u8; 32])),
    )
}

/// Helper: builds bytecode that invokes `op_call` with `k=0` args
/// (plain single-action callee — no selector). Spec stack
/// (bottom→top): `k gas addr call`.
fn call_script(target: &ActorID, gas: u64) -> Vec<u8> {
    ScriptBuilder::new()
        .push_int(0u64) // k = 0 args
        .push_int(gas) // gas
        .push_str(String::from(target.to_hash().to_vec())) // addr (32-byte)
        .call()
        .to_bytecode()
}

/// Helper: like [`call_script`] but passes `sel` as the single
/// (topmost) argument — the ADR 0020 selector convention for
/// dispatch-coded callees.
fn call_with_selector(target: &ActorID, sel: u64, gas: u64) -> Vec<u8> {
    ScriptBuilder::new()
        .push_int(sel) // selector arg (top)
        .push_int(1u64) // k = 1 arg
        .push_int(gas)
        .push_str(String::from(target.to_hash().to_vec()))
        .call()
        .to_bytecode()
}

/// `nop`-only recv — short fixture used when the test only cares
/// about call-frame mechanics, not the callee body.
fn nop_recv() -> Vec<u8> {
    ScriptBuilder::new().nop().to_bytecode()
}

#[test]
fn call_rejects_nested_nonportable_argument() {
    let mut reg = MemRegistry::new();
    let callee = deploy_recv(&mut reg, nop_recv(), 100);
    let caller = deploy_recv(
        &mut reg,
        ScriptBuilder::new().nop().nop().to_bytecode(),
        100,
    );
    let mut vm = vm_for_actor(caller, ScriptBuilder::new().call().to_bytecode());
    let mut inner = Dict::new();
    inner.insert(
        Int253::ZERO,
        Value::ClearToken(ClearToken::new(Int253::from(-1i64), FLAME_FLAVOR)),
    );
    let mut outer = Dict::new();
    outer.insert(Int253::ZERO, Value::Dict(inner));
    vm.current_call.stack = vec![
        Value::Dict(outer),
        Value::Int253(Int253::ONE),
        Value::Int253(Int253::from(10_000u64)),
        Value::String(String::from(callee.to_hash().to_vec())),
    ];

    let err = vm.step_internal_with_registry(&mut reg).unwrap_err();
    assert!(matches!(err, VMError::NonPortableInCall));
}

#[test]
fn call_a_to_b_creates_new_frame_with_callee_identity() {
    let mut reg = MemRegistry::new();
    // B: just `nop` so it does nothing then frame exits clean.
    let b = deploy_recv(&mut reg, nop_recv(), 1_000);

    // A: call B; expect 0 results.
    let a_script = call_script(&b, 10_000);
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
    let a_script = call_script(&b, 10_000);
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
    reg.deploy(
        id.clone(),
        ScriptBuilder::new().nop().to_bytecode(),
        empty_state(),
        10_000,
    )
    .expect("deploy");
    // Check the state out, as a live frame mid-update would.
    reg.actor_mut(&id).expect("present").state = None;

    let script = call_script(&id, 10_000);
    let mut vm = vm_for_actor(id.clone(), script);
    while vm.step_internal_with_registry(&mut reg).is_ok() {
        if vm.current_call.stack.len() == 1 {
            match &vm.current_call.stack[0] {
                Value::Int253(i) => {
                    assert_eq!(*i, Int253::from(0u64), "block marker");
                    assert_eq!(
                        vm.txlog.len(),
                        1,
                        "blocked re-entry must not emit side effects"
                    );
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
    let a_method1 = ScriptBuilder::new()
        .push_str(String::from(b"reentered".to_vec()))
        .log()
        .to_bytecode();
    // A.recv (method 0): call B, drop B's [count, success].
    let a_recv = {
        let mut p = ScriptBuilder::parse(&call_script(&b_id, 200_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    // A's code: a dispatch blob — method 0 → a_recv, method 1 → a_method1.
    let a_code = dispatch_code(&[(0, a_recv.clone()), (1, a_method1)]);
    reg.deploy(a_id.clone(), a_code, empty_state(), 1_000_000)
        .expect("deploy A");

    // B.recv: call A.method1, drop A's [count, success].
    let b_recv = {
        let mut p = ScriptBuilder::parse(&call_with_selector(&a_id, 1, 100_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    // B has only recv (method 0) → its code is the recv script directly.
    reg.deploy(b_id.clone(), b_recv, empty_state(), 1_000_000)
        .expect("deploy B");

    let mut vm = vm_for_actor(a_id, a_recv);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    // A.method1 ran via the re-entry → its Data entry is in the txlog.
    let logged = vm
        .txlog
        .iter()
        .any(|e| -> bool { matches!(e, TxEntry::Data(_)) });
    assert!(logged, "re-entrant A.method1 must have run (and logged)");
}

#[test]
fn sibling_calls_to_same_actor_allowed_after_return() {
    // A calls B, B returns 0; A calls B again — the second call is
    // not re-entrancy because B isn't on the live call stack at
    // the time of the second call.
    let mut reg = MemRegistry::new();
    // B: `push:0; return` — returns 0 results.
    let b_recv = ScriptBuilder::new().push_int(0u64).return_().to_bytecode();
    let b = deploy_recv(&mut reg, b_recv, 1_000);

    // A: call B; drop returned `[count=0, success=1]`; call B again;
    // drop those two markers — leaves an empty stack for the implicit
    // root finish_call.
    let a_script = {
        let mut p = ScriptBuilder::parse(&call_script(&b, 5_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.push_instr(Instruction::Drop);
        let second = ScriptBuilder::parse(&call_script(&b, 5_000)).expect("parse");
        for i in second.into_instructions() {
            p.push_instr(i);
        }
        p.push_instr(Instruction::Drop);
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    let a = deploy_recv(&mut reg, a_script.clone(), 100_000);

    let mut vm = vm_for_actor(a, a_script);
    // Step the whole way — no errors, no panic.
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}
}

/// Call-entry debit: granting more gas than the caller has remaining
/// hard-fails OutOfGas at the call site (no frame is created).
#[test]
fn call_grant_exceeding_caller_budget_is_out_of_gas() {
    let mut reg = MemRegistry::new();
    let b = deploy_recv(&mut reg, nop_recv(), 1_000);
    // Caller budget 100; grant 5_000 to the callee.
    let a_script = call_script(&b, 5_000);
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);
    let kind = CallKind::InternalRoot {
        actor: a,
        caller: None,
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&a_script).unwrap().into_instructions(),
            kind,
            100,
            0,
            0,
        )
        .with_anchor(Anchor([0u8; 32])),
    );
    let err = loop {
        match vm.step_internal_with_registry(&mut reg) {
            Ok(true) => continue,
            Ok(false) => panic!("must error before completing"),
            Err(e) => break e,
        }
    };
    assert!(matches!(err, VMError::OutOfGas));
}

/// Leftover-gas refund: after a child call returns cleanly, the parent's
/// spent gas equals (grant − child leftover) + parent's own instructions
/// — not the full grant.
#[test]
fn call_refunds_leftover_gas_to_caller() {
    let mut reg = MemRegistry::new();
    // Child: push:0 return — 2 instructions ≈ 2 gas of a 5_000 grant.
    let b_recv = ScriptBuilder::new().push_int(0u64).return_().to_bytecode();
    let b = deploy_recv(&mut reg, b_recv, 1_000);
    let a_script = {
        let mut p = ScriptBuilder::parse(&call_script(&b, 5_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);
    let mut vm = vm_for_actor(a, a_script);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}
    // Parent budget 1_000_000: a handful of own instructions plus ~2 gas
    // consumed by the child. Far below the 5_000 grant.
    let remaining = vm.current_call.gas_limit - vm.current_call.gas_used;
    assert!(
        remaining > 1_000_000 - 100,
        "leftover grant must be refunded; remaining = {}",
        remaining
    );
}

/// Read-only re-entrancy is not expressible (the strongest property of
/// the checkout lock): a re-entrant *view* method that `load`s the
/// mid-update state is blocked exactly like a writer — there is no
/// peek-state path. A.recv loads, then calls B; B calls A.method1
/// (which `load`s); the inner load surfaces ActorEmpty → B sees `0`.
#[test]
fn reentrant_view_of_mid_update_state_is_blocked() {
    let mut reg = MemRegistry::new();
    let a_id = ActorID::Hash([0xaa; 32]);
    let b_id = ActorID::Hash([0xbb; 32]);

    // A.method1 (a "view"): load state — never reached, A holds the lock.
    let a_method1 = ScriptBuilder::new().load().save().to_bytecode();
    // A.recv: load (acquire lock), call B.recv (succeeds → 2 markers,
    // drop both), then save the held state back so A exits cleanly.
    let a_recv = {
        let mut p = ScriptBuilder::new().load().to_bytecode();
        let mut q = ScriptBuilder::parse(&call_script(&b_id, 200_000)).expect("parse");
        q.push_instr(Instruction::Drop);
        q.push_instr(Instruction::Drop);
        p.extend_from_slice(&q.to_bytecode());
        p.extend_from_slice(&ScriptBuilder::new().save().to_bytecode());
        p
    };
    let a_code = dispatch_code(&[(0, a_recv.clone()), (1, a_method1)]);
    reg.deploy(a_id.clone(), a_code, empty_state(), 1_000_000)
        .expect("deploy A");

    // B.recv: call A.method1 — blocked (A checked out) → single `0`
    // marker, drop it once; B exits cleanly.
    let b_recv = {
        let mut p = ScriptBuilder::parse(&call_with_selector(&a_id, 1, 100_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    reg.deploy(b_id.clone(), b_recv, empty_state(), 1_000_000)
        .expect("deploy B");

    let mut vm = vm_for_actor(a_id, a_recv);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}
    // No ActorSave from A.method1 — the inner load was blocked, so the
    // view re-entry produced no state mutation. A's own recv save is the
    // only ActorSave.
    let a_saves = vm
        .txlog
        .iter()
        .filter(|e| matches!(e, TxEntry::ActorSave { .. }))
        .count();
    assert_eq!(
        a_saves, 1,
        "only A.recv's save; the view re-entry was blocked"
    );
}

/// Call depth is structurally capped: a self-recursing actor stops at
/// MAX_CALL_DEPTH with a `0` marker, not an unbounded heap blow-up.
#[test]
fn call_depth_is_capped() {
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0xcc; 32]);
    // recv: call SELF (no state load → re-entry permitted), drop markers.
    let recv = {
        let mut p = ScriptBuilder::parse(&call_script(&id, 900_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    reg.deploy(id.clone(), recv.clone(), empty_state(), 1_000_000)
        .expect("deploy");
    let mut vm = vm_for_actor(id, recv);
    // Must TERMINATE (depth cap bottoms out the recursion, which then
    // unwinds via failure markers) rather than hang or blow the stack.
    // Tolerate either a clean finish or a propagated error — the point
    // is that there is no unbounded recursion.
    let mut steps = 0;
    while let Ok(true) = vm.step_internal_with_registry(&mut reg) {
        steps += 1;
        assert!(steps < 5_000_000, "must terminate via depth cap");
    }
}

/// setcode end-to-end: deploy A with code C1 (method 0 logs "v1",
/// method 1 self-upgrades to C2), drive call(0) → call(1) → call(0)
/// in ONE tx. The third call must run C2 (logs "v2"), the txlog must
/// carry the SetCode entry, and the registry must hold C2. Labels are
/// collected fresh per frame (no caching — gas must not depend on
/// cache state), so the upgraded code dispatches from scratch.
#[test]
fn setcode_upgrade_end_to_end() {
    let mut reg = MemRegistry::new();
    let a_id = ActorID::Hash([0x5c; 32]);

    // C2: method 0 logs "v2"; padded with nops so its label positions
    // differ from C1's (staleness detector).
    let v2_handler = ScriptBuilder::new()
        .nop()
        .nop()
        .nop()
        .push_str(String::from(b"v2".to_vec()))
        .log()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let c2 = dispatch_code(&[(0, v2_handler)]);

    // C1: method 0 logs "v1"; method 1 replaces code with C2.
    let v1_handler = ScriptBuilder::new()
        .push_str(String::from(b"v1".to_vec()))
        .log()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let upgrade_handler = ScriptBuilder::new()
        .push_str(String::from(c2.clone()))
        .setcode()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let c1 = dispatch_code(&[(0, v1_handler), (1, upgrade_handler)]);
    reg.deploy(a_id.clone(), c1, empty_state(), 1_000_000)
        .expect("deploy A");

    // Driver R: call A.0, call A.1 (upgrade), call A.0 — drop markers.
    let mut driver = ScriptBuilder::parse(&call_with_selector(&a_id, 0, 100_000)).expect("parse");
    driver.push_instr(Instruction::Drop);
    driver.push_instr(Instruction::Drop);
    for instr in ScriptBuilder::parse(&call_with_selector(&a_id, 1, 100_000))
        .expect("parse")
        .into_instructions()
    {
        driver.push_instr(instr);
    }
    driver.push_instr(Instruction::Drop);
    driver.push_instr(Instruction::Drop);
    for instr in ScriptBuilder::parse(&call_with_selector(&a_id, 0, 100_000))
        .expect("parse")
        .into_instructions()
    {
        driver.push_instr(instr);
    }
    driver.push_instr(Instruction::Drop);
    driver.push_instr(Instruction::Drop);
    let script = driver.to_bytecode();

    let r_id = ActorID::Hash([0xd1; 32]);
    let mut vm = vm_for_actor(r_id, script);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    // Effects: Data("v1"), SetCode, Data("v2") in that order.
    let datas: Vec<&[u8]> = vm
        .txlog
        .iter()
        .filter_map(|e| match e {
            TxEntry::Data(d) => Some(d.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(
        datas,
        vec![b"v1".as_slice(), b"v2".as_slice()],
        "old code then NEW code ran"
    );
    assert!(
        vm.txlog
            .iter()
            .any(|e| matches!(e, TxEntry::SetCode { .. })),
        "SetCode effect recorded"
    );
    assert_eq!(
        reg.actor(&a_id).unwrap().code,
        c2,
        "registry holds the new code"
    );
}

/// A failed sub-call burns the entire gas grant (spec §gas): the parent
/// gets no refund when the child fails mid-execution.
#[test]
fn failed_call_burns_full_grant() {
    let mut reg = MemRegistry::new();
    // B: runs a couple of instructions, then fails.
    let b_code = ScriptBuilder::new()
        .nop()
        .push_int(0u64)
        .verify()
        .to_bytecode();
    let b = deploy_recv(&mut reg, b_code, 1_000);
    // A: call B with a 5_000 grant, drop the single failure marker.
    let a_script = {
        let mut p = ScriptBuilder::parse(&call_script(&b, 5_000)).expect("parse");
        p.push_instr(Instruction::Drop);
        p.to_bytecode()
    };
    let a = deploy_recv(&mut reg, a_script.clone(), 10_000);
    let mut vm = vm_for_actor(a, a_script);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}
    // Parent budget 1_000_000 (vm_for_actor): the full 5_000 grant is
    // burned (no refund on failure) plus a handful of own instructions.
    let remaining = vm.current_call.gas_limit - vm.current_call.gas_used;
    assert!(
        remaining <= 1_000_000 - 5_000,
        "grant must not be refunded: {}",
        remaining
    );
    assert!(
        remaining > 1_000_000 - 5_100,
        "only the grant + own instrs spent: {}",
        remaining
    );
}

/// The recursion cap binds at exactly MAX_CALL_DEPTH parents on the
/// call stack (an off-by-one would silently change the bound). The
/// recursive grant is `gas − 200` (computed via the `gas` opcode) so
/// each level can afford the next — depth, not gas, is the binding
/// constraint.
#[test]
fn call_depth_caps_at_exactly_max() {
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0xce; 32]);
    // recv: args…k=0, gas−200, bytes=0, method=0, addr → call; drop marker.
    let recv = ScriptBuilder::new()
        .push_int(0u64) // k = 0 args
        .gas()
        .push_int(-200i64)
        .add() // grant = remaining − 200
        .push_str(String::from(id.to_hash().to_vec()))
        .call()
        .drop_()
        .to_bytecode();
    reg.deploy(id.clone(), recv.clone(), empty_state(), 1_000_000)
        .expect("deploy");
    let mut vm = vm_for_actor(id.clone(), recv);
    let mut max_depth = 0;
    while let Ok(true) = vm.step_internal_with_registry(&mut reg) {
        max_depth = max_depth.max(vm.call_stack.len());
    }
    assert_eq!(
        max_depth, MAX_CALL_DEPTH,
        "cap binds exactly at the constant"
    );
}

#[test]
fn call_without_registry_errors() {
    let mut reg = MemRegistry::new();
    let b = deploy_recv(&mut reg, nop_recv(), 1_000);
    let a_script = call_script(&b, 10_000);
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
    let evil_recv = ScriptBuilder::new()
        .load()
        .push_int(0u64)
        .verify()
        .to_bytecode();
    let x_id = deploy_recv(&mut reg, evil_recv, 10_000);

    // A.recv: call X (k=0, gas=5_000), drop the failure marker so
    // A's frame exits clean (stack empty → no StackNotClean).
    let a_recv = ScriptBuilder::new()
        .push_int(0u64) // k = 0 args
        .push_int(5_000u64) // gas
        .push_str(String::from(x_id.to_hash().to_vec())) // addr
        .call()
        .drop_() // drop failure marker (0)
        .to_bytecode();
    let a_id = deploy_recv(&mut reg, a_recv, 10_000);

    // Run via execute_internal targeting A.
    let block = BlockContext { height: 100 };
    let msg = Message::new(
        a_id,
        None,
        Anchor([0x42; 32]),
        Vec::new(),
        100_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
    let _ = VM::execute_internal(dummy_header(), msg, &mut reg, &block)
        .expect("outer tx succeeds (A drops the inner failure marker)");

    // F1 invariant: X survives the failed sub-call. Pre-fix, X's
    // mark would have persisted past the rollback boundary, and
    // tx-end `commit_tx_destructions` would have destroyed X.
    assert!(
        reg.exists(&x_id),
        "X must still exist after the failed sub-call"
    );
    assert!(
        !reg.actor(&x_id).is_some_and(|a| a.is_checked_out()),
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
    let evil_recv = ScriptBuilder::new()
        .load()
        .save()
        .push_int(0u64)
        .verify()
        .to_bytecode();
    let x_id = deploy_recv(&mut reg, evil_recv, 10_000);
    let root_before = state_root(
        reg.actor(&x_id)
            .expect("X exists")
            .state
            .as_ref()
            .expect("present"),
    );

    let a_recv = ScriptBuilder::new()
        .push_int(0u64)
        .push_int(5_000u64)
        .push_str(String::from(x_id.to_hash().to_vec()))
        .call()
        .drop_()
        .to_bytecode();
    let a_id = deploy_recv(&mut reg, a_recv, 10_000);

    let block = BlockContext { height: 100 };
    let msg = Message::new(
        a_id,
        None,
        Anchor([0x42; 32]),
        Vec::new(),
        100_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
    let result =
        VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("outer tx succeeds");

    // F1 invariants:
    let x = reg.actor(&x_id).expect("X must still exist after rollback");
    assert_eq!(
        state_root(x.state.as_ref().expect("present")),
        root_before,
        "X's state must be the rolled-back pre-call root",
    );
    assert!(!reg.actor(&x_id).is_some_and(|a| a.is_checked_out()));

    // F2 / Phase-36 invariant: no ActorSave entry for X in the
    // txlog. The failed sub-call's ActorSave was truncated by
    // `fail_current_call`'s txlog rollback. A's recv didn't save
    // anything, so its txlog should contain no ActorSave at all
    // for either actor.
    let x_save_count = result
        .txlog
        .iter()
        .filter(|e| {
            matches!(e,
                TxEntry::ActorSave { actor, .. } if actor == &x_id
            )
        })
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
    let evil_recv = ScriptBuilder::new()
        .load() // check out state, [state]
        .drop_() // drop it (empty state is droppable)
        .push_str(String::from(b"x".to_vec())) // ["x"]
        .transcript() // [Merlin] — non-portable
        .save() // NonPortableInState → frame fails
        .to_bytecode();
    let x_id = deploy_recv(&mut reg, evil_recv, 10_000);
    let root_before = state_root(
        reg.actor(&x_id)
            .expect("X exists")
            .state
            .as_ref()
            .expect("present"),
    );

    let a_recv = ScriptBuilder::new()
        .push_int(0u64)
        .push_int(5_000u64)
        .push_str(String::from(x_id.to_hash().to_vec()))
        .call()
        .drop_()
        .to_bytecode();
    let a_id = deploy_recv(&mut reg, a_recv, 10_000);

    let block = BlockContext { height: 100 };
    let msg = Message::new(
        a_id,
        None,
        Anchor([0x42; 32]),
        Vec::new(),
        100_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
    let _ = VM::execute_internal(dummy_header(), msg, &mut reg, &block)
        .expect("outer tx succeeds (failure swallowed into marker)");

    let x = reg
        .actor(&x_id)
        .expect("X must survive — save failure no longer destroys");
    assert_eq!(state_root(x.state.as_ref().expect("present")), root_before);
    assert!(!reg.actor(&x_id).is_some_and(|a| a.is_checked_out()));
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
    let recv = ScriptBuilder::new().load().save().to_bytecode();
    let id = ActorID::Hash([0xab; 32]);
    let state = empty_state();
    let expected_root = state_root(&state);
    reg.deploy(id.clone(), recv.clone(), state, 10_000)
        .expect("deploy");

    let mut vm = vm_for_actor(id.clone(), recv);
    while vm.step_internal_with_registry(&mut reg).expect("step ok") {}

    let save = vm
        .txlog
        .iter()
        .find_map(|e| match e {
            TxEntry::ActorSave { actor, state } => Some((actor.clone(), state_root(state))),
            _ => None,
        })
        .expect("ActorSave entry present");
    assert_eq!(save.0, id);
    // The state is whatever was loaded then re-saved — same root.
    assert_eq!(save.1, expected_root, "state.root() matches deployed state");
    let save_count = vm
        .txlog
        .iter()
        .filter(|e| matches!(e, TxEntry::ActorSave { .. }))
        .count();
    assert_eq!(save_count, 1);
}

#[test]
fn call_to_unknown_actor_rejected_with_marker() {
    let mut reg = MemRegistry::new();
    // Caller exists; target does not — pre-frame registry lookup
    // fails → marker `0` on caller's stack. No side effects emitted.
    let ghost = ActorID::Hash([0xab; 32]);
    let a_script = call_script(&ghost, 10_000);
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
