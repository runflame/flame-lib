//! Tests for tx-level / frame introspection opcodes:
//! `timelock`, `version`, `gas`, `bytes`, `gaslimit`, `memlimit`,
//! `newbytes`. (`selfid` / `anchor` / `callerid` / `method` are
//! covered in `test_actor_introspection.rs`.)

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::vm::LOCKTIME_TIMESTAMP_THRESHOLD;
use crate::{empty_state, ActorID, MemRegistry, Int253, RECV_METHOD};

/// Build an ExternalRoot VM with caller-controlled TxHeader and gas/mem
/// budgets. Used by every test in this file that doesn't need a
/// registry.
fn vm_with_header_and_budgets(
    header: crate::tx::TxHeader,
    script: Vec<u8>,
    gas_limit: u64,
    mem_limit: u64,
) -> VM {
    VM::new(
        header,
        CallFrame::new(
            Program::parse(&script).expect("parse").into_instructions(),
            CallKind::ExternalRoot,
            gas_limit,
            mem_limit,
            0,
        ),
    )
}

// ── timelock ────────────────────────────────────────────────────

#[test]
fn timelock_below_threshold_pushes_value_and_flag_zero() {
    // A block-height locktime — under BIP-65 the flag must be 0.
    let header = crate::tx::TxHeader { version: 1, locktime: 800_000 };
    let mut vm = vm_with_header_and_budgets(header, Program::new().timelock().to_bytecode(), 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    // Stack (bottom→top): [locktime, flag].
    assert_int(&vm.current_call.stack[0], Int253::from(800_000u64));
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn timelock_at_or_above_threshold_pushes_flag_one() {
    let header = crate::tx::TxHeader {
        version: 1,
        locktime: LOCKTIME_TIMESTAMP_THRESHOLD,
    };
    let mut vm = vm_with_header_and_budgets(header, Program::new().timelock().to_bytecode(), 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(LOCKTIME_TIMESTAMP_THRESHOLD as u64));
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
}

// ── version ─────────────────────────────────────────────────────

#[test]
fn version_pushes_tx_header_version() {
    let header = crate::tx::TxHeader { version: 42, locktime: 0 };
    let mut vm = vm_with_header_and_budgets(header, Program::new().version().to_bytecode(), 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(42u64));
}

// ── gas / gaslimit ──────────────────────────────────────────────

#[test]
fn gas_pushes_remaining_budget() {
    // No opcode in this script charges gas yet, so remaining == limit.
    let mut vm = vm_with_header_and_budgets(dummy_header(), Program::new().gas().to_bytecode(), 12_345, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(12_345u64));
}

#[test]
fn gaslimit_pushes_total_cap() {
    let mut vm = vm_with_header_and_budgets(dummy_header(), Program::new().gaslimit().to_bytecode(), 99_999, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(99_999u64));
}

// ── memlimit / newbytes ────────────────────────────────────────

#[test]
fn memlimit_pushes_frame_mem_cap() {
    let mut vm = vm_with_header_and_budgets(dummy_header(), Program::new().memlimit().to_bytecode(), 1_000_000, 4096);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(4096u64));
}

#[test]
fn newbytes_pushes_zero_at_external_root() {
    // The outermost ExternalRoot has no parent → newbytes = 0.
    let mut vm = vm_with_header_and_budgets(dummy_header(), Program::new().newbytes().to_bytecode(), 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn newbytes_value_observable_inside_cell_open_frame() {
    // Build a cell whose leaf returns its `newbytes` value to the
    // parent: `newbytes; push:1; return`. Open the cell with a
    // distinctive `bytes` operand and verify the parent sees that
    // exact value on its stack.
    let leaf = Program::new().newbytes().push_int(1u64).return_().to_bytecode();
    let (tree, cp) = build_predicate_with_program(&leaf, 0);
    let pred_point = tree.compute_point();
    let mut p = Program::new()
        .push_int(0u64)                                // payload count = 0
        .push_point(*pred_point.as_bytes())
        .cell();
    p = push_callproof_to_program(p, &cp);
    let script = p
        .push_int(1024u64)                             // gas
        .push_int(777u64)                              // bytes
        .push_int(0u64)                                // k = 0 args
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    // `run_to_end` exits as soon as the current frame's Run is
    // finished. After `return` swaps back to the parent and the
    // parent's Run has no more instructions, the loop exits without
    // calling finish_call — so the returned value stays visible.
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(777u64));
}

// ── bytes (internal-only, requires registry + actor) ───────────

#[test]
fn bytes_pushes_actor_vbyte_balance() {
    // Deploy an actor with 12_345 vbytes; run `bytes` inside its frame.
    let mut reg = MemRegistry::new();
    let code = Program::new().bytes().to_bytecode();
    let id = ActorID::Hash([0xab; 32]);
    reg.deploy(id.clone(), code, empty_state(), 12_345, 0).expect("deploy");
    let kind = CallKind::InternalRoot {
        actor: id.clone(),
        method: Int253::from(0u64),
        caller: None,
        anchor: Anchor([0u8; 32]),
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            vec![crate::ops::Instruction::Bytes],
            kind,
            1_000_000,
            0,
            0,
        ),
    );
    vm.step_internal_with_registry(&mut reg).expect("ok");
    assert_int(&vm.current_call.stack[0], Int253::from(12_345u64));
}

#[test]
fn bytes_in_external_root_errors_no_actor_context() {
    let mut vm = vm_with_script(Program::new().bytes().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::RegistryUnavailable
    ));
}

#[test]
fn bytes_without_registry_in_actor_frame_errors_registry_unavailable() {
    // InternalRoot frame but driven through registry-less stepper.
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0xab; 32]),
        method: Int253::from(0u64),
        caller: None,
        anchor: Anchor([0u8; 32]),
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(vec![crate::ops::Instruction::Bytes], kind, 1_000_000, 0, 0),
    );
    let err = vm.step_internal().expect_err("bytes needs a registry");
    assert!(matches!(err, VMError::RegistryUnavailable));
}
