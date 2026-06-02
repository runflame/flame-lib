//! Tests for control flow.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn verify_truthy_pops() {
    // push:1, verify — succeeds, stack empties.
    let mut vm = vm_with_script(Program::new().push_int(1u64).verify().to_bytecode());
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn verify_zero_fails() {
    let mut vm = vm_with_script(Program::new().push_int(0u64).verify().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::VerifyFailed
    ));
}

#[test]
fn verify_requires_int() {
    // pushpoint, verify — top is Point not Int253.
    let script = Program::new().push_point([0u8; 32]).verify().to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

#[test]
fn run_creates_nested_run() {
    // pushstr [push:7], run — after run, current_run is the
    // subprogram and outer is suspended.
    let mut script = pushstr_bytes(&[0x07]);
    script.push(0xa1);
    let mut vm = vm_with_script(script);
    vm.step_internal().unwrap(); // pushstr
    assert_eq!(vm.current_call.stack.len(), 1);
    vm.step_internal().unwrap(); // run
    assert_eq!(vm.current_call.run_stack.len(), 1);
    assert!(vm.current_call.stack.is_empty());
    vm.step_internal().unwrap(); // push:7 in subprog
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
}

#[test]
fn run_resumes_outer_after_subprogram_finishes() {
    // pushstr [push:7, drop], run — subprog cleans up, outer ends empty.
    let mut script = pushstr_bytes(&[0x07, 0x1c]);
    script.push(0xa1);
    let mut reg = StubRegistry { script };
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
}

#[test]
fn run_requires_string() {
    // push:5, run — top is Int253 not String.
    let mut vm = vm_with_script(Program::new().push_int(5u64).run().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotString
    ));
}

/// Prover-side: a sub-script pushed via `push_script` (i.e. as
/// `String::Script(instrs)`) carries witnesses through `run`. The
/// inner `alloc(Some(_))` slots survive the dispatch, so a CS
/// equality involving them satisfies the constraint system at
/// prove time. End-to-end via Prover::prove → Verifier::verify.
///
/// Regression guard for the witness-erasing path that existed
/// before `String::Script` was introduced (then, every sub-script
/// went through `String::Opaque(bytes)` and lost witnesses).
#[test]
fn run_preserves_alloc_witnesses_via_script_string() {
    let pc_gens = bulletproofs::PedersenGens::default();
    // Inner: alloc(7), alloc(3), add, alloc(10), eq, verify.
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    // Outer: push the inner as a Script-string, then run it.
    let outer = Program::new().push_script(inner).run();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .expect("prove with witness-bearing sub-script");
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = bulletproofs::PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .expect("verify ok");
}

/// Negative counterpart: pushing the inner script as
/// `String::Opaque(bytes)` (not `Script`) erases witnesses, so
/// the prover's CS variables come up unassigned and the proof
/// step fails with `WitnessMissing`. Demonstrates that
/// `push_script` is what carries the witness through; the bytes
/// path is only useful for verifier-side input.
#[test]
fn run_with_opaque_sub_script_erases_alloc_witnesses() {
    let pc_gens = bulletproofs::PedersenGens::default();
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let inner_bytes = inner.to_bytecode();
    let outer = Program::new()
        .push_str(String::from(inner_bytes)) // Opaque, no witnesses
        .run();
    let err = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .unwrap_err();
    assert!(matches!(err, VMError::R1CSError(_)));
}

/// Same shape with `op_switch` — confirm the chosen branch's
/// witnesses survive too. Picks `a` (x=1), which holds the
/// witness-bearing equality script; `b` is a no-op script.
#[test]
fn switch_preserves_alloc_witnesses_via_script_string() {
    let pc_gens = bulletproofs::PedersenGens::default();
    let a = Program::new()
        .alloc(Some(Int253::from(4u64)))
        .alloc(Some(Int253::from(4u64)))
        .eq()
        .verify();
    let b = Program::new(); // never taken
    let outer = Program::new()
        .push_int(1u64) // x = 1 → pick a
        .push_script(a)
        .push_script(b)
        .switch();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .expect("prove with witness-bearing switch branch");
    let TxResult { bytecode, proof, .. } = result;
    let proof = proof.expect("proof set");
    let pc_gens_v = bulletproofs::PedersenGens::default();
    Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .expect("verify ok");
}

#[test]
fn loop_resets_run_cursor_to_start() {
    // nop, loop — after `loop` the Run cursor is back at the start,
    // so the next step parses `nop` again (not end-of-script).
    let mut vm = vm_with_script(Program::new().nop().loop_().to_bytecode());
    vm.step_internal().unwrap(); // nop
    vm.step_internal().unwrap(); // loop
    // Cursor should be at the start: the next instruction is `nop` again.
    let next = vm.current_call.current_run.next_instruction().unwrap();
    assert!(matches!(next, Some(crate::ops::Instruction::Nop)));
}

#[test]
fn switch_picks_a_when_x_nonzero() {
    // push:1, pushstr [push:9, drop], pushstr [push:8, drop], switch
    // — x=1 → runs a (pushes 9, drops it). End stack empty.
    let mut script = vec![0x01];
    script.extend_from_slice(&pushstr_bytes(&[0x09, 0x1c]));
    script.extend_from_slice(&pushstr_bytes(&[0x08, 0x1c]));
    script.push(0xa3);
    let mut reg = StubRegistry { script };
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block)
        .unwrap();
}

#[test]
fn switch_a_actually_runs_when_x_nonzero() {
    // Verifies the *chosen* branch executes by inspecting mid-flight.
    // push:1, pushstr [push:9], pushstr [push:8], switch
    let mut script = vec![0x01];
    script.extend_from_slice(&pushstr_bytes(&[0x09]));
    script.extend_from_slice(&pushstr_bytes(&[0x08]));
    script.push(0xa3);
    let mut vm = vm_with_script(script);
    while !vm.current_call.run_stack.is_empty()
        || !vm.current_call.current_run.is_finished()
    {
        // Pre-switch: keep stepping until switch happens (run_stack
        // becomes non-empty) and then the chosen subprogram runs to
        // its end.
        if !vm.step_internal().unwrap() {
            break;
        }
        if !vm.current_call.run_stack.is_empty()
            && vm.current_call.current_run.is_finished()
        {
            break;
        }
    }
    // The 9 (from branch a) should be the only stack item.
    assert_int(
        vm.current_call.stack.last().unwrap(),
        Int253::from(9u64),
    );
}

#[test]
fn switch_picks_b_when_x_zero() {
    // push:0, pushstr [push:9, drop], pushstr [push:8, drop], switch
    let mut script = vec![0x00];
    script.extend_from_slice(&pushstr_bytes(&[0x09, 0x1c]));
    script.extend_from_slice(&pushstr_bytes(&[0x08, 0x1c]));
    script.push(0xa3);
    let mut reg = StubRegistry { script };
    let block = BlockContext { height: 0 };
    // x=0 → runs branch b (push:8, drop) → empty stack at end → ok.
    VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
}

#[test]
fn return_zero_at_root_errors() {
    // push:0, return — root frame has no caller, so `return` errors
    // even with k=0. Scripts that want a clean early exit use `break:0`.
    let mut vm = vm_with_script(Program::new().push_int(0u64).return_().to_bytecode());
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::ReturnAtRoot
    ));
}

#[test]
fn return_nonzero_at_root_errors() {
    // push:7, push:1, return — k=1 at root: nowhere for 7 to go.
    let mut vm = vm_with_script(
        Program::new().push_int(7u64).push_int(1u64).return_().to_bytecode(),
    );
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::ReturnAtRoot
    ));
}

#[test]
fn break_zero_at_root_with_clean_stack_exits_cleanly() {
    // break:0 at root — preferred way to short-circuit cleanly.
    let mut vm = vm_with_script(Program::new().break_k(0).to_bytecode());
    run_until_tx_done(&mut vm).unwrap();
}

#[test]
fn break_zero_at_root_with_leftover_stack_errors() {
    // push:5, break:0 — break works, but finish_call catches the leftover.
    let mut vm = vm_with_script(
        Program::new().push_int(5u64).break_k(0).to_bytecode(),
    );
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::StackNotClean
    ));
}

#[test]
fn return_with_dirty_leftover_errors() {
    // Inside a child frame: push:9, push:7, push:1, return — k=1, two
    // items below count → StackNotClean. Error is caught and translated
    // to a `0` failure marker on the parent.
    let mut vm = vm_with_nested_child_script(
        Program::new()
            .push_int(9u64).push_int(7u64).push_int(1u64).return_()
            .to_bytecode(),
    );
    while !vm.call_stack.is_empty() {
        vm.step_internal().expect("step ok — error swallowed into marker");
    }
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn return_too_few_items_errors() {
    // Inside a child frame: push:5, return — k=5 popped, zero items
    // remain → BadReturnArity. The `step` wrapper catches the error,
    // unwinds the child, and pushes `0` (failure marker) onto the parent.
    let mut vm = vm_with_nested_child_script(
        Program::new().push_int(5u64).return_().to_bytecode(),
    );
    while !vm.call_stack.is_empty() {
        vm.step_internal().expect("step ok — error swallowed into marker");
    }
    // Parent stack: just the failure marker.
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(0u64));
}

#[test]
fn return_transfers_values_to_parent() {
    // Set up a nested call manually (the `call` opcode is not yet
    // wired — see `op_call` plan).
    // Child script: push:7, push:1, return (k=1).
    let child_script = Program::new().push_int(7u64).push_int(1u64).return_().to_bytecode();
    let parent_frame =
        CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500, 0, 0);
    let child_kind = CallKind::CellOpen {
        anchor: Anchor([0u8; 32]),
        predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
        external_context: true,
    };
    let child_frame = CallFrame::new(
        Program::parse(&child_script).expect("parse").into_instructions(),
        child_kind, 500, 0, 0,
    );
    let mut vm = VM::new(dummy_header(), parent_frame);
    let initial_parent = mem::replace(&mut vm.current_call, child_frame);
    vm.call_stack.push(initial_parent);

    // Step until the call_stack collapses back to the parent. Stops
    // before the root finish_call check kicks in (it would error on
    // the leftover 7 because there's no further script to clean it).
    while !vm.call_stack.is_empty() {
        vm.step_internal().unwrap();
    }

    // Parent received the 7 then the count (1) then the success marker (1).
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn break_zero_ends_current_run_only() {
    // Outer: pushstr [break:0, pushint8 99], run
    // — break:0 stops the subprog before pushint8 runs; outer resumes
    //   with empty stack and the tx exits clean.
    let mut script = pushstr_bytes(&[0xb0, 0x10, 99]);
    script.push(0xa1);
    let mut reg = StubRegistry { script };
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block)
        .unwrap();
}

#[test]
fn break_one_ends_subprog_and_outer() {
    // Outer: pushstr [break:1], run, push:99
    //  — subprog issues break:1, which also discards the outer's
    //    resumed run, so push:99 never executes. Outer call exits
    //    with empty stack.
    // (push:99 is encoded as pushint8 + byte, but break:1 makes it
    // unreachable, so we don't even need to keep stack clean for it.)
    let mut script = pushstr_bytes(&[0xb1]); // [break:1]
    script.push(0xa1); // run
    script.push(0x10); // pushint8
    script.push(99);
    let mut reg = StubRegistry { script };
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
}

#[test]
fn break_out_of_call_errors() {
    // Top-level break:1 — but run_stack is empty, so this tries to
    // break past the call boundary.
    let mut vm = vm_with_script(Program::new().break_k(1).to_bytecode());
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::BreakOutOfCall
    ));
}

#[test]
fn break_zero_at_root_ends_cleanly() {
    // break:0 at root: ends current run (which IS the root run),
    // run_stack empty → finish_call with empty stack → clean exit.
    let mut vm = vm_with_script(Program::new().break_k(0).to_bytecode());
    run_until_tx_done(&mut vm).unwrap();
}

#[test]
fn type_pushes_int253_code() {
    // push:5, type, drop, drop — top is type code (0 for Int253), then 5.
    let mut vm = vm_with_script(
        Program::new().push_int(5u64).type_().to_bytecode(),
    );
    vm.step_internal().unwrap(); // push:5
    vm.step_internal().unwrap(); // type
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    assert_int(&vm.current_call.stack[0], Int253::from(5u64));
}

#[test]
fn type_pushes_string_code() {
    let mut script = pushstr_bytes(&[]); // empty string
    script.push(0xa5); // type
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
}

#[test]
fn type_underflow_errors() {
    let mut vm = vm_with_script(Program::new().type_().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}

