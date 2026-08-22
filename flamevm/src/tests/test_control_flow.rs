//! Tests for control flow: verify, label/jump/jumpif, the `build_*`
//! combinators, return, and type. See ADR 0015.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn verify_truthy_pops() {
    // push:1, verify — succeeds, stack empties.
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(1u64).verify().to_bytecode());
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn verify_zero_fails() {
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(0u64).verify().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::VerifyFailed
    ));
}

#[test]
fn verify_requires_int() {
    // pushpoint, verify — top is Point not Int253.
    let script = ScriptBuilder::new().push_point([0u8; 32]).verify().to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotInt253
    ));
}

// ── label / jump / jumpif primitives ───────────────────────

#[test]
fn jumpif_taken_skips_forward() {
    // push:1, jumpif L0, push:5, label L0 — true cond jumps past push:5.
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(1u64).jumpif(0).push_int(5u64).label(0).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn jumpif_not_taken_falls_through() {
    // push:0, jumpif L0, push:5, label L0 — false cond runs push:5.
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(0u64).jumpif(0).push_int(5u64).label(0).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(5u64));
}

#[test]
fn jump_unconditional_skips_forward() {
    // jump L0, push:5, label L0 — push:5 never runs.
    let mut vm = vm_with_script(
        ScriptBuilder::new().jump(0).push_int(5u64).label(0).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn jump_to_missing_label_errors() {
    // jump L5 with no label 5 anywhere → scan hits end → LabelNotFound.
    let mut vm = vm_with_script(ScriptBuilder::new().jump(5).to_bytecode());
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::LabelNotFound
    ));
}

#[test]
fn label_out_of_order_errors() {
    // label 1 before label 0 → out of sequence → LabelOutOfOrder.
    let mut vm = vm_with_script(ScriptBuilder::new().label(1).to_bytecode());
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::LabelOutOfOrder
    ));
}

/// Regression guard for ADR 0015's re-visit rule: a loop whose body
/// contains an inner label re-traverses that label every iteration.
/// Without the "same-position re-visit is OK" clause, iteration 2 would
/// hard-fail `LabelOutOfOrder` on `label 1`.
#[test]
fn loop_revisits_inner_label() {
    // n=2; label TOP(0); label INNER(1); n -= 1; dup; jumpif TOP; drop.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(2u64)
            .label(0) // TOP
            .label(1) // inner — re-visited each iteration
            .push_int(-1i64)
            .add()
            .dup_k(0) // copy n (dup_k(0) = duplicate top)
            .jumpif(0) // loop back to TOP while n != 0
            .drop_()
            .to_bytecode(),
    );
    run_until_tx_done(&mut vm).unwrap();
}

// ── build_* combinators ─────────────────────────────────────

#[test]
fn build_if_runs_then_when_true() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(1u64).build_if(|p| p.push_int(9u64)).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(9u64));
}

#[test]
fn build_if_skips_then_when_false() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(0u64).build_if(|p| p.push_int(9u64)).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn build_if_else_picks_else_when_false() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(0u64)
            .build_if_else(|p| p.push_int(9u64), |p| p.push_int(8u64))
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(8u64));
}

#[test]
fn build_if_else_picks_then_when_true() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(1u64)
            .build_if_else(|p| p.push_int(9u64), |p| p.push_int(8u64))
            .to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(9u64));
}

#[test]
fn build_while_counts_down_and_exits() {
    // while (n) { n -= 1 }, starting n=3 — terminates with a clean stack.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(3u64)
            .build_while(|p| p.dup_k(0), |p| p.push_int(-1i64).add())
            .drop_()
            .to_bytecode(),
    );
    run_until_tx_done(&mut vm).unwrap();
}

#[test]
fn build_loop_break_runs_once() {
    // loop { push:7; break } — body runs once, break exits.
    let mut vm = vm_with_script(
        ScriptBuilder::new().build_loop(|p| p.push_int(7u64).build_break()).to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
}

#[test]
fn build_continue_skips_rest_of_body() {
    // while (n) { n -= 1; continue; push:99 } — continue skips push:99
    // every iteration, so the loop exits with a clean stack. A dirty
    // stack (push:99 leaked) would fail the root clean-stack check.
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_int(2u64)
            .build_while(
                |p| p.dup_k(0),
                |p| p.push_int(-1i64).add().build_continue().push_int(99u64),
            )
            .drop_()
            .to_bytecode(),
    );
    run_until_tx_done(&mut vm).unwrap();
}

// ── gas metering (per-instruction; ADR 0009 mechanism) ──────

/// An unbounded loop terminates with OutOfGas instead of hanging — the
/// safety property that replaces "no loops" (ADR 0015).
#[test]
fn infinite_loop_exhausts_gas() {
    let script = ScriptBuilder::new().build_loop(|p| p.nop()).to_bytecode();
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0u8; 32]),
        caller: None,
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(ScriptBuilder::parse(&script).unwrap().into_instructions(), kind, 1_000).with_anchor(Anchor([0u8; 32])),
    );
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::OutOfGas
    ));
}

/// A forward jump's skip-scan charges per scanned instruction, so a long
/// dead region can't be skipped for free.
#[test]
fn skip_scan_charges_gas() {
    // jump L0, then 50 nops, label L0. Budget of 10 < 1 (jump) + 51 scans.
    let mut p = ScriptBuilder::new().jump(0);
    for _ in 0..50 {
        p = p.nop();
    }
    let script = p.label(0).to_bytecode();
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0u8; 32]),
        caller: None,
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(ScriptBuilder::parse(&script).unwrap().into_instructions(), kind, 10).with_anchor(Anchor([0u8; 32])),
    );
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::OutOfGas
    ));
}

// ── allocation gas accounting ─────────────────────────────────────────

#[test]
fn gas_counter_overflow_is_out_of_gas() {
    let mut frame = CallFrame::new(Vec::new(), CallKind::ExternalRoot, u64::MAX);
    frame.gas_used = u64::MAX;
    assert!(matches!(frame.charge_gas(1), Err(VMError::OutOfGas)));
}

/// `writezeros` growth spends gas cumulatively; freeing the previous buffer
/// does not refund it.
#[test]
fn gas_bounds_cumulative_string_growth() {
    let script = ScriptBuilder::new()
        .push_str(String::from(Vec::new()))
        .push_int(60u64)
        .write_zeros()
        .push_int(60u64)
        .write_zeros()
        .to_bytecode();
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0u8; 32]),
        caller: None,
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script).unwrap().into_instructions(),
            kind,
            100,
        ).with_anchor(Anchor([0u8; 32])),
    );
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::OutOfGas
    ));
}

/// Each variable-sized allocation independently spends gas.
#[test]
fn allocation_gas_trips_on_pushstr_append_and_tread() {
    let run_metered = |script: Vec<u8>, gas: u64| {
        let kind = CallKind::InternalRoot {
            actor: ActorID::Hash([0u8; 32]),
            caller: None,
        };
        let mut vm = VM::new(
            dummy_header(),
            CallFrame::new(
                ScriptBuilder::parse(&script).unwrap().into_instructions(),
                kind,
                gas,
            ).with_anchor(Anchor([0u8; 32])),
        );
        run_until_tx_done(&mut vm)
    };

    // pushstr: a 60-byte literal against a 50-byte cap.
    let s = run_metered(
        ScriptBuilder::new().push_str(String::from(vec![7u8; 60])).to_bytecode(),
        50,
    );
    assert!(matches!(s.unwrap_err(), VMError::OutOfGas), "pushstr charges");

    // append: 40 + 40 literals fit a 100 cap (80), the 40-byte append
    // pushes the high-water to 120.
    let s = run_metered(
        ScriptBuilder::new()
            .push_str(String::from(vec![1u8; 40]))
            .push_str(String::from(vec![2u8; 40]))
            .append()
            .to_bytecode(),
        100,
    );
    assert!(matches!(s.unwrap_err(), VMError::OutOfGas), "append charges");

    // tread: a 200-byte challenge squeeze against a 100 cap.
    let s = run_metered(
        ScriptBuilder::new()
            .push_str(String::from(b"L".to_vec()))
            .transcript()
            .push_str(String::from(b"x".to_vec()))
            .push_int(200u64)
            .tread()
            .to_bytecode(),
        100,
    );
    assert!(matches!(s.unwrap_err(), VMError::OutOfGas), "tread charges");
}

// ── return ──────────────────────────────────────────────────

#[test]
fn return_zero_at_root_errors() {
    // push:0, return — root frame has no caller, so `return` errors even
    // with k=0. Scripts terminate cleanly by running off the end instead.
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(0u64).return_().to_bytecode());
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::ReturnAtRoot
    ));
}

#[test]
fn return_nonzero_at_root_errors() {
    // push:7, push:1, return — k=1 at root: nowhere for 7 to go.
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(7u64).push_int(1u64).return_().to_bytecode(),
    );
    assert!(matches!(
        run_until_tx_done(&mut vm).unwrap_err(),
        VMError::ReturnAtRoot
    ));
}

#[test]
fn return_with_dirty_leftover_errors() {
    // Inside a child frame: push:9, push:7, push:1, return — k=1, two
    // items below count → StackNotClean. Error is caught and translated
    // to `[count=0, success=0]` on the parent.
    let mut vm = vm_with_nested_child_script(
        ScriptBuilder::new()
            .push_int(9u64).push_int(7u64).push_int(1u64).return_()
            .to_bytecode(),
    );
    while !vm.call_stack.is_empty() {
        vm.step_internal().expect("step ok — error swallowed into marker");
    }
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Int253::ZERO);
    assert_int(&vm.current_call.stack[1], Int253::ZERO);
}

#[test]
fn return_too_few_items_errors() {
    // Inside a child frame: push:5, return — k=5 popped, zero items
    // remain → BadReturnArity. The `step` wrapper catches the error,
    // unwinds the child, and pushes `[count=0, success=0]` onto the parent.
    let mut vm = vm_with_nested_child_script(
        ScriptBuilder::new().push_int(5u64).return_().to_bytecode(),
    );
    while !vm.call_stack.is_empty() {
        vm.step_internal().expect("step ok — error swallowed into marker");
    }
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Int253::ZERO);
    assert_int(&vm.current_call.stack[1], Int253::ZERO);
}

#[test]
fn return_transfers_values_to_parent() {
    // Child script: push:7, push:1, return (k=1).
    let child_script = ScriptBuilder::new().push_int(7u64).push_int(1u64).return_().to_bytecode();
    let parent_frame =
        CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500);
    let child_kind = CallKind::CellOpen {
        predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
        external_context: true,
    };
    let child_frame = CallFrame::new(
        ScriptBuilder::parse(&child_script).expect("parse").into_instructions(),
        child_kind, 500,
    );
    let mut vm = VM::new(dummy_header(), parent_frame);
    let initial_parent = mem::replace(&mut vm.current_call, child_frame);
    vm.call_stack.push(initial_parent);

    // Step until the call_stack collapses back to the parent.
    while !vm.call_stack.is_empty() {
        vm.step_internal().unwrap();
    }

    // Parent received the 7 then the count (1) then the success marker (1).
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

// ── type ────────────────────────────────────────────────────

#[test]
fn type_pushes_int253_code() {
    // push:5, type — top is type code (0 for Int253), then 5.
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(5u64).type_().to_bytecode(),
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
    let mut vm = vm_with_script(ScriptBuilder::new().type_().to_bytecode());
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::StackUnderflow
    ));
}
