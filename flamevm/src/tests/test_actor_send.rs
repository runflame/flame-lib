//! Tests for op_send, anchor split, payload, refund predicate.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::{ActorID, ActorRegistry, Int253};

/// Builds bytecode that pushes the operands for `op_send` and
/// executes it. Spec stack (bottom→top):
///   args… (none here)
///   k    (=0)
///   refund  (32-byte String)
///   gas
///   bytes (vbyte allotment)
///   method
///   addr
///   send
fn send_script(
    target: &ActorID,
    refund_bytes: [u8; 32],
    selector: u64,
    gas: u64,
    vbytes: u64,
) -> Vec<u8> {
    // ADR 0020: the selector is just the topmost payload arg.
    Program::new()
        .push_int(selector)                            // selector arg
        .push_int(1u64)                                // k = 1 arg
        .push_str(String::from(refund_bytes.to_vec())) // refund (32-byte)
        .push_int(gas)
        .push_int(vbytes)
        .push_str(String::from(target.to_hash().to_vec())) // addr (32-byte)
        .send()
        .to_bytecode()
}

/// Builds an InternalRoot VM running `script`.
fn vm_internal(actor: ActorID, script: Vec<u8>) -> VM {
    let kind = CallKind::InternalRoot {
        actor,
        caller: None,
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        ).with_anchor(Anchor([0u8; 32])),
    )
}

#[test]
fn send_queues_message_and_emits_txentry() {
    let target = ActorID::Hash([0xbb; 32]);
    let refund_bytes = [0x77; 32];
    let script = send_script(&target, refund_bytes, 3, 10_000, 500);
    let mut vm = vm_internal(ActorID::Hash([0xaa; 32]), script);
    while vm.step_internal().expect("step ok") {}

    // Send entry recorded in txlog (after Header). The full message
    // (including payload + caller) lives in the entry — no separate
    // sends queue.
    let send_count = vm.txlog.iter().filter(|e| matches!(e, crate::tx::TxEntry::Send(_))).count();
    assert_eq!(send_count, 1);
    match vm.txlog.iter().find(|e| matches!(e, crate::tx::TxEntry::Send(_))) {
        Some(crate::tx::TxEntry::Send(msg)) => {
            assert_eq!(msg.target, target);

            assert_eq!(msg.gas, 10_000);
            assert_eq!(msg.vbytes, 500);
            assert_eq!(msg.refund_predicate.to_point().as_bytes(), &refund_bytes);
            // Anchor is the LEFT half of split(InternalRoot.anchor)
            // (the zero seed in this test fixture).
            let (expected_send_anchor, _) = Anchor([0u8; 32]).split();
            assert_eq!(msg.anchor, expected_send_anchor);
            // Caller = the executing actor (InternalRoot's `actor`).
            assert_eq!(msg.caller, Some(ActorID::Hash([0xaa; 32])));
            // The selector rides as the sole payload arg (ADR 0020).
            assert_eq!(msg.payload.len(), 1);
            match &msg.payload[0] {
                Value::Int253(i) => assert_eq!(*i, Int253::from(3u64)),
                other => panic!("expected selector arg, got {:?}", other),
            }
        }
        _ => panic!("expected Send entry"),
    }
}

#[test]
fn two_sends_get_distinct_split_anchors() {
    let target = ActorID::Hash([0xcc; 32]);
    let refund = [0u8; 32];
    let mut script = send_script(&target, refund, 0, 1, 0);
    script.extend(send_script(&target, refund, 0, 1, 0));
    let mut vm = vm_internal(ActorID::Hash([0x11; 32]), script);
    while vm.step_internal().expect("step ok") {}

    // Anchor flow (sends recorded inside TxLog):
    //   parent_0 = InternalRoot.anchor (zero in this test fixture)
    //   send₁: split(parent_0) → (a₀ = entry[1].anchor, parent_1)
    //   send₂: split(parent_1) → (a₁ = entry[2].anchor, parent_2)
    let send_anchors: Vec<Anchor> = vm
        .txlog
        .iter()
        .filter_map(|e| match e {
            crate::tx::TxEntry::Send(msg) => Some(msg.anchor),
            _ => None,
        })
        .collect();
    assert_eq!(send_anchors.len(), 2);
    assert_ne!(send_anchors[0], send_anchors[1], "anchors must differ");
    let (expected_a0, parent_1) = Anchor([0u8; 32]).split();
    let (expected_a1, _) = parent_1.split();
    assert_eq!(send_anchors[0], expected_a0);
    assert_eq!(send_anchors[1], expected_a1);
}

#[test]
fn send_from_external_root_has_no_caller() {
    let target = ActorID::Hash([0xdd; 32]);
    let refund = [0u8; 32];
    let script = send_script(&target, refund, 0, 1, 0);
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    // External root has no inherent anchor — production scripts call
    // `input` first. Seed the frame directly so this test can focus
    // on the caller-id semantics.
    vm.last_anchor = Some(Anchor([0xaa; 32]));
    while vm.step_internal().expect("step ok") {}
    let caller = vm.txlog.iter().find_map(|e| match e {
        crate::tx::TxEntry::Send(msg) => Some(msg.caller.clone()),
        _ => None,
    });
    assert_eq!(caller, Some(None), "no caller from ExternalRoot");
}

#[test]
fn send_payload_in_txlog_differs_across_args() {
    // Two sends with different payloads → different TxEntry::Send
    // payloads (and consequently different TxIDs).
    let target = ActorID::Hash([0xee; 32]);
    let refund = [0u8; 32];

    // Helper that builds a script with 1 portable arg (an Int253).
    let one_arg_send = |arg: u64| {
        Program::new()
            .push_int(arg)                                     // payload[0]
            .push_int(1u64)                                    // k = 1
            .push_str(String::from(refund.to_vec()))           // refund
            .push_int(1u64)                                    // gas
            .push_int(0u64)                                    // bytes
            .push_str(String::from(target.to_hash().to_vec())) // addr
            .send()
            .to_bytecode()
    };

    let script_a = one_arg_send(7);
    let script_b = one_arg_send(9);
    let mut vm_a = vm_internal(ActorID::Hash([0x55; 32]), script_a);
    let mut vm_b = vm_internal(ActorID::Hash([0x55; 32]), script_b);
    while vm_a.step_internal().expect("a") {}
    while vm_b.step_internal().expect("b") {}

    let payload_of = |vm: &VM| -> Vec<Int253> {
        vm.txlog
            .iter()
            .find_map(|e| match e {
                crate::tx::TxEntry::Send(msg) => Some(msg.payload
                    .iter()
                    .map(|v| match v {
                        Value::Int253(i) => *i,
                        _ => panic!("non-Int payload in test"),
                    })
                    .collect()),
                _ => None,
            })
            .expect("Send present")
    };
    assert_ne!(payload_of(&vm_a), payload_of(&vm_b));
    assert_ne!(
        crate::tx::TxID::from_log(&vm_a.txlog),
        crate::tx::TxID::from_log(&vm_b.txlog),
        "different payloads → different TxIDs"
    );
}

#[test]
fn external_txid_includes_send_entry() {
    // External TxID covers every TxEntry — adding a Send must
    // change the TxID. (Q5: External TxID covers SendIDs.)
    let target = ActorID::Hash([0x99; 32]);
    let refund = [0u8; 32];

    // Run a tx with one send.
    let with_send = send_script(&target, refund, 0, 1, 0);
    let mut vm1 = vm_internal(ActorID::Hash([0x33; 32]), with_send);
    while vm1.step_internal().expect("step") {}
    let txid_with = crate::tx::TxID::from_log(&vm1.txlog);

    // Run a no-op tx (nop instead of send).
    let mut vm2 = vm_internal(ActorID::Hash([0x33; 32]), Program::new().nop().to_bytecode());
    while vm2.step_internal().expect("step") {}
    let txid_without = crate::tx::TxID::from_log(&vm2.txlog);

    assert_ne!(txid_with, txid_without, "send must affect TxID");
}

#[test]
fn send_with_non_32_byte_addr_errors() {
    // Stack-shape OK for everything except the addr String, which is
    // 16 bytes instead of 32. `op_send` errors `MalformedAddress`.
    // Built via the public builder; the bad String comes from a
    // shorter byte slice. (No need for raw bytes — only the value
    // length matters.)
    let script = Program::new()
        .push_int(0u64)                              // k = 0
        .push_str(String::from(vec![0u8; 32]))       // refund (valid 32 bytes)
        .push_int(1u64)                              // gas
        .push_int(0u64)                              // bytes
        .push_str(String::from(vec![0u8; 16]))       // BAD addr: 16 bytes
        .send()
        .to_bytecode();
    let mut vm = vm_internal(ActorID::Hash([0u8; 32]), script);
    let err = loop {
        match vm.step_internal() {
            Ok(true) => continue,
            Ok(false) => panic!("ran out before send"),
            Err(e) => break e,
        }
    };
    assert!(matches!(err, VMError::MalformedAddress), "got {:?}", err);
}

#[test]
fn send_with_non_32_byte_refund_errors() {
    let script = Program::new()
        .push_int(0u64)                              // k = 0
        .push_str(String::from(vec![0u8; 16]))       // BAD refund: 16 bytes
        .push_int(1u64)                              // gas
        .push_int(0u64)                              // bytes
        .push_str(String::from(vec![0u8; 32]))       // addr (valid)
        .send()
        .to_bytecode();
    let mut vm = vm_internal(ActorID::Hash([0u8; 32]), script);
    let err = loop {
        match vm.step_internal() {
            Ok(true) => continue,
            Ok(false) => panic!("ran out before send"),
            Err(e) => break e,
        }
    };
    assert!(matches!(err, VMError::MalformedAddress), "got {:?}", err);
}
