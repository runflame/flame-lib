//! Tests for dispatch.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn internal_empty_script_finishes() {
    let mut reg = StubRegistry { script: vec![] };
    let block = BlockContext { height: 0 };
    let result =
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
    assert!(result.gas_used > 0, "per-instruction metering");
}

#[test]
fn internal_nop_script_finishes() {
    let mut reg = StubRegistry {
        script: Program::new().nop().nop().nop().to_bytecode(),
    };
    let block = BlockContext { height: 0 };
    let result =
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
    assert!(result.gas_used > 0, "per-instruction metering"); // gas accounting not yet wired up
}

#[test]
fn internal_unknown_opcode_errors() {
    // Plant an explicit invalid opcode (0xff) — raw bytes by design.
    let mut reg = StubRegistry { script: vec![0x1d, 0xff] };
    let block = BlockContext { height: 0 };
    let err =
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap_err();
    assert!(matches!(err, VMError::UnknownOpcode(0xff)));
}

#[test]
fn callframe_advances_through_instructions() {
    // Bytecode: push:5, drop, nop. Three Instructions, then end.
    let instrs = Program::parse(&[0x05, 0x1c, 0x1d]).unwrap().into_instructions();
    let mut frame = CallFrame::new(instrs, CallKind::ExternalRoot, 0, 0, 0);
    use crate::ops::Instruction;
    assert!(matches!(
        frame.next_instruction().unwrap(),
        Some(Instruction::PushInt(_))
    ));
    assert!(matches!(
        frame.next_instruction().unwrap(),
        Some(Instruction::Drop)
    ));
    assert!(matches!(
        frame.next_instruction().unwrap(),
        Some(Instruction::Nop)
    ));
    assert!(frame.next_instruction().unwrap().is_none());
}

/// The reserved chain-info bytes (0xf2–0xf7: height, blockhash,
/// blockburn, blockweight, blockrate, chainstate) must reject with
/// `UnknownOpcode` at *dispatch*, not just parse to `Ext` — scripts
/// using them fail deterministically until the handlers land.
#[test]
fn reserved_chain_info_opcodes_reject_at_dispatch() {
    for byte in 0xf2u8..=0xf7 {
        let mut reg = StubRegistry { script: vec![byte] };
        let block = BlockContext { height: 0 };
        let err = VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block)
            .expect_err("reserved opcode must error");
        assert!(
            matches!(err, VMError::UnknownOpcode(b) if b == byte),
            "0x{:02x} → {:?}",
            byte, err
        );
    }
}

#[test]
fn dirty_stack_at_call_exit_is_an_error() {
    // Strict cross-call semantics: a script that leaves anything on the
    // callee's stack must use `return` to ship those values explicitly.
    // Reaching end-of-script with a non-empty stack is a script bug.
    use crate::Int253;
    let reg = StubRegistry { script: vec![] };
    let block = BlockContext { height: 0 };
    // Re-create what `execute_internal` would, but pre-load the stack.
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0u8; 32]),
        caller: None,
        anchor: Anchor([0u8; 32]),
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(Vec::new(), kind, 1000, 0, 0),
    );
    vm.current_call.stack.push(Value::Int253(Int253::from(7u64)));
    // First step: empty script → finish_call → dirty stack.
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::StackNotClean));
    // Caller's perspective: nothing leaks. (Sanity — there's no parent
    // call to inspect because this is a root frame.)
    let _ = (reg.actor_vbytes(&ActorID::Hash([0; 32])), block.height); // silence warnings
}

#[test]
fn dispatch_falls_through_to_int_path_when_no_constraint_on_top() {
    // Pure Int253 path for `and` — must NOT route to Constraint
    // overload when both operands are Int253. push:1 push:1 and
    // → push:1.
    let mut vm = vm_with_script(
        Program::new().push_int(1u64).push_int(1u64).and().to_bytecode(),
    );
    run_to_end(&mut vm).expect("int and ok");
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(1u64));
}

