//! Tests for tx-level / frame introspection opcodes:
//! `timelock`, `version`, `gas`, `bytes`, `gaslimit`, `memlimit`,
//! `newbytes`. (`actorid` / `anchor` / `callerid` / `method` are
//! covered in `test_actor_introspection.rs`.)

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::vm::LOCKTIME_TIMESTAMP_THRESHOLD;
use crate::{ActorID, ActorState, MemRegistry, Int253, RECV_METHOD};

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
    let mut vm = vm_with_header_and_budgets(header, vec![0x9a], 1_000_000, 0);
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
    let mut vm = vm_with_header_and_budgets(header, vec![0x9a], 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(LOCKTIME_TIMESTAMP_THRESHOLD as u64));
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
}

// ── version ─────────────────────────────────────────────────────

#[test]
fn version_pushes_tx_header_version() {
    let header = crate::tx::TxHeader { version: 42, locktime: 0 };
    let mut vm = vm_with_header_and_budgets(header, vec![0x9b], 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(42u64));
}

// ── gas / gaslimit ──────────────────────────────────────────────

#[test]
fn gas_pushes_remaining_budget() {
    // No opcode in this script charges gas yet, so remaining == limit.
    let mut vm = vm_with_header_and_budgets(dummy_header(), vec![0x9e], 12_345, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(12_345u64));
}

#[test]
fn gaslimit_pushes_total_cap() {
    let mut vm = vm_with_header_and_budgets(dummy_header(), vec![0xa2], 99_999, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(99_999u64));
}

// ── memlimit / newbytes ────────────────────────────────────────

#[test]
fn memlimit_pushes_frame_mem_cap() {
    let mut vm = vm_with_header_and_budgets(dummy_header(), vec![0xa3], 1_000_000, 4096);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(4096u64));
}

#[test]
fn newbytes_pushes_zero_at_external_root() {
    // The outermost ExternalRoot has no parent → newbytes = 0.
    let mut vm = vm_with_header_and_budgets(dummy_header(), vec![0xa4], 1_000_000, 0);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn newbytes_value_observable_inside_cell_open_frame() {
    // Build a cell whose leaf returns its `newbytes` value to the
    // parent: `newbytes; push:1; return`. Open the cell with a
    // distinctive `bytes` operand and verify the parent sees that
    // exact value on its stack.
    let leaf = vec![0xa4, 0x01, 0x7e]; // newbytes, push:1, return
    let (tree, cp) = build_predicate_with_program(&leaf, 0);
    let pred_point = tree.compute_point();
    let mut script = vec![0x00]; // payload count = 0
    push_point_bytes(&mut script, pred_point.as_bytes());
    script.push(0x91); // cell
    push_callproof_pieces(&mut script, &cp);
    script.push(0x12); // pushint16_pos (gas = 1024)
    script.extend_from_slice(&1024u16.to_le_bytes());
    script.push(0x12); // pushint16_pos (bytes = 777)
    script.extend_from_slice(&777u16.to_le_bytes());
    script.push(0x00); // k=0 args
    script.push(0x93); // open
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

/// Build an InternalRoot frame for actor `id` and run a script via
/// the registry-aware stepper.
fn run_internal_with_actor(
    reg: &mut MemRegistry,
    actor: ActorID,
    script: Vec<u8>,
) -> Result<VM, VMError> {
    let kind = CallKind::InternalRoot {
        actor,
        method: Int253::from(0u64),
        caller: None,
        anchor: Anchor([0u8; 32]),
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(&script).unwrap().into_instructions(),
            kind,
            1_000_000,
            0,
            0,
        ),
    );
    while vm.step_internal_with_registry(reg)? {}
    Ok(vm)
}

#[test]
fn bytes_pushes_actor_vbyte_balance() {
    // Deploy an actor with 12_345 vbytes; run `bytes` inside its frame.
    let mut reg = MemRegistry::new();
    let mut state = ActorState::new();
    state.public.insert(RECV_METHOD, Value::String(String::from(b"\x9f".to_vec())));
    let id = ActorID::Hash([0xab; 32]);
    reg.deploy(id.clone(), state, 12_345, 0).expect("deploy");
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
    let mut vm = vm_with_script(vec![0x9f]); // bytes
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
