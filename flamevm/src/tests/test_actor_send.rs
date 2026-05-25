//! Tests for op_send, anchor ratchet, payload_hash, refund predicate.

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
///
/// For 0 args: push k, refund, gas, bytes, method, addr, send.
fn send_script(
    target: &ActorID,
    refund_bytes: [u8; 32],
    method: u64,
    gas: u64,
    vbytes: u64,
) -> Vec<u8> {
    let mut s = Vec::new();
    // k = 0
    s.push(0x00);
    // refund: pushstr 32 bytes
    s.push(0x19); // pushstr
    s.push(0x00); // sub-varint tag U8
    s.push(32);   // length
    s.extend_from_slice(&refund_bytes);
    // gas
    s.extend(push_int_bytes(gas));
    // bytes
    s.extend(push_int_bytes(vbytes));
    // method
    s.extend(push_int_bytes(method));
    // addr (32-byte hash)
    s.push(0x19);
    s.push(0x00);
    s.push(32);
    s.extend_from_slice(&target.to_hash());
    // send
    s.push(0x94);
    s
}

fn push_int_bytes(n: u64) -> Vec<u8> {
    if n < 16 {
        vec![n as u8]
    } else {
        let mut v = vec![0x14];
        v.extend_from_slice(&n.to_le_bytes());
        v
    }
}

/// Builds an InternalRoot VM running `script`.
fn vm_internal(actor: ActorID, script: Vec<u8>) -> VM {
    let kind = CallKind::InternalRoot {
        actor,
        method: Int253::from(0u64),
        caller: None,
        anchor: Anchor([0u8; 32]),
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        ),
    )
}

#[test]
fn send_queues_message_and_emits_txentry() {
    let target = ActorID::Hash([0xbb; 32]);
    let refund_bytes = [0x77; 32];
    let script = send_script(&target, refund_bytes, 3, 10_000, 500);
    let mut vm = vm_internal(ActorID::Hash([0xaa; 32]), script);
    while vm.step_internal().expect("step ok") {}

    // Send entry recorded in txlog (after Header).
    let send_entry = vm.txlog.iter().find_map(|e| match e {
        crate::tx::TxEntry::Send {
            target: t,
            method,
            gas,
            vbytes,
            refund_predicate,
            anchor,
            payload_hash,
        } => Some((
            t.clone(),
            *method,
            *gas,
            *vbytes,
            refund_predicate.clone(),
            *anchor,
            *payload_hash,
        )),
        _ => None,
    });
    let (t, method, gas, vbytes, refund, anchor, _hash) =
        send_entry.expect("Send entry present");
    assert_eq!(t, target);
    assert_eq!(method, Int253::from(3u64));
    assert_eq!(gas, 10_000);
    assert_eq!(vbytes, 500);
    assert_eq!(refund.to_point().as_bytes(), &refund_bytes);
    // Anchor is the ratchet of the zero seed (no prior anchor in
    // this frame).
    assert_eq!(anchor, Anchor([0u8; 32]).ratchet());

    // Message queued into vm.sends.
    assert_eq!(vm.sends.len(), 1);
    let msg = &vm.sends[0];
    assert_eq!(msg.target, target);
    assert_eq!(msg.method, Int253::from(3u64));
    assert_eq!(msg.gas, 10_000);
    assert_eq!(msg.vbytes, 500);
    assert_eq!(msg.anchor, anchor);
    // Caller = the executing actor (InternalRoot's actor field).
    assert_eq!(msg.caller, Some(ActorID::Hash([0xaa; 32])));
    // SendID == anchor.
    assert_eq!(msg.id().as_bytes(), &anchor.0);
}

#[test]
fn two_sends_get_distinct_ratcheted_anchors() {
    let target = ActorID::Hash([0xcc; 32]);
    let refund = [0u8; 32];
    let mut script = send_script(&target, refund, 0, 1, 0);
    script.extend(send_script(&target, refund, 0, 1, 0));
    let mut vm = vm_internal(ActorID::Hash([0x11; 32]), script);
    while vm.step_internal().expect("step ok") {}

    assert_eq!(vm.sends.len(), 2);
    let a0 = vm.sends[0].anchor;
    let a1 = vm.sends[1].anchor;
    assert_ne!(a0, a1, "anchors must differ");
    assert_eq!(a1, a0.ratchet(), "second anchor is first.ratchet()");
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
    while vm.step_internal().expect("step ok") {}
    assert_eq!(vm.sends.len(), 1);
    assert_eq!(vm.sends[0].caller, None, "no caller from ExternalRoot");
}

#[test]
fn send_payload_hash_binds_to_args() {
    // Two sends with different payloads → different payload_hash.
    let target = ActorID::Hash([0xee; 32]);
    let refund = [0u8; 32];

    // Helper that builds a script with 1 portable arg (an Int253).
    let one_arg_send = |arg: u8| {
        let mut s = Vec::new();
        // arg
        s.push(arg as u8); // push:k (small)
        // k = 1
        s.push(0x01);
        // refund
        s.push(0x19); s.push(0x00); s.push(32); s.extend_from_slice(&refund);
        // gas = 1, bytes = 0, method = 0
        s.push(0x01); s.push(0x00); s.push(0x00);
        // addr
        s.push(0x19); s.push(0x00); s.push(32);
        s.extend_from_slice(&target.to_hash());
        s.push(0x94); // send
        s
    };

    let script_a = one_arg_send(7);
    let script_b = one_arg_send(9);
    let mut vm_a = vm_internal(ActorID::Hash([0x55; 32]), script_a);
    let mut vm_b = vm_internal(ActorID::Hash([0x55; 32]), script_b);
    while vm_a.step_internal().expect("a") {}
    while vm_b.step_internal().expect("b") {}

    let hash = |vm: &VM| {
        vm.txlog
            .iter()
            .find_map(|e| match e {
                crate::tx::TxEntry::Send { payload_hash, .. } => Some(*payload_hash),
                _ => None,
            })
            .expect("Send present")
    };
    assert_ne!(
        hash(&vm_a),
        hash(&vm_b),
        "different payloads → different hashes"
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
    let mut vm2 = vm_internal(ActorID::Hash([0x33; 32]), vec![0x1d]);
    while vm2.step_internal().expect("step") {}
    let txid_without = crate::tx::TxID::from_log(&vm2.txlog);

    assert_ne!(txid_with, txid_without, "send must affect TxID");
}

#[test]
fn send_with_non_32_byte_addr_errors() {
    let mut s = Vec::new();
    // k = 0, refund (valid), gas = 1, bytes = 0, method = 0
    s.push(0x00);
    s.push(0x19); s.push(0x00); s.push(32); s.extend_from_slice(&[0u8; 32]);
    s.push(0x01); s.push(0x00); s.push(0x00);
    // BAD addr: only 16 bytes
    s.push(0x19); s.push(0x00); s.push(16); s.extend_from_slice(&[0u8; 16]);
    s.push(0x94);
    let mut vm = vm_internal(ActorID::Hash([0u8; 32]), s);
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
    let mut s = Vec::new();
    // k = 0
    s.push(0x00);
    // BAD refund: 16 bytes
    s.push(0x19); s.push(0x00); s.push(16); s.extend_from_slice(&[0u8; 16]);
    // gas, bytes, method, addr
    s.push(0x01); s.push(0x00); s.push(0x00);
    s.push(0x19); s.push(0x00); s.push(32); s.extend_from_slice(&[0u8; 32]);
    s.push(0x94);
    let mut vm = vm_internal(ActorID::Hash([0u8; 32]), s);
    let err = loop {
        match vm.step_internal() {
            Ok(true) => continue,
            Ok(false) => panic!("ran out before send"),
            Err(e) => break e,
        }
    };
    assert!(matches!(err, VMError::MalformedAddress), "got {:?}", err);
}
