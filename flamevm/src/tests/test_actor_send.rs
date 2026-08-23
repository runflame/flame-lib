//! Tests for portable asynchronous messages and their public effects.

use super::test_helpers::*;
use crate::tx::{TxEntry, TxID};
use crate::{ActorID, Int253};
use readerwriter::Encodable;

fn send_script(target: &ActorID, refund: [u8; 32], selector: u64, gas: u64) -> Vec<u8> {
    ScriptBuilder::new()
        .push_int(selector)
        .push_int(1u64)
        .push_str(String::from(refund.to_vec()))
        .push_int(gas)
        .push_str(String::from(target.to_hash().to_vec()))
        .send()
        .to_bytecode()
}

fn sends(log: &[TxEntry]) -> Vec<&Message> {
    log.iter()
        .filter_map(|entry| match entry {
            TxEntry::Send(message) => Some(message),
            _ => None,
        })
        .collect()
}

#[test]
fn send_commits_message_fields() {
    let target = ActorID::Hash([0xbb; 32]);
    let refund = [0x77; 32];
    let mut reg = MemRegistry::new();
    let sender = deploy_actor(&mut reg, send_script(&target, refund, 3, 10_000));
    let log = deliver(&mut reg, msg_to(sender.clone()));

    let messages = sends(&log);
    assert_eq!(messages.len(), 1);
    let message = messages[0];
    assert_eq!(message.target, target);
    assert_eq!(message.gas, 10_000);
    assert_eq!(message.caller, Some(sender));
    assert_eq!(message.refund_predicate.to_point().as_bytes(), &refund);
    assert_eq!(message.anchor, Anchor([0u8; 32]).split().0);
    assert!(matches!(
        message.payload(),
        [Value::Int253(value)] if *value == Int253::from(3u64)
    ));
}

#[test]
fn sequential_sends_get_distinct_anchors_and_ids() {
    let target = ActorID::Hash([0xcc; 32]);
    let mut code = send_script(&target, [0; 32], 0, 1);
    code.extend(send_script(&target, [0; 32], 0, 1));
    let mut reg = MemRegistry::new();
    let actor = deploy_actor(&mut reg, code);
    let log = deliver(&mut reg, msg_to(actor));
    let messages = sends(&log);
    let anchors: Vec<_> = messages.iter().map(|message| message.anchor).collect();
    let (first, remainder) = Anchor([0u8; 32]).split();
    let (second, _) = remainder.split();
    assert_eq!(anchors, vec![first, second]);
    assert_ne!(messages[0].id(), messages[1].id());
}

#[test]
fn send_payload_changes_transaction_id() {
    let target = ActorID::Hash([0xee; 32]);
    let mut reg = MemRegistry::new();
    let first = deploy_actor(&mut reg, send_script(&target, [0; 32], 7, 1));
    let second = deploy_actor(&mut reg, send_script(&target, [0; 32], 9, 1));
    let first = deliver(&mut reg, msg_to(first));
    let second = deliver(&mut reg, msg_to(second));
    let payload_int = |log: &[TxEntry]| match sends(log)[0].payload() {
        [Value::Int253(value)] => *value,
        other => panic!("unexpected payload: {:?}", other),
    };
    assert_ne!(payload_int(&first), payload_int(&second));
    assert_ne!(TxID::from_log(&first), TxID::from_log(&second));
}

#[test]
fn send_rejects_malformed_addresses() {
    for (refund, target) in [(vec![0; 16], vec![0; 32]), (vec![0; 32], vec![0; 16])] {
        let code = ScriptBuilder::new()
            .push_int(0u64)
            .push_str(String::from(refund))
            .push_int(1u64)
            .push_str(String::from(target))
            .send()
            .to_bytecode();
        let mut reg = MemRegistry::new();
        let actor = deploy_actor(&mut reg, code);
        assert!(matches!(
            deliver_err(&mut reg, msg_to(actor)),
            VMError::MalformedAddress
        ));
    }
}

#[test]
fn send_rejects_nested_nonportable_payload() {
    let actor = ActorID::Hash([0x11; 32]);
    let mut vm = vm_internal_with_actor(ScriptBuilder::new().send().to_bytecode(), actor);
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
        Value::String(String::from(vec![0u8; 32])),
        Value::Int253(Int253::ONE),
        Value::String(String::from(vec![0u8; 32])),
    ];
    assert!(matches!(
        vm.step_internal(),
        Err(VMError::NonPortableInSend)
    ));
}

#[test]
fn send_rejects_widetoken_payload() {
    let script = ScriptBuilder::new()
        .push_int(1u64)
        .fee()
        .push_int(1u64)
        .push_str(String::from(vec![0u8; 32]))
        .push_int(1u64)
        .push_str(String::from(vec![0u8; 32]))
        .send()
        .to_bytecode();
    let mut vm = vm_external_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    let pc_gens = PedersenGens::default();
    let mut prover = Prover::new(&pc_gens);

    let err = loop {
        match vm.step_external(&mut prover) {
            Ok(true) => continue,
            Ok(false) => panic!("send unexpectedly succeeded"),
            Err(error) => break error,
        }
    };
    assert!(matches!(err, VMError::NonPortableInSend));
}

#[test]
fn external_send_has_no_caller() {
    let target = ActorID::Hash([0xdd; 32]);
    let script = send_script(&target, [0; 32], 0, 1);
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script).unwrap().into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    );
    vm.last_anchor = Some(Anchor([0xaa; 32]));
    while vm.step_internal().unwrap() {}
    let message = sends(&vm.txlog)[0];
    assert!(message.caller.is_none());
}

#[test]
fn send_preserves_constructor_code_for_first_delivery() {
    let target = ActorID::Constructor(ScriptBuilder::new().nop().to_bytecode());
    let script = ScriptBuilder::new()
        .push_int(0u64)
        .push_str(String::from(vec![0; 32]))
        .push_int(0u64)
        .push_str(String::from(target.encode_to_vec()))
        .send()
        .to_bytecode();
    let mut vm = vm_external_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));

    run_to_end(&mut vm).unwrap();

    assert_eq!(sends(&vm.txlog)[0].target, target);
}

#[test]
fn send_debits_its_full_grant() {
    fn gas_used(grant: u64) -> u64 {
        let target = ActorID::Hash([0x61; 32]);
        let mut vm = vm_internal_with_actor(
            send_script(&target, [0; 32], 0, grant),
            ActorID::Hash([0x60; 32]),
        );
        run_to_end(&mut vm).expect("send succeeds");
        vm.current_call.gas_used
    }

    assert_eq!(gas_used(37) - gas_used(0), 37);
}

#[test]
fn send_cannot_reserve_more_than_the_remaining_frame_gas() {
    let grant = 100;
    let target = ActorID::Hash([0x63; 32]);
    let mut vm = vm_internal_with_actor(
        send_script(&target, [0; 32], 0, grant),
        ActorID::Hash([0x62; 32]),
    );

    // Execute the five operand pushes, then leave enough gas for the send
    // instruction itself but one unit less than its requested grant.
    for _ in 0..5 {
        assert!(vm.step_internal().expect("operand push succeeds"));
    }
    vm.current_call.gas_limit = vm.current_call.gas_used + grant;

    assert!(matches!(vm.step_internal(), Err(VMError::OutOfGas)));
    assert!(sends(&vm.txlog).is_empty());
}

#[test]
fn internal_send_cannot_mint_a_larger_descendant_budget() {
    let target = ActorID::Hash([0x65; 32]);
    let mut reg = MemRegistry::new();
    let sender = deploy_actor(&mut reg, send_script(&target, [0; 32], 0, 500));
    let message = Message::new(
        sender,
        None,
        Anchor([0x64; 32]),
        Vec::new(),
        500,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("empty payload is portable");

    assert!(matches!(deliver_err(&mut reg, message), VMError::OutOfGas));
}
