//! Tests for op_load / op_save + tx-end self-destruct.

#![allow(unused_imports)]

use super::test_helpers::*;

use crate::{state_with_public, ActorID, Dict, MemRegistry, Int253, RECV_METHOD};

/// Builds an empty state with a single `recv` method that runs the
/// caller-supplied bytes. Returns (state, id).
fn deploy_with_recv(reg: &mut MemRegistry, recv: Vec<u8>, vbytes: u64, height: u64)
    -> ActorID
{
    let mut public = Dict::new();
    public.insert(
        RECV_METHOD,
        Value::String(String::from(recv.clone())),
    );
    let state = state_with_public(public);
    // Derive the id from the recv bytes (stand-in for the
    // real constructor that would deploy this state).
    let id = ActorID::Hash(ActorID::Constructor(recv).to_hash());
    reg.deploy(id.clone(), state, vbytes, height).expect("deploy");
    id
}

/// Build a VM pre-loaded with an InternalRoot frame and arbitrary
/// inline script bytes (overrides what the registry would resolve).
/// Mirrors `vm_internal_with_actor` from test_helpers.
fn vm_for(actor: ActorID, script: Vec<u8>) -> VM {
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

#[test]
fn load_pushes_wrapper_dict_and_marks_actor() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);

    // Script: just `load`. Step once and inspect; don't run to
    // end (end-of-frame is checked clean-stack, which a bare
    // `load` would violate).
    let script = Program::new().load().to_bytecode();
    let mut vm = vm_for(id.clone(), script);
    vm.step_internal_with_registry(&mut reg).expect("load step");

    // Top of stack should be the wrapper Dict.
    match vm.current_call.stack.last().expect("stack not empty") {
        Value::Dict(d) => {
            assert_eq!(d.len(), 2, "wrapper has public + private");
        }
        _ => panic!("expected Dict"),
    }
    // Registry mark is set.
    assert!(reg.is_marked_for_destruction(&id));
    // Frame flag set.
    assert!(vm.current_call.loaded);
}

#[test]
fn load_then_save_round_trips_and_clears_mark() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);

    // Script: `load; save`.
    let script = Program::new().load().save().to_bytecode();
    let mut vm = vm_for(id.clone(), script);
    while vm.step_internal_with_registry(&mut reg).expect("step") {}

    // Stack drained by save.
    assert!(vm.current_call.stack.is_empty());
    // Mark cleared.
    assert!(!reg.is_marked_for_destruction(&id));
    // Frame flag cleared.
    assert!(!vm.current_call.loaded);
}

#[test]
fn load_in_external_root_errors_actor_context() {
    let mut reg = MemRegistry::new();
    // ExternalRoot has no actor identity → require_actor errors.
    let kind = CallKind::ExternalRoot;
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            Program::new().load().into_instructions(),
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
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);
    let mut vm = vm_for(id, Program::new().load().to_bytecode());
    // step_internal (no registry) hits the RegistryUnavailable
    // guard inside op_load.
    let err = vm.step_internal().expect_err("must error");
    assert!(matches!(err, VMError::RegistryUnavailable));
}

#[test]
fn second_load_on_same_frame_errors_already_marked() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);

    // Script: `load; load`.
    let script = Program::new().load().load().to_bytecode();
    let mut vm = vm_for(id, script);
    // First step: `load` succeeds.
    assert!(vm.step_internal_with_registry(&mut reg).expect("first"));
    // Second step: second `load` errors.
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::LoadAlreadyMarked));
}

#[test]
fn load_against_marked_actor_from_outside_frame_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);
    // Pre-mark the actor (as if a sibling frame loaded it).
    reg.mark_for_destruction(&id);

    let mut vm = vm_for(id, Program::new().load().to_bytecode());
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::LoadAlreadyMarked));
}

#[test]
fn load_against_frozen_actor_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);
    // Force-freeze the actor.
    {
        let a = reg.actor_mut(&id).expect("present");
        a.vbytes = 0;
        a.frozen_since = Some(10);
    }

    let mut vm = vm_for(id, Program::new().load().to_bytecode());
    let err = vm.step_internal_with_registry(&mut reg).expect_err("must error");
    assert!(matches!(err, VMError::ActorFrozen));
}

#[test]
fn save_without_load_errors() {
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);

    // Script: push:0, dict, save. Save runs without a prior
    // load → SaveWithoutLoad error.
    let script = Program::new().push_int(0u64).dict().save().to_bytecode();
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
    let id = deploy_with_recv(&mut reg, Program::new().nop().to_bytecode(), 1_000, 0);
    reg.mark_for_destruction(&id); // simulate prior load

    // Script: push:0; dict; save. Empty Dict, no wrapper shape.
    let script = Program::new().push_int(0u64).dict().save().to_bytecode();
    let mut vm = vm_for(id, script);
    vm.current_call.loaded = true; // simulate prior op_load on this frame
    for _ in 0..3 {
        vm.step_internal_with_registry(&mut reg).expect("step ok");
    }
    // Save succeeded; an ActorSave entry is in the txlog.
    let save_count = vm.txlog.iter()
        .filter(|e| matches!(e, crate::tx::TxEntry::ActorSave { .. }))
        .count();
    assert_eq!(save_count, 1);
}

#[test]
fn load_without_save_then_commit_tx_destroys_actor_and_queues_vbytes() {
    // Combined unit test: after `op_load` succeeds the registry
    // mark is set; then `commit_tx_destructions` (the tx-end hook
    // wired into VM::execute_internal) clears the actor and queues
    // its vbytes for release at height + maturity.
    let mut reg = MemRegistry::new();
    let id = deploy_with_recv(&mut reg, vec![0x1d], 10_000, 0);
    assert!(reg.exists(&id));

    // Step a single `load`. We don't run to end-of-frame (that'd
    // hit StackNotClean) — we're testing the registry mutation,
    // not the script's clean exit.
    let mut vm = vm_for(id.clone(), Program::new().load().to_bytecode());
    vm.step_internal_with_registry(&mut reg).expect("load ok");
    assert!(reg.is_marked_for_destruction(&id));

    // Tx-end commit hook (what `execute_internal` runs after a
    // clean script exit; here we invoke it directly to test the
    // Q6 path in isolation).
    let cleared = reg.commit_tx_destructions(100);
    assert_eq!(cleared, 1);
    assert!(!reg.exists(&id));
    let release_at = 100 + crate::VBYTE_MATURITY_BLOCKS;
    let queued = reg
        .vbyte_pool()
        .maturing
        .get(&release_at)
        .copied()
        .unwrap_or(0);
    assert_eq!(queued, 10_000);
}

#[test]
fn load_followed_by_save_preserves_actor() {
    // BlockContext + ActorRegistry come in via test_helpers' wildcard.
    let mut reg = MemRegistry::new();
    // recv = `load; save` — full pair. No self-destruct.
    let id = deploy_with_recv(
        &mut reg,
        Program::new().load().save().to_bytecode(),
        10_000,
        0,
    );

    let block = BlockContext { height: 100 };
    let msg = Message {
        target: id.clone(),
        method: RECV_METHOD,
        caller: None,
        anchor: Anchor([0x02; 32]),
        payload: Vec::new(),
        gas: 1_000_000,
        vbytes: 0,
        refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
    };
    VM::execute_internal(dummy_header(), msg, &mut reg, &block).expect("ok");

    // Actor still present; mark cleared.
    assert!(reg.exists(&id));
    assert!(!reg.is_marked_for_destruction(&id));
}

// ── Receive (SendID committed into Internal TxID) ───────────────────

/// `VM::execute_internal` emits `TxEntry::Receive(send_id)` as the
/// first effect after the Header so the Internal TxID commits to the
/// triggering Send's identity. Symmetric with `op_input` for external
/// transactions. Without this entry, an internal tx's TxID would say
/// nothing about which Send produced it.
///
/// SendID is the canonical hash of the entire Send (anchor, target,
/// caller, method, payload, gas, vbytes, refund predicate) — not just
/// the anchor — so the comparison rebuilds the expected SendID from
/// the originating `Message`.
#[test]
fn receive_committed_as_first_effect_after_header() {
    let mut reg = MemRegistry::new();
    // Minimal recv: a single `nop`. The script does nothing, but
    // execute_internal still pushes Header + Receive into the txlog.
    let id = deploy_with_recv(
        &mut reg,
        Program::new().nop().to_bytecode(),
        10_000,
        0,
    );
    let block = BlockContext { height: 100 };
    let known_anchor = [0xab; 32];
    let msg = Message {
        target: id.clone(),
        method: RECV_METHOD,
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
    assert!(matches!(result.txlog[0], crate::tx::TxEntry::Header(_)));
    match &result.txlog[1] {
        crate::tx::TxEntry::Receive(send_id) => {
            assert_eq!(
                *send_id, expected_send_id,
                "Receive must carry the originating Message's SendID"
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
    fn run_with_anchor(anchor_bytes: [u8; 32]) -> crate::tx::TxID {
        let mut reg = MemRegistry::new();
        let id = deploy_with_recv(
            &mut reg,
            Program::new().nop().to_bytecode(),
            10_000,
            0,
        );
        let block = BlockContext { height: 100 };
        let msg = Message {
            target: id,
            method: RECV_METHOD,
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
        "Internal TxID must distinguish runs by their triggering SendID",
    );
}

/// Sanity: same anchor + same actor + same script → identical
/// Internal TxIDs. Confirms that the Receive-binding is deterministic
/// (no stray randomness leaked into the merkle root via the anchor
/// path).
#[test]
fn receive_internal_txid_is_deterministic_for_same_anchor() {
    fn run() -> crate::tx::TxID {
        let mut reg = MemRegistry::new();
        let id = deploy_with_recv(
            &mut reg,
            Program::new().nop().to_bytecode(),
            10_000,
            0,
        );
        let block = BlockContext { height: 100 };
        let msg = Message {
            target: id,
            method: RECV_METHOD,
            caller: None,
            anchor: Anchor([0xcd; 32]),
            payload: Vec::new(),
            gas: 1_000_000,
            vbytes: 0,
            refund_predicate: Predicate::opaque(Predicate::unspendable_key()),
        };
        VM::execute_internal(dummy_header(), msg, &mut reg, &block)
            .expect("execute_internal ok")
            .txid
    }
    assert_eq!(run().0, run().0);
}
