//! End-to-end control-flow integration tests.
//!
//! These drive whole transactions through the delivery path
//! (`VM::execute_internal` via the `deliver` harness) and assert on the
//! **public effect log** (`TxEntry`s) — never on `CallFrame` / `CallKind`
//! / `vm.current_call`. Each test exercises a control-flow construct
//! (loop, if/else, break, call chain, failed-call rollback, re-entrancy,
//! setcode upgrade) and observes it the way the state machine does: by
//! the effects it leaves behind. See test architecture notes (Bucket C).

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::empty_state;
use crate::tx::TxEntry;

fn b() -> ScriptBuilder {
    ScriptBuilder::new()
}

/// The `Data` payloads emitted by a delivery, in order — the observable
/// trace of which `log` statements ran.
fn data_trace(log: &[TxEntry]) -> Vec<Vec<u8>> {
    log.iter()
        .filter_map(|e| match e {
            TxEntry::Data(d) => Some(d.clone()),
            _ => None,
        })
        .collect()
}

/// `log "<s>"` as a builder step — emits one `TxEntry::Data`.
fn log_str(p: ScriptBuilder, s: &str) -> ScriptBuilder {
    p.push_str(String::from(s.as_bytes().to_vec())).log()
}

// ── Loops ────────────────────────────────────────────────────────────

#[test]
fn while_loop_emits_one_effect_per_iteration() {
    // recv: n=3; while (n) { log "x"; n-- }; drop n.
    let recv = b()
        .push_int(3u64)
        .build_while(|p| p.dup_k(0), |p| log_str(p, "x").push_int(-1i64).add())
        .drop_()
        .to_bytecode();
    let mut reg = MemRegistry::new();
    let id = deploy_actor(&mut reg, recv);
    let log = deliver(&mut reg, msg_to(id));
    assert_eq!(
        data_trace(&log),
        vec![b"x".to_vec(); 3],
        "one Data per loop iteration"
    );
}

#[test]
fn break_exits_loop_after_first_iteration() {
    // recv: loop { log "once"; break }.
    let recv = b()
        .build_loop(|p| log_str(p, "once").build_break())
        .to_bytecode();
    let mut reg = MemRegistry::new();
    let id = deploy_actor(&mut reg, recv);
    let log = deliver(&mut reg, msg_to(id));
    assert_eq!(
        data_trace(&log),
        vec![b"once".to_vec()],
        "break stops after one pass"
    );
}

#[test]
fn continue_skips_rest_of_body_each_iteration() {
    // recv: n=2; while (n) { n--; continue; log "skipped" }.
    // `continue` jumps past the log every time → zero Data effects.
    let recv = b()
        .push_int(2u64)
        .build_while(
            |p| p.dup_k(0),
            |p| log_str(p.push_int(-1i64).add().build_continue(), "skipped"),
        )
        .drop_()
        .to_bytecode();
    let mut reg = MemRegistry::new();
    let id = deploy_actor(&mut reg, recv);
    let log = deliver(&mut reg, msg_to(id));
    assert!(
        data_trace(&log).is_empty(),
        "continue skips the log every iteration"
    );
}

// ── Branching ────────────────────────────────────────────────────────

#[test]
fn if_else_takes_branch_by_selector_arg() {
    // recv reads its top-of-stack arg: if truthy log "then" else "else".
    let recv = b()
        .build_if_else(|p| log_str(p, "then"), |p| log_str(p, "else"))
        .to_bytecode();
    let mut reg = MemRegistry::new();
    let id = deploy_actor(&mut reg, recv);

    // Deliver with a truthy payload arg, then a falsy one.
    let truthy = msg_with_sel(id.clone(), 1);
    let falsy = msg_with_sel(id, 0);
    assert_eq!(
        data_trace(&deliver(&mut reg, truthy)),
        vec![b"then".to_vec()]
    );
    assert_eq!(
        data_trace(&deliver(&mut reg, falsy)),
        vec![b"else".to_vec()]
    );
}

// ── Calls ────────────────────────────────────────────────────────────

#[test]
fn call_chain_interleaves_callee_then_caller_effects() {
    // B.recv logs "B". A.recv calls B (drop the [count,success] markers)
    // then logs "A". The log must contain B's effect before A's, and the
    // `call` itself emits nothing.
    let mut reg = MemRegistry::new();
    let b_recv = log_str(b(), "B").to_bytecode();
    let b_id = deploy_actor(&mut reg, b_recv);

    let a_recv = {
        let mut p = call_to(&b_id);
        p = p.drop_().drop_(); // discard B's [count, success]
        log_str(p, "A").to_bytecode()
    };
    let a_id = deploy_actor(&mut reg, a_recv);

    let log = deliver(&mut reg, msg_to(a_id));
    assert_eq!(
        data_trace(&log),
        vec![b"B".to_vec(), b"A".to_vec()],
        "callee then caller"
    );
    // No Send/Receive/Output spawned by the intra-tx call itself.
    assert_eq!(
        log.iter().filter(|e| matches!(e, TxEntry::Send(_))).count(),
        0
    );
}

#[test]
fn failed_subcall_effects_roll_back_but_caller_continues() {
    // B.recv logs "B" then hard-fails (`verify 0`). A.recv calls B
    // (failure → `0` marker, drop it), then logs "A". B's "B" Data must
    // be rolled back; only "A" survives — the canonical effect-rollback
    // control-flow case.
    let mut reg = MemRegistry::new();
    let b_recv = log_str(b(), "B").push_int(0u64).verify().to_bytecode();
    let b_id = deploy_actor(&mut reg, b_recv);

    let a_recv = {
        let p = call_to(&b_id).drop_(); // failed call pushes a single `0` marker
        log_str(p, "A").to_bytecode()
    };
    let a_id = deploy_actor(&mut reg, a_recv);

    let log = deliver(&mut reg, msg_to(a_id));
    assert_eq!(
        data_trace(&log),
        vec![b"A".to_vec()],
        "B's effect rolled back, A's survives"
    );
}

// ── Re-entrancy ──────────────────────────────────────────────────────

#[test]
fn reentrant_call_runs_and_its_effect_is_observable() {
    // A.recv (method 0) calls A.method1; A never holds its state, so the
    // re-entry is permitted (ADR 0017) and A.method1's "R" log appears.
    let mut reg = MemRegistry::new();
    let a_id = ActorID::Hash([0xaa; 32]);
    let a_method1 = log_str(b(), "R").to_bytecode();
    let a_recv = call_with_sel(&a_id, 1).drop_().drop_().to_bytecode();
    let a_code = dispatch_code(&[(0, a_recv.clone()), (1, a_method1)]);
    reg.deploy(a_id.clone(), a_code, empty_state(), 1_000_000)
        .expect("deploy");

    let log = deliver(&mut reg, msg_with_sel(a_id, 0));
    assert!(
        data_trace(&log).contains(&b"R".to_vec()),
        "re-entrant method ran"
    );
}

// ── setcode upgrade ──────────────────────────────────────────────────

#[test]
fn setcode_upgrade_changes_dispatched_behavior() {
    // Method 0 (recv) logs "v1". Method 1 replaces the code with C2 whose
    // recv logs "v2". Deliver recv (→ "v1"), deliver the upgrade, deliver
    // recv again (→ "v2"). Pins that setcode changes future dispatch.
    let mut reg = MemRegistry::new();
    // C2 is the full replacement code (a 1-method dispatch blob, so it
    // consumes the selector just like the original). recv_v1 is the
    // method-0 handler inside the original blob.
    let c2 = dispatch_code(&[(0, log_str(b(), "v2").to_bytecode())]);
    let recv_v1 = log_str(b(), "v1").to_bytecode();
    // Method 1: push the new code string, setcode.
    let upgrade = b()
        .push_str(String::from(c2.clone()))
        .setcode()
        .to_bytecode();
    let id = ActorID::Hash([0xcc; 32]);
    let code = dispatch_code(&[(0, recv_v1), (1, upgrade)]);
    reg.deploy(id.clone(), code, empty_state(), 1_000_000)
        .expect("deploy");

    assert_eq!(
        data_trace(&deliver(&mut reg, msg_with_sel(id.clone(), 0))),
        vec![b"v1".to_vec()]
    );
    // Run the upgrade (emits a SetCode effect, no Data).
    let up_log = deliver(&mut reg, msg_with_sel(id.clone(), 1));
    assert!(
        up_log.iter().any(|e| matches!(e, TxEntry::SetCode { .. })),
        "upgrade emits SetCode"
    );
    // Note: the in-memory registry's working copy is mutated by setcode,
    // so a follow-up delivery dispatches the new code.
    assert_eq!(
        data_trace(&deliver(&mut reg, msg_with_sel(id, 0))),
        vec![b"v2".to_vec()]
    );
}
