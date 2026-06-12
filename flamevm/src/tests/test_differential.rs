//! Differential tests: the two execution backends — `Code::Instrs`
//! (prover, pre-decoded) and `Code::Bytes` (verifier, streaming decode)
//! — must agree on result, gas, and emitted effects for the same
//! bytecode. Plus decoder-robustness sweeps over adversarial bytes.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::tx::TxID;

fn internal_kind() -> CallKind {
    CallKind::InternalRoot {
        actor: ActorID::Hash([0u8; 32]),
        caller: None,
        anchor: Anchor([0u8; 32]),
    }
}

/// Runs `bytecode` through both backends and asserts identical
/// (result, gas_used, TxID-over-txlog).
fn assert_backends_agree(bytecode: &[u8]) {
    let mut vm_instrs = VM::new(
        dummy_header(),
        CallFrame::new(
            Program::parse(bytecode).expect("parse").into_instructions(),
            internal_kind(),
            100_000,
            0,
            0,
        ),
    );
    let r_instrs = run_until_tx_done(&mut vm_instrs);

    let mut vm_bytes = VM::new(
        dummy_header(),
        CallFrame::from_bytecode(bytecode.to_vec(), internal_kind(), 100_000, 0, 0),
    );
    let r_bytes = run_until_tx_done(&mut vm_bytes);

    assert_eq!(
        format!("{r_instrs:?}"),
        format!("{r_bytes:?}"),
        "result divergence on {bytecode:02x?}"
    );
    assert_eq!(
        vm_instrs.current_call.gas_used, vm_bytes.current_call.gas_used,
        "gas divergence on {bytecode:02x?}"
    );
    assert_eq!(
        TxID::from_log(&vm_instrs.txlog),
        TxID::from_log(&vm_bytes.txlog),
        "txlog divergence on {bytecode:02x?}"
    );
}

#[test]
fn backends_agree_across_sample_programs() {
    let programs: Vec<Program> = vec![
        // Arithmetic, clean exit.
        Program::new().push_int(2u64).push_int(3u64).add().drop_(),
        // Countdown loop (label re-visit + back-edges).
        Program::new()
            .push_int(3u64)
            .build_while(|p| p.dup_k(0), |p| p.push_int(-1i64).add())
            .drop_(),
        // Branching, both arms.
        Program::new()
            .push_int(1u64)
            .build_if_else(|p| p.push_int(9u64), |p| p.push_int(8u64))
            .drop_(),
        Program::new()
            .push_int(0u64)
            .build_if_else(|p| p.push_int(9u64), |p| p.push_int(8u64))
            .drop_(),
        // Forward jump skip-scan over dead code.
        Program::new().jump(0).push_int(7u64).push_int(8u64).label(0),
        // String growth (charges mem identically) + type probe.
        Program::new()
            .push_str(String::from(b"ab".to_vec()))
            .push_str(String::from(b"cd".to_vec()))
            .append()
            .drop_(),
        Program::new().push_int(5u64).type_().drop_().drop_(),
        // Failing programs must fail identically.
        Program::new().push_int(0u64).verify(),
        Program::new().label(1), // out-of-order label
        Program::new().jump(9), // missing label
        Program::new().push_int(7u64), // dirty stack at exit
    ];
    for p in programs {
        assert_backends_agree(&p.to_bytecode());
    }
}

/// Deterministic pseudo-random byte strings must never panic the
/// bytecode parser or the value decoder — reject or succeed, only.
#[test]
fn decoders_never_panic_on_random_bytes() {
    let mut state = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state = state.wrapping_mul(0x2545F4914F6CDD1D);
        state
    };
    for _ in 0..5_000 {
        let len = (next() % 64) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
        let _ = Program::parse(&bytes);
        let mut r: &[u8] = &bytes;
        let _ = crate::encoding::read_value(&mut r);
    }
}

/// Single-byte mutations of a valid program must parse-or-reject
/// without panicking, and executing the mutants through both backends
/// must never diverge.
#[test]
fn mutated_programs_never_panic_or_diverge() {
    let base = Program::new()
        .push_int(3u64)
        .build_while(|p| p.dup_k(0), |p| p.push_int(-1i64).add())
        .drop_()
        .to_bytecode();
    for i in 0..base.len() {
        for delta in [1u8, 0x7f, 0xff] {
            let mut mutant = base.clone();
            mutant[i] = mutant[i].wrapping_add(delta);
            // Parse may reject; if it parses, both backends must agree.
            if Program::parse(&mutant).is_ok() {
                assert_backends_agree(&mutant);
            }
        }
    }
}
