//! Tests for fee.

#![allow(unused_imports)]

use super::test_helpers::*;

/// `op_fee` records a `TxEntry::Fee(qty)` and pushes a `WideToken`
/// debt onto the stack. We can't reach a clean exit (WideToken is
/// non-droppable), so step through 2 instructions and inspect.
#[test]
fn phase19_op_fee_records_txlog_and_pushes_debt() {
    let pc_gens = PedersenGens::default();
    // push:100, fee → 2 instructions.
    let program = ScriptBuilder::new().push_int(100u64).fee();
    let (vm, _prover) = run_external_steps(&pc_gens, program, 2);
    // Stack now holds the WideToken debt.
    assert_eq!(vm.current_call.stack.len(), 1);
    let Value::WideToken(wide) = &vm.current_call.stack[0] else {
        panic!("fee must push a WideToken");
    };
    assert_eq!(
        wide.0.assignment.as_deref().expect("prover assignment").f,
        FLAME_FLAVOR.to_dalek(),
    );
    // Txlog: Header at 0, CellWitness at 1, Fee(100) at 2.
    assert_eq!(vm.txlog.len(), 3);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    assert!(matches!(vm.txlog[1], TxEntry::CellWitness(_)));
    assert!(matches!(vm.txlog[2], TxEntry::Fee(100)));
    // total_fee accumulator updated.
    assert_eq!(vm.total_fee.total(), 100);
}

/// Two `op_fee` calls accumulate into `total_fee` and produce
/// two `TxEntry::Fee` entries. Step through 4 instructions:
/// push, fee, push, fee — and stop before the
/// (impossible) clean exit.
#[test]
fn phase19_op_fee_accumulates_total() {
    let pc_gens = PedersenGens::default();
    // After the first fee, a WideToken sits on the stack — the
    // second fee builds another one. The stack will hold both
    // before we inspect. We don't try to clean up.
    let program = ScriptBuilder::new()
        .push_int(30u64)
        .fee()
        .push_int(70u64)
        .fee();
    let (vm, _prover) = run_external_steps(&pc_gens, program, 4);
    // 2 WideTokens stacked.
    assert_eq!(vm.current_call.stack.len(), 2);
    // Txlog: Header + CellWitness + Fee(30) + Fee(70).
    assert_eq!(vm.txlog.len(), 4);
    assert!(matches!(vm.txlog[2], TxEntry::Fee(30)));
    assert!(matches!(vm.txlog[3], TxEntry::Fee(70)));
    // Accumulator carries the sum.
    assert_eq!(vm.total_fee.total(), 100);
}

/// Negative `qty` is rejected at the opcode boundary — fees can't
/// be negative (refunds aren't a Flame concept; the negative half
/// shows up as the debt token, not the recorded amount).
#[test]
fn phase19_op_fee_rejects_negative_qty() {
    let pc_gens = PedersenGens::default();
    // Build script directly so we can push a negative Scalar.
    // pushint8 neg 50 (qty = -50) — minimal, followed by fee.
    let script = vec![0x11, 50, 0x9b];
    let program = ScriptBuilder::parse(&script).expect("decode");
    // Run both instructions; fee must error.
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    vm.step_external(&mut prover).expect("push qty");
    let err = vm.step_external(&mut prover).unwrap_err();
    assert!(matches!(err, VMError::FeeQtyNegative));
}

/// `qty > MAX_FEE` is rejected on the single-fee path.
#[test]
fn phase19_op_fee_rejects_qty_over_cap() {
    let pc_gens = PedersenGens::default();
    // MAX_FEE = 2^24. Push 2^24 + 1.
    let over = (1u64 << 24) + 1;
    let program = ScriptBuilder::new().push_int(over).fee();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    vm.step_external(&mut prover).expect("push qty");
    let err = vm.step_external(&mut prover).unwrap_err();
    assert!(matches!(err, VMError::FeeTooHigh));
}

/// Aggregate total over `MAX_FEE` is rejected: two ok fees that
/// individually fit but sum past the cap. After two MAX_FEE/2
/// fees the running total is MAX_FEE (exactly at cap); adding 2
/// pushes it over.
#[test]
fn phase19_op_fee_rejects_aggregate_over_cap() {
    let pc_gens = PedersenGens::default();
    let half = (1u64 << 24) / 2; // 2^23
    let program = ScriptBuilder::new()
        .push_int(half)
        .fee()
        .push_int(half)
        .fee()
        .push_int(2u64)
        .fee();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    // First fee — running total becomes half (2^23).
    for _ in 0..2 {
        vm.step_external(&mut prover).expect("first fee");
    }
    // Second fee — running total becomes MAX_FEE.
    for _ in 0..2 {
        vm.step_external(&mut prover).expect("second fee");
    }
    assert_eq!(vm.total_fee.total(), 1u64 << 24);
    // Third pair: push, fee — the fee must error
    // FeeTooHigh on aggregate overflow.
    vm.step_external(&mut prover).expect("push qty 3");
    let err = vm.step_external(&mut prover).unwrap_err();
    assert!(matches!(err, VMError::FeeTooHigh));
}

/// `fee` in internal context errors `ExternalOnly`: the opcode
/// needs Bulletproofs to allocate the WideToken's CS variables.
#[test]
fn phase19_op_fee_rejects_internal_context() {
    let script = vec![0x01, 0x9b]; // push:1, fee
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

/// `TxEntry::Fee(qty)` participates in the TxID merkle root —
/// changing `qty` changes the TxID, proving the fee entry is
/// committed by the proof transcript binding.
#[test]
fn phase19_fee_qty_changes_txid() {
    // Compute TxIDs directly off TxEntry sequences (bypasses the
    // CS so we don't need to drain a WideToken to reach a clean
    // exit). Both logs have identical Header; only the Fee qty
    // differs — TxID must diverge.
    use crate::tx::{TxEntry, TxID};
    let header = TxEntry::Header(dummy_header());
    let log_a = vec![TxEntry::Header(dummy_header()), TxEntry::Fee(100)];
    let log_b = vec![TxEntry::Header(dummy_header()), TxEntry::Fee(101)];
    let _ = header; // shut up unused
    let id_a = TxID::from_log(&log_a);
    let id_b = TxID::from_log(&log_b);
    assert_ne!(id_a, id_b, "fee qty must affect TxID");
}
