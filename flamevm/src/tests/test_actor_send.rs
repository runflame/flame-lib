//! Tests for op_send, anchor split, payload, refund predicate.
//!
//! These drive whole internal txs through the delivery path (`deliver`)
//! and observe the **public effect log** (`TxEntry::Send`), not internal
//! VM state. The one external-root case keeps a focused harness because
//! there is no lightweight public external runner (prove+verify is heavy).

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::tx::{TxEntry, TxID};
use crate::{ActorID, ActorRegistry, Int253};

/// `op_send` operands as a recv-code blob. Spec stack (bottom→top):
/// `args… k refund gas bytestoken addr send`.
fn send_script(target: &ActorID, refund_bytes: [u8; 32], selector: u64, gas: u64, vbytes: u64) -> Vec<u8> {
    // ADR 0020: the selector is just the topmost payload arg.
    let mut program = ScriptBuilder::new()
        .push_int(selector) // selector arg
        .push_int(1u64) // k = 1 arg
        .push_str(String::from(refund_bytes.to_vec())) // refund (32-byte)
        .push_int(gas);
    program = if vbytes == 0 {
        program.push_int(BYTES_FLAVOR).pushtoken()
    } else {
        // Produce a balanced ±bytes pair, retire the debt half, and
        // leave the positive bearer as send's byte-token operand.
        program
            .push_int(vbytes)
            .push_int(BYTES_FLAVOR)
            .borrow()
            .push_int(1u64)
            .roll()
            .retire()
    };
    program
        .push_str(String::from(target.to_hash().to_vec())) // addr (32-byte)
        .send()
        .to_bytecode()
}

/// The `Send` messages emitted by a delivery, in order.
fn sends(log: &[TxEntry]) -> Vec<&Message> {
    log.iter()
        .filter_map(|e| match e {
            TxEntry::Send(m) => Some(m),
            _ => None,
        })
        .collect()
}

#[test]
fn send_queues_message_and_emits_txentry() {
    let target = ActorID::Hash([0xbb; 32]);
    let refund_bytes = [0x77; 32];
    let mut reg = MemRegistry::new();
    let sender = deploy_actor(&mut reg, send_script(&target, refund_bytes, 3, 10_000, 500));
    let log = deliver(&mut reg, msg_to(sender.clone()));

    let sends = sends(&log);
    assert_eq!(sends.len(), 1, "exactly one Send effect");
    let msg = sends[0];
    assert_eq!(msg.target, target);
    assert_eq!(msg.gas, 10_000);
    assert_eq!(msg.vbytes, 500);
    assert_eq!(msg.refund_predicate.to_point().as_bytes(), &refund_bytes);
    // Anchor is the LEFT half of split(delivering message's anchor) — the
    // zero seed from `msg_to`.
    let (expected_send_anchor, _) = Anchor([0u8; 32]).split();
    assert_eq!(msg.anchor, expected_send_anchor);
    // Caller = the actor that ran the send (the delivered-to actor).
    assert_eq!(msg.caller, Some(sender));
    // The selector rides as the sole payload arg (ADR 0020).
    assert_eq!(msg.payload.len(), 1);
    match &msg.payload[0] {
        Value::Int253(i) => assert_eq!(*i, Int253::from(3u64)),
        other => panic!("expected selector arg, got {:?}", other),
    }
}

#[test]
fn two_sends_get_distinct_split_anchors() {
    let target = ActorID::Hash([0xcc; 32]);
    let refund = [0u8; 32];
    let mut recv = send_script(&target, refund, 0, 1, 0);
    recv.extend(send_script(&target, refund, 0, 1, 0));
    let mut reg = MemRegistry::new();
    let sender = deploy_actor(&mut reg, recv);
    let log = deliver(&mut reg, msg_to(sender));

    // Anchor flow: parent_0 = delivering anchor (zero); each send splits
    // it, threading the right half forward.
    let anchors: Vec<Anchor> = sends(&log).iter().map(|m| m.anchor).collect();
    assert_eq!(anchors.len(), 2);
    assert_ne!(anchors[0], anchors[1], "anchors must differ");
    let (expected_a0, parent_1) = Anchor([0u8; 32]).split();
    let (expected_a1, _) = parent_1.split();
    assert_eq!(anchors[0], expected_a0);
    assert_eq!(anchors[1], expected_a1);
}

#[test]
fn send_payload_differs_across_args() {
    // Two sends with different payloads → different Send payloads and
    // (consequently) different TxIDs.
    let target = ActorID::Hash([0xee; 32]);
    let refund = [0u8; 32];
    let one_arg_send = |arg: u64| {
        ScriptBuilder::new()
            .push_int(arg) // payload[0]
            .push_int(1u64) // k = 1
            .push_str(String::from(refund.to_vec())) // refund
            .push_int(1u64) // gas
            .push_int(BYTES_FLAVOR)
            .pushtoken() // zero-byte token
            .push_str(String::from(target.to_hash().to_vec())) // addr
            .send()
            .to_bytecode()
    };
    let mut reg = MemRegistry::new();
    let a = deploy_actor(&mut reg, one_arg_send(7));
    let bb = deploy_actor(&mut reg, one_arg_send(9));
    let log_a = deliver(&mut reg, msg_to(a));
    let log_b = deliver(&mut reg, msg_to(bb));

    let payload_ints = |log: &[TxEntry]| -> Vec<Int253> {
        sends(log)[0]
            .payload
            .iter()
            .map(|v| match v {
                Value::Int253(i) => *i,
                _ => panic!("non-Int payload in test"),
            })
            .collect()
    };
    assert_ne!(payload_ints(&log_a), payload_ints(&log_b));
    assert_ne!(TxID::from_log(&log_a), TxID::from_log(&log_b), "different payloads → different TxIDs");
}

#[test]
fn txid_includes_send_entry() {
    // Internal TxID covers every TxEntry — adding a Send must change it.
    let target = ActorID::Hash([0x99; 32]);
    let refund = [0u8; 32];
    let mut reg = MemRegistry::new();
    let with = deploy_actor(&mut reg, send_script(&target, refund, 0, 1, 0));
    let without = deploy_actor(&mut reg, ScriptBuilder::new().nop().to_bytecode());
    let txid_with = TxID::from_log(&deliver(&mut reg, msg_to(with)));
    let txid_without = TxID::from_log(&deliver(&mut reg, msg_to(without)));
    assert_ne!(txid_with, txid_without, "send must affect TxID");
}

#[test]
fn send_with_non_32_byte_addr_errors() {
    // 16-byte addr String → `op_send` errors `MalformedAddress`.
    let recv = ScriptBuilder::new()
        .push_int(0u64) // k = 0
        .push_str(String::from(vec![0u8; 32])) // refund (valid)
        .push_int(1u64) // gas
        .push_int(BYTES_FLAVOR)
        .pushtoken() // zero-byte token
        .push_str(String::from(vec![0u8; 16])) // BAD addr: 16 bytes
        .send()
        .to_bytecode();
    let mut reg = MemRegistry::new();
    let id = deploy_actor(&mut reg, recv);
    assert!(matches!(deliver_err(&mut reg, msg_to(id)), VMError::MalformedAddress));
}

#[test]
fn send_with_non_32_byte_refund_errors() {
    let recv = ScriptBuilder::new()
        .push_int(0u64) // k = 0
        .push_str(String::from(vec![0u8; 16])) // BAD refund: 16 bytes
        .push_int(1u64) // gas
        .push_int(BYTES_FLAVOR)
        .pushtoken() // zero-byte token
        .push_str(String::from(vec![0u8; 32])) // addr (valid)
        .send()
        .to_bytecode();
    let mut reg = MemRegistry::new();
    let id = deploy_actor(&mut reg, recv);
    assert!(matches!(deliver_err(&mut reg, msg_to(id)), VMError::MalformedAddress));
}

#[test]
fn send_from_external_root_has_no_caller() {
    // External-root semantics: a send emitted by an external tx has
    // `caller = None` (the external sender has no actor identity). No
    // lightweight public external runner exists (prove+verify is heavy),
    // so this stays a focused harness test — exactly the documented
    // exception to the no-white-box rule.
    let target = ActorID::Hash([0xdd; 32]);
    let script = send_script(&target, [0u8; 32], 0, 1, 0);
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    // External root has no inherent anchor — seed it directly (production
    // scripts call `input` first).
    vm.last_anchor = Some(Anchor([0xaa; 32]));
    while vm.step_internal().expect("step ok") {}
    let caller = vm.txlog.iter().find_map(|e| match e {
        TxEntry::Send(msg) => Some(msg.caller.clone()),
        _ => None,
    });
    assert_eq!(caller, Some(None), "no caller from ExternalRoot");
}
