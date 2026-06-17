//! Tests for op_load / op_save + tx-end self-destruct.

#![allow(unused_imports)]

use super::test_helpers::*;

use crate::{empty_state, ActorID, Dict, Env, Int253, Limits, VBYTE_MATURITY_BLOCKS};

/// Builds an empty state with a single `recv` method that runs the
/// caller-supplied bytes. Returns (state, id).
fn deploy_with_recv(reg: &mut MemRegistry, recv: Vec<u8>, vbytes: u64, height: u64)
    -> ActorID
{
    // The recv bytes ARE the actor's code; state starts empty. Id is
    // derived from the code (stand-in for the real constructor).
    let id = ActorID::Hash(ActorID::Constructor(recv.clone()).to_hash());
    reg.deploy(id.clone(), recv, empty_state(), vbytes, height).expect("deploy");
    id
}

/// Build a VM pre-loaded with an InternalRoot frame and arbitrary
/// inline script bytes (overrides what the registry would resolve).
/// Mirrors `vm_internal_with_actor` from test_helpers.
fn vm_for(actor: ActorID, script: Vec<u8>) -> VM {
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
        ).with_anchor(Anchor([0u8; 32])),
    )
}

/// Facade roundtrip (internal): deploy an actor with a `recv`, run a
/// Message through `Message::execute_tx` against a read-only `MemEnv`,
/// then `apply_changes`. Exercises lifecycle step 4.
#[test]
fn facade_internal_execute_tx_roundtrip() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);
    let mut env = MemEnv { registry: reg, height: 0 };
    let msg = Message {
        target: id,
        caller: None,
        anchor: Anchor([1u8; 32]),
        payload: Vec::new(),
        gas: 1_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let itx = msg
        .execute_tx(Limits { gas: 1_000_000, mem: 0 }, &env)
        .expect("execute_tx");
    // Header + Receive are emitted before the recv body runs.
    assert!(itx.log().entries().len() >= 2, "header + receive at minimum");
    assert!(itx.metrics().gas_used > 0, "internal metering reported via TxMetrics");
    // env was read-only during execution; apply effects afterwards (the
    // nop recv saves no state, so this replays nothing).
    env.apply_changes(itx.log());
}

/// Deploy-on-first-delivery edge: a second message to the same
/// Constructor-form target must NOT re-deploy or re-credit vbytes —
/// the actor (and its balance) persists from the first delivery.
#[test]
fn second_constructor_send_does_not_redeploy() {
    let mut reg = MemRegistry::new();
    let code = ScriptBuilder::new().nop().to_bytecode();
    let target = ActorID::Constructor(code.clone());
    let canonical = ActorID::Hash(target.to_hash());
    let mk = |vbytes| Message {
        target: target.clone(),
        caller: None,
        anchor: Anchor([0x07; 32]),
        payload: Vec::new(),
        gas: 10_000,
        vbytes,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let block = BlockContext { height: 1 };
    VM::execute_internal(dummy_header(), mk(777), &mut reg, &block).expect("first");
    VM::execute_internal(dummy_header(), mk(500), &mut reg, &block).expect("second");
    let a = reg.actor(&canonical).expect("deployed once");
    assert_eq!(a.vbytes, 777, "second delivery must not re-deploy or re-credit");
    assert_eq!(a.code, code);
}

/// Deploy-on-first-delivery edge: a zero-vbyte constructor send still
/// deploys (with an empty storage budget).
#[test]
fn constructor_send_with_zero_vbytes_deploys() {
    let mut reg = MemRegistry::new();
    let code = ScriptBuilder::new().nop().to_bytecode();
    let target = ActorID::Constructor(code.clone());
    let msg = Message {
        target: target.clone(),
        caller: None,
        anchor: Anchor([0x08; 32]),
        payload: Vec::new(),
        gas: 10_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("deploy+run");
    let a = reg.actor(&ActorID::Hash(target.to_hash())).expect("deployed");
    assert_eq!(a.vbytes, 0);
}

#[test]
fn load_checks_out_state() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);

    // Script: just `load`. Step once and inspect; don't run to
    // end (end-of-frame is checked clean-stack, which a bare
    // `load` would violate).
    let script = ScriptBuilder::new().load().to_bytecode();
    let mut vm = vm_for(id.clone(), script);
    vm.step_internal_with_registry(&mut reg).expect("load step");

    // Top of stack should be the state Value (here the empty initial
    // Dict), moved out of the actor.
    match vm.current_call.stack.last().expect("stack not empty") {
        Value::Dict(d) => assert!(d.is_empty(), "empty initial state"),
        _ => panic!("expected Dict"),
    }
    // The actor is now checked out (state moved to the stack) — its
    // presence is the re-entrancy lock.
    assert!(reg.actor(&id).expect("present").is_checked_out());
}

#[test]
fn load_then_save_round_trips() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);

    // Script: `load; save`.
    let script = ScriptBuilder::new().load().save().to_bytecode();
    let mut vm = vm_for(id.clone(), script);
    while vm.step_internal_with_registry(&mut reg).expect("step") {}

    // Stack drained by save.
    assert!(vm.current_call.stack.is_empty());
    // State checked back in.
    assert!(!reg.actor(&id).expect("present").is_checked_out());
}

#[test]
fn load_in_external_root_errors_actor_context() {
    let mut reg = MemRegistry::new();
    // ExternalRoot has no actor identity → require_actor errors.
    let kind = CallKind::ExternalRoot;
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::new().load().into_instructions(),
            kind,
            1000,
            0,
            0,
        ),
    );
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

#[test]
fn load_without_registry_errors_registry_unavailable() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);
    let mut vm = vm_for(id, ScriptBuilder::new().load().to_bytecode());
    // step_internal (no registry) hits the RegistryUnavailable
    // guard inside op_load.
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::RegistryUnavailable));
}

#[test]
fn second_load_errors_actor_empty() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);

    // Script: `load; load`. The second load finds the actor empty
    // (state already checked out) → ActorEmpty.
    let script = ScriptBuilder::new().load().load().to_bytecode();
    let mut vm = vm_for(id, script);
    assert!(vm.step_internal_with_registry(&mut reg).expect("first"));
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::ActorEmpty));
}

#[test]
fn load_against_checked_out_actor_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);
    // Simulate a sibling frame having checked out the state.
    reg.actor_mut(&id).expect("present").state = None;

    let mut vm = vm_for(id, ScriptBuilder::new().load().to_bytecode());
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::ActorEmpty));
}

#[test]
fn load_against_frozen_actor_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);
    // Force-freeze the actor.
    {
        let a = reg.actor_mut(&id).expect("present");
        a.vbytes = 0;
        a.frozen_since = Some(10);
    }

    let mut vm = vm_for(id, ScriptBuilder::new().load().to_bytecode());
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::ActorFrozen));
}

#[test]
fn save_without_load_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);

    // Script: push:0, dict, save. Save runs without a prior
    // load → SaveWithoutLoad error.
    let script = ScriptBuilder::new().push_int(0u64).dict().save().to_bytecode();
    let mut vm = vm_for(id, script);
    // 2 instructions before save: pushint8(0), dict.
    for i in 0..2 {
        vm.step_internal_with_registry(&mut reg)
            .unwrap_or_else(|e| panic!("pre-save step {} errored: {:?}", i, e));
    }
    let err = vm
        .step_internal_with_registry(&mut reg)
        .expect_err("save must error");
    assert!(matches!(err, VMError::SaveWithoutLoad), "got {:?}", err);
}

#[test]
fn save_accepts_arbitrary_portable_dict_shape() {
    // The VM no longer enforces the conventional `{0x00 → public,
    // 0x01 → private}` wrapper shape at `op_save`. Any portable
    // Dict is accepted; the shape is purely a script-side
    // convention. (Method dispatch via `resolve_method` consults
    // 0x00 — scripts that ignore the convention just won't be
    // dispatchable, but that's their choice.)
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000, 0);
    // Check the state out (as a prior op_load would) so save can move
    // a fresh Dict back in.
    reg.actor_mut(&id).expect("present").state = None;

    // Script: push:0; dict; save. Empty Dict, no wrapper shape.
    let script = ScriptBuilder::new().push_int(0u64).dict().save().to_bytecode();
    let mut vm = vm_for(id, script);
    for _ in 0..3 {
        vm.step_internal_with_registry(&mut reg).expect("step ok");
    }
    // Save succeeded; an ActorSave entry is in the txlog.
    let save_count = vm.txlog.iter()
        .filter(|e| matches!(e, TxEntry::ActorSave { .. }))
        .count();
    assert_eq!(save_count, 1);
}

#[test]
fn load_then_dismantle_self_destructs_and_queues_vbytes() {
    // Self-destruct end-to-end: recv `load`s its (droppable) state and
    // `drop`s it — explicitly destroying the state recursively leaves
    // the actor empty, which execute_internal's tx-end hook reaps,
    // queuing its vbytes for release at height + maturity.
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(
        &mut reg,
        ScriptBuilder::new().load().drop_().to_bytecode(),
        10_000,
        0,
    );
    assert!(reg.exists(&id));

    let block = BlockContext { height: 100 };
    let msg = Message {
        target: id.clone(),
        caller: None,
        anchor: Anchor([0x07; 32]),
        payload: Vec::new(),
        gas: 1_000_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("ok");

    assert!(!reg.exists(&id), "dismantled state self-destructs the actor");
    let release_at = 100 + VBYTE_MATURITY_BLOCKS;
    assert_eq!(reg.vbyte_pool().maturing.get(&release_at).copied().unwrap_or(0), 10_000);
}

/// Conservation: a non-zero token in actor state survives a
/// `load; save` round-trip **exactly once** — the registry's post-state
/// holds the same single token, neither dropped nor duplicated, despite
/// op_save's Rust-level deep clone into the txlog (audit p9).
#[test]
fn token_survives_load_save_roundtrip_exactly_once() {
    let mut reg = MemRegistry::new();
    let mut d = Dict::new();
    d.insert(
        Int253::from(0u64),
        Value::ClearToken(ClearToken::new(Int253::from(5u64), Int253::from(9u64))),
    );
    let state = Value::Dict(d);
    let recv = ScriptBuilder::new().load().save().to_bytecode();
    let id = ActorID::Hash(ActorID::Constructor(recv.clone()).to_hash());
    reg.deploy(id.clone(), recv, state, 10_000, 0).expect("deploy");

    let msg = Message {
        target: id.clone(),
        caller: None,
        anchor: Anchor([0x11; 32]),
        payload: Vec::new(),
        gas: 100_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("run");

    // Exactly one token of the same qty/flavor remains in the registry.
    match reg.actor(&id).unwrap().state.as_ref().unwrap() {
        Value::Dict(d) => {
            assert_eq!(d.len(), 1, "no extra entry");
            match d.get(&Int253::from(0u64)) {
                Some(Value::ClearToken(t)) => {
                    assert_eq!(t.qty, Int253::from(5u64));
                    assert_eq!(t.flv, Int253::from(9u64));
                }
                other => panic!("token vanished or mutated: {:?}", other),
            }
        }
        other => panic!("unexpected state shape: {:?}", other),
    }
}

/// Deploy-on-first-delivery: a message addressed to a Constructor-form
/// id instantiates the actor (code = constructor bytes, empty state,
/// funded by the message's vbytes), then dispatches into it.
#[test]
fn constructor_send_deploys_then_runs() {
    let mut reg = MemRegistry::new();
    let code = ScriptBuilder::new().nop().to_bytecode();
    let target = ActorID::Constructor(code.clone());
    let canonical = ActorID::Hash(target.to_hash());
    assert!(!reg.exists(&target));

    let msg = Message {
        target: target.clone(),
        caller: None,
        anchor: Anchor([0x07; 32]),
        payload: Vec::new(),
        gas: 10_000,
        vbytes: 777,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let block = BlockContext { height: 42 };
    VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("deploy+run");

    let a = reg.actor(&canonical).expect("actor deployed");
    assert_eq!(a.code, code);
    assert_eq!(a.vbytes, 777);
    assert_eq!(a.last_activation_height, 42);
}

/// A Hash-form target that doesn't exist still fails — only the
/// Constructor form carries deployable code.
#[test]
fn hash_send_to_unknown_actor_errors() {
    let mut reg = MemRegistry::new();
    let block = BlockContext { height: 0 };
    let err = VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block)
        .expect_err("unknown hash target must fail");
    assert!(matches!(err, VMError::ActorNotFound));
}

#[test]
fn dismantle_token_bearing_state_requires_retire() {
    // Tokens must be explicitly retired/spent, never dropped: a state
    // holding a live (non-zero) ClearToken can't be `drop`ped —
    // `TypeNotDroppable` (a zero-qty ClearToken shell would be
    // droppable). recv = `load; drop`.
    let mut reg = MemRegistry::new();
    let recv = ScriptBuilder::new().load().drop_().to_bytecode();
    // Token-bearing state: any Value — here a Dict holding a live ClearToken.
    let mut state_dict = Dict::new();
    state_dict.insert(
        Int253::from(0u64),
        Value::ClearToken(ClearToken::new(Int253::from(5u64), Int253::from(9u64))),
    );
    let state = Value::Dict(state_dict);
    let id = ActorID::Hash([0x09; 32]);
    reg.deploy(id.clone(), recv, state, 10_000, 0).expect("deploy");

    let block = BlockContext { height: 100 };
    let msg = Message {
        target: id.clone(),
        caller: None,
        anchor: Anchor([0x09; 32]),
        payload: Vec::new(),
        gas: 1_000_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let err = VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect_err("must error");
    assert!(matches!(err, VMError::TypeNotDroppable));
    assert!(reg.exists(&id), "rolled back — balance intact");
}

#[test]
fn load_without_discharge_errors_stack_not_clean() {
    // Forgetting to discharge a loaded state is caught loudly: the
    // Dict is left on the stack at frame end → StackNotClean (rolled
    // back). A missing `save` is never a silent self-destruct.
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().load().to_bytecode(), 10_000, 0);
    let block = BlockContext { height: 100 };
    let msg = Message {
        target: id.clone(),
        caller: None,
        anchor: Anchor([0x08; 32]),
        payload: Vec::new(),
        gas: 1_000_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let err = VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect_err("must error");
    assert!(matches!(err, VMError::StackNotClean));
    // Rolled back — the actor is intact and available.
    assert!(reg.exists(&id));
    assert!(!reg.actor(&id).expect("present").is_checked_out());
}

#[test]
fn load_followed_by_save_preserves_actor() {
    // BlockContext + ActorRegistry come in via test_helpers' wildcard.
    let mut reg = MemRegistry::new();
    // recv = `load; save` — full pair. No self-destruct.
    let id = deploy_with_recv(
        &mut reg,
        ScriptBuilder::new().load().save().to_bytecode(),
        10_000,
        0,
    );

    let block = BlockContext { height: 100 };
    let msg = Message {
        target: id.clone(),
        caller: None,
        anchor: Anchor([0x02; 32]),
        payload: Vec::new(),
        gas: 1_000_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("ok");

    // Actor still present; state checked back in (not self-destructed).
    assert!(reg.exists(&id));
    assert!(!reg.actor(&id).expect("present").is_checked_out());
}

// ── Receive (MessageID committed into Internal TxID) ───────────────────

/// `VM::execute_internal` emits `TxEntry::Receive(send_id)` as the
/// first effect after the Header so the Internal TxID commits to the
/// triggering Send's identity. Symmetric with `op_input` for external
/// transactions. Without this entry, an internal tx's TxID would say
/// nothing about which Send produced it.
///
/// MessageID is the canonical hash of the entire Send (anchor, target,
/// caller, method, payload, gas, vbytes, refund predicate) — not just
/// the anchor — so the comparison rebuilds the expected MessageID from
/// the originating `Message`.
#[test]
fn receive_committed_as_first_effect_after_header() {
    let mut reg = MemRegistry::new();
    // Minimal recv: a single `nop`. The script does nothing, but
    // execute_internal still pushes Header + Receive into the txlog.
    let id = deploy_with_recv(
        &mut reg,
        ScriptBuilder::new().nop().to_bytecode(),
        10_000,
        0,
    );
    let block = BlockContext { height: 100 };
    let known_anchor = [0xab; 32];
    let msg = Message {
        target: id.clone(),
        caller: None,
        anchor: Anchor(known_anchor),
        payload: Vec::new(),
        gas: 1_000_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    let expected_send_id = *msg.id().as_bytes();
    let result = VM::execute_internal(dummy_header(), msg, &mut reg, &block)
        .expect("execute_internal ok");

    // txlog[0] = Header, txlog[1] = Receive(send_id).
    assert!(result.txlog.len() >= 2, "txlog too short: {}", result.txlog.len());
    assert!(matches!(result.txlog[0], TxEntry::Header(_)));
    match &result.txlog[1] {
        TxEntry::Receive(send_id) => {
            assert_eq!(
                *send_id, expected_send_id,
                "Receive must carry the originating Message's MessageID"
            );
        }
        other => panic!("expected txlog[1] to be TxEntry::Receive, got {:?}", other),
    }
}

/// Two internal txs with identical scripts + identical (empty) actor
/// state mutations but DIFFERENT Send anchors must produce DIFFERENT
/// Internal TxIDs — because the Internal TxID merkle root binds to
/// the Receive entry. Regression guard: if `Receive` ever gets
/// dropped, both internal txs would hash to the same TxID, conflating
/// distinct sends in any state-machine indexer.
#[test]
fn receive_makes_internal_txid_bind_to_send_anchor() {
    fn run_with_anchor(anchor_bytes: [u8; 32]) -> TxID {
        let mut reg = MemRegistry::new();
        let id = deploy_with_recv(
            &mut reg,
            ScriptBuilder::new().nop().to_bytecode(),
            10_000,
            0,
        );
        let block = BlockContext { height: 100 };
        let msg = Message {
            target: id,
            caller: None,
            anchor: Anchor(anchor_bytes),
            payload: Vec::new(),
            gas: 1_000_000,
            vbytes: 0,
            refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
        };
        VM::execute_internal(dummy_header(), msg, &mut reg, &block)
            .expect("execute_internal ok")
            .txid
    }
    let txid_a = run_with_anchor([0x01; 32]);
    let txid_b = run_with_anchor([0x02; 32]);
    assert_ne!(
        txid_a.0, txid_b.0,
        "Internal TxID must distinguish runs by their triggering MessageID",
    );
}
