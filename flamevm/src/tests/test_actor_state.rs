//! Tests for op_load / op_save + tx-end self-destruct.

#![allow(unused_imports)]

use super::test_helpers::*;

use crate::{empty_state, ActorID, Dict, Int253, StoragePurchase};

/// Builds an empty state with a single `recv` method that runs the
/// caller-supplied bytes. Returns (state, id).
fn deploy_with_recv(reg: &mut MemRegistry, recv: Vec<u8>, capacity: u64) -> ActorID {
    // The recv bytes ARE the actor's code; state starts empty. Id is
    // derived from the code (stand-in for the real constructor).
    let id = ActorID::Hash(ActorID::Constructor(recv.clone()).to_hash());
    reg.deploy(id.clone(), recv, empty_state(), capacity)
        .expect("deploy");
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
        )
        .with_anchor(Anchor([0u8; 32])),
    )
}

/// Facade roundtrip (internal): deploy an actor with a `recv`, run a
/// Message through the direct checkpointed internal-execution facade.
#[test]
fn facade_internal_execute_tx_roundtrip() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);
    let mut reg = reg;
    let msg = Message::new(
        id,
        None,
        Anchor([1u8; 32]),
        Vec::new(),
        1_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
    let itx = msg
        .execute_tx(&mut reg, &BlockContext { height: 0 })
        .expect("execute_tx");
    // Header + Receive are emitted before the recv body runs.
    assert!(
        itx.log().entries().len() >= 2,
        "header + receive at minimum"
    );
    assert!(
        itx.metrics().gas_used > 0,
        "internal metering reported via TxMetrics"
    );
}

/// A provisional constructor can buy its own capacity, and later constructor-
/// form deliveries reuse the same actor instead of redeploying it.
#[test]
fn constructor_buys_storage_and_is_reused() {
    let mut reg = MemRegistry::new();
    reg.set_storage_quote(Some(StoragePurchase {
        fee_sparks: Int253::from(77u64),
        expiry_height: 52_500,
    }));
    let code = ScriptBuilder::new()
        .push_int(1_024u64)
        .addstorage()
        .drop_()
        .merge()
        .drop_()
        .drop_()
        .to_bytecode();
    let target = ActorID::Constructor(code.clone());
    let canonical = target.to_canonical();
    let mk = || {
        Message::new(
            target.clone(),
            None,
            Anchor([0x07; 32]),
            vec![Value::ClearToken(ClearToken::new(
                Int253::from(77u64),
                FLAME_FLAVOR,
            ))],
            10_000,
            Predicate::opaque(Predicate::unspendable_key()),
        )
        .expect("message payload is portable")
    };
    let block = BlockContext { height: 1 };
    let first = VM::execute_internal(dummy_header(), mk(), &mut reg, &block).unwrap();
    let second = VM::execute_internal(dummy_header(), mk(), &mut reg, &block).unwrap();
    let a = reg.actor(&canonical).expect("deployed once");
    assert_eq!(a.code, code);
    assert_eq!(a.capacity, 2_048);
    for result in [first, second] {
        assert!(result.txlog.iter().any(
            |entry| matches!(entry, TxEntry::StoragePurchase { actor, .. } if actor == &canonical)
        ));
    }
}

#[test]
fn load_checks_out_state() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);

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
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);

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
        CallFrame::new(ScriptBuilder::new().load().into_instructions(), kind, 1000),
    );
    let err = vm
        .step_internal_with_registry(&mut reg)
        .expect_err("must error");
    assert!(matches!(err, VMError::OpcodeRequiresActorContext));
}

#[test]
fn load_without_registry_errors_registry_unavailable() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);
    let mut vm = vm_for(id, ScriptBuilder::new().load().to_bytecode());
    // step_internal (no registry) hits the RegistryUnavailable
    // guard inside op_load.
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::RegistryUnavailable));
}

#[test]
fn second_load_errors_actor_empty() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);

    // Script: `load; load`. The second load finds the actor empty
    // (state already checked out) → ActorEmpty.
    let script = ScriptBuilder::new().load().load().to_bytecode();
    let mut vm = vm_for(id, script);
    assert!(vm.step_internal_with_registry(&mut reg).expect("first"));
    let err = vm
        .step_internal_with_registry(&mut reg)
        .expect_err("must error");
    assert!(matches!(err, VMError::ActorEmpty));
}

#[test]
fn load_against_checked_out_actor_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);
    // Simulate a sibling frame having checked out the state.
    reg.actor_mut(&id).expect("present").state = None;

    let mut vm = vm_for(id, ScriptBuilder::new().load().to_bytecode());
    let err = vm
        .step_internal_with_registry(&mut reg)
        .expect_err("must error");
    assert!(matches!(err, VMError::ActorEmpty));
}

#[test]
fn save_without_load_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);

    // Script: push:0, dict, save. Save runs without a prior
    // load → SaveWithoutLoad error.
    let script = ScriptBuilder::new()
        .push_int(0u64)
        .dict()
        .save()
        .to_bytecode();
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
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);
    // Check the state out (as a prior op_load would) so save can move
    // a fresh Dict back in.
    reg.actor_mut(&id).expect("present").state = None;

    // Script: push:0; dict; save. Empty Dict, no wrapper shape.
    let script = ScriptBuilder::new()
        .push_int(0u64)
        .dict()
        .save()
        .to_bytecode();
    let mut vm = vm_for(id, script);
    for _ in 0..3 {
        vm.step_internal_with_registry(&mut reg).expect("step ok");
    }
    // Save succeeded; an ActorSave entry is in the txlog.
    let save_count = vm
        .txlog
        .iter()
        .filter(|e| matches!(e, TxEntry::ActorSave { .. }))
        .count();
    assert_eq!(save_count, 1);
}

#[test]
fn save_rejects_nested_nonportable_state() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 1_000);
    reg.actor_mut(&id).expect("present").state = None;

    let mut inner = Dict::new();
    inner.insert(
        Int253::ZERO,
        Value::ClearToken(ClearToken::new(Int253::from(-1i64), FLAME_FLAVOR)),
    );
    let mut outer = Dict::new();
    outer.insert(Int253::ZERO, Value::Dict(inner));

    let mut vm = vm_for(id.clone(), ScriptBuilder::new().save().to_bytecode());
    vm.current_call.stack.push(Value::Dict(outer));
    assert!(matches!(
        vm.step_internal_with_registry(&mut reg),
        Err(VMError::NonPortableInState)
    ));
    assert!(reg.actor(&id).expect("present").is_checked_out());
}

#[test]
fn load_then_dismantle_emits_actor_destroy() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(
        &mut reg,
        ScriptBuilder::new().load().drop_().to_bytecode(),
        10_000,
    );
    assert!(reg.exists(&id));

    let block = BlockContext { height: 100 };
    let msg = Message::new(
        id.clone(),
        None,
        Anchor([0x07; 32]),
        Vec::new(),
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
    let result = VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("ok");

    assert!(
        !reg.exists(&id),
        "dismantled state self-destructs the actor"
    );
    assert!(result
        .txlog
        .iter()
        .any(|entry| matches!(entry, TxEntry::ActorDestroy { actor } if actor == &id)));
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
    reg.deploy(id.clone(), recv, state, 10_000).expect("deploy");

    let msg = Message::new(
        id.clone(),
        None,
        Anchor([0x11; 32]),
        Vec::new(),
        100_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
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
    reg.deploy(id.clone(), recv, state, 10_000).expect("deploy");

    let block = BlockContext { height: 100 };
    let msg = Message::new(
        id.clone(),
        None,
        Anchor([0x09; 32]),
        Vec::new(),
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
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
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().load().to_bytecode(), 10_000);
    let block = BlockContext { height: 100 };
    let msg = Message::new(
        id.clone(),
        None,
        Anchor([0x08; 32]),
        Vec::new(),
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
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
    );

    let block = BlockContext { height: 100 };
    let msg = Message::new(
        id.clone(),
        None,
        Anchor([0x02; 32]),
        Vec::new(),
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
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
/// caller, payload, gas, refund predicate) — not just
/// the anchor — so the comparison rebuilds the expected MessageID from
/// the originating `Message`.
#[test]
fn receive_committed_as_first_effect_after_header() {
    let mut reg = MemRegistry::new();
    // Minimal recv: a single `nop`. The script does nothing, but
    // execute_internal still pushes Header + Receive into the txlog.
    let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 10_000);
    let block = BlockContext { height: 100 };
    let known_anchor = [0xab; 32];
    let msg = Message::new(
        id.clone(),
        None,
        Anchor(known_anchor),
        Vec::new(),
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable");
    let expected_send_id = *msg.id().as_bytes();
    let result =
        VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("execute_internal ok");

    // txlog[0] = Header, txlog[1] = Receive(send_id).
    assert!(
        result.txlog.len() >= 2,
        "txlog too short: {}",
        result.txlog.len()
    );
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
        let id = deploy_with_recv(&mut reg, ScriptBuilder::new().nop().to_bytecode(), 10_000);
        let block = BlockContext { height: 100 };
        let msg = Message::new(
            id,
            None,
            Anchor(anchor_bytes),
            Vec::new(),
            1_000_000,
            Predicate::opaque(Predicate::unspendable_key()),
        )
        .expect("message payload is portable");
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
