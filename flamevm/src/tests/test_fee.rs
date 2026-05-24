//! Tests for fee.

#![allow(unused_imports)]

use super::test_helpers::*;

/// `op_fee` records a `TxEntry::Fee(qty)` and pushes a `WideToken`
/// debt onto the stack. We can't reach a clean exit (WideToken is
/// non-droppable), so step through 3 instructions and inspect.
#[test]
fn phase19_op_fee_records_txlog_and_pushes_debt() {
    let pc_gens = PedersenGens::default();
    // push:100, push:7, fee → 3 instructions.
    let program = Program::new()
        .push_int(100u64)
        .push_int(7u64)
        .fee();
    let (vm, _prover) = run_external_steps(&pc_gens, program, 3);
    // Stack now holds the WideToken debt.
    assert_eq!(vm.current_call.stack.len(), 1);
    assert!(matches!(vm.current_call.stack[0], Value::WideToken(_)));
    // Txlog: Header at 0, Fee(100) at 1.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    assert!(matches!(vm.txlog[1], crate::tx::TxEntry::Fee(100)));
    // total_fee accumulator updated.
    assert_eq!(vm.total_fee.total(), 100);
}

/// Two `op_fee` calls accumulate into `total_fee` and produce
/// two `TxEntry::Fee` entries. Step through 7 instructions:
/// push, push, fee, push, push, fee — and stop before the
/// (impossible) clean exit.
#[test]
fn phase19_op_fee_accumulates_total() {
    let pc_gens = PedersenGens::default();
    // After the first fee, a WideToken sits on the stack — the
    // second fee builds another one. The stack will hold both
    // before we inspect. We don't try to clean up.
    let program = Program::new()
        .push_int(30u64)
        .push_int(0u64)
        .fee()
        .push_int(70u64)
        .push_int(0u64)
        .fee();
    let (vm, _prover) = run_external_steps(&pc_gens, program, 6);
    // 2 WideTokens stacked.
    assert_eq!(vm.current_call.stack.len(), 2);
    // Txlog: Header + Fee(30) + Fee(70).
    assert_eq!(vm.txlog.len(), 3);
    assert!(matches!(vm.txlog[1], crate::tx::TxEntry::Fee(30)));
    assert!(matches!(vm.txlog[2], crate::tx::TxEntry::Fee(70)));
    // Accumulator carries the sum.
    assert_eq!(vm.total_fee.total(), 100);
}

/// Negative `qty` is rejected at the opcode boundary — fees can't
/// be negative (refunds aren't a Flame concept; the negative half
/// shows up as the debt token, not the recorded amount).
#[test]
fn phase19_op_fee_rejects_negative_qty() {
    let pc_gens = PedersenGens::default();
    // Build script directly so we can push a negative Int253.
    let mut script = Vec::new();
    // pushint8 neg 50 (qty = -50)
    script.push(0x11);
    script.push(50);
    // pushint8 pos 0 (flv = 0)
    script.push(0x10);
    script.push(0);
    // fee
    script.push(0x7a);
    let program = crate::Program::parse(&script).expect("decode");
    // Run all 3 instructions; the third (fee) must error.
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    vm.step_external(&mut prover).expect("push qty");
    vm.step_external(&mut prover).expect("push flv");
    let err = vm.step_external(&mut prover).unwrap_err();
    assert!(matches!(err, VMError::FeeQtyNegative));
}

/// `qty > MAX_FEE` is rejected on the single-fee path.
#[test]
fn phase19_op_fee_rejects_qty_over_cap() {
    let pc_gens = PedersenGens::default();
    // MAX_FEE = 2^24. Push 2^24 + 1.
    let over = (1u64 << 24) + 1;
    let program = Program::new().push_int(over).push_int(0u64).fee();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    vm.step_external(&mut prover).expect("push qty");
    vm.step_external(&mut prover).expect("push flv");
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
    let program = Program::new()
        .push_int(half)
        .push_int(0u64)
        .fee()
        .push_int(half)
        .push_int(0u64)
        .fee()
        .push_int(2u64)
        .push_int(0u64)
        .fee();
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
            0,
            0,
        ),
    );
    let mut prover = Prover::new(&pc_gens);
    // First fee — running total becomes half (2^23).
    for _ in 0..3 {
        vm.step_external(&mut prover).expect("first triple");
    }
    // Second fee — running total becomes MAX_FEE.
    for _ in 0..3 {
        vm.step_external(&mut prover).expect("second triple");
    }
    assert_eq!(vm.total_fee.total(), 1u64 << 24);
    // Third triple: push, push, fee — the fee must error
    // FeeTooHigh on aggregate overflow.
    vm.step_external(&mut prover).expect("push qty 3");
    vm.step_external(&mut prover).expect("push flv 3");
    let err = vm.step_external(&mut prover).unwrap_err();
    assert!(matches!(err, VMError::FeeTooHigh));
}

/// `fee` in internal context errors `ExternalOnly`: the opcode
/// needs Bulletproofs to allocate the WideToken's CS variables.
#[test]
fn phase19_op_fee_rejects_internal_context() {
    let mut script = Vec::new();
    // qty=1, flv=0, fee
    script.push(0x01); // push:1
    script.push(0x00); // push:0
    script.push(0x7a); // fee
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

/// `Fee` instruction roundtrips through encode/parse.
#[test]
fn phase19_fee_instruction_roundtrip() {
    use crate::ops::Instruction;
    let mut buf = Vec::new();
    Instruction::Fee.encode(&mut buf);
    assert_eq!(buf, vec![0x7a]);
    let mut r: &[u8] = &buf;
    assert!(matches!(
        Instruction::parse(&mut r).expect("parses"),
        Instruction::Fee
    ));
}

/// `TxEntry::Fee(qty)` participates in the TxID merkle root —
/// changing `qty` changes the TxID, proving the fee entry is
/// committed by the proof transcript binding (Phase 18).
#[test]
fn phase19_fee_qty_changes_txid() {
    // Compute TxIDs directly off TxEntry sequences (bypasses the
    // CS so we don't need to drain a WideToken to reach a clean
    // exit). Both logs have identical Header; only the Fee qty
    // differs — TxID must diverge.
    use crate::tx::{TxEntry, TxID};
    let header = TxEntry::Header(dummy_header());
    let log_a = vec![
        TxEntry::Header(dummy_header()),
        TxEntry::Fee(100),
    ];
    let log_b = vec![
        TxEntry::Header(dummy_header()),
        TxEntry::Fee(101),
    ];
    let _ = header; // shut up unused
    let id_a = TxID::from_log(&log_a);
    let id_b = TxID::from_log(&log_b);
    assert_ne!(id_a, id_b, "fee qty must affect TxID");
}

