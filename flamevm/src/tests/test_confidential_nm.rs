//! Tests for confidential nm.

#![allow(unused_imports)]

use super::test_helpers::*;

/// N=1, M=1, 1 flavor: simplest possible confidential transfer.
#[test]
fn confidential_1_to_1_single_flavor() {
    let _ = run_confidential_nm(
        &[NMInputSpec {
            qty: 100,
            flv: 7,
            qty_blind: 11,
            flv_blind: 13,
            anchor: [0xa1; 32],
        }],
        &[NMOutputSpec {
            qty: 100,
            flv: 7,
            qty_blind: 17,
            flv_blind: 19,
            predicate_tag: 0xb1,
        }],
    );
}

/// N=1, M=2, 1 flavor: split 10 → [4, 6].
#[test]
fn confidential_1_to_2_single_flavor_split() {
    let _ = run_confidential_nm(
        &[NMInputSpec {
            qty: 10,
            flv: 7,
            qty_blind: 11,
            flv_blind: 13,
            anchor: [0xa1; 32],
        }],
        &[
            NMOutputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 6,
                flv: 7,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
        ],
    );
}

/// N=2, M=1, 1 flavor: merge [3, 7] → 10.
#[test]
fn confidential_2_to_1_single_flavor_merge() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 3,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 7,
                flv: 7,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
        ],
        &[NMOutputSpec {
            qty: 10,
            flv: 7,
            qty_blind: 21,
            flv_blind: 22,
            predicate_tag: 0xb1,
        }],
    );
}

/// N=2, M=2, 1 flavor: 4-way shuffle / re-blind. Both sides
/// total 12 (5+7 = 4+8).
#[test]
fn confidential_2_to_2_single_flavor() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 5,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 7,
                flv: 7,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
        ],
        &[
            NMOutputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 8,
                flv: 7,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
        ],
    );
}

/// N=2, M=2, 2 flavors: gold (flv=7) and silver (flv=11)
/// balanced separately.
#[test]
fn confidential_2_to_2_two_flavors() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 10,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 25,
                flv: 11,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
        ],
        &[
            NMOutputSpec {
                qty: 10,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 25,
                flv: 11,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
        ],
    );
}

/// N=3, M=3, 2 flavors: full shuffle. Gold: 5+5 → 7+3. Silver: 8 → 8.
#[test]
fn confidential_3_to_3_two_flavors() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 5,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 5,
                flv: 7,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
            NMInputSpec {
                qty: 8,
                flv: 11,
                qty_blind: 16,
                flv_blind: 17,
                anchor: [0xa3; 32],
            },
        ],
        &[
            NMOutputSpec {
                qty: 7,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 3,
                flv: 7,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
            NMOutputSpec {
                qty: 8,
                flv: 11,
                qty_blind: 41,
                flv_blind: 42,
                predicate_tag: 0xb3,
            },
        ],
    );
}

/// N=3, M=2, 2 flavors. Gold 10+5+0 → 15; Silver 8 → 8.
/// Wait, three inputs but only one flavor on first two... let's
/// do Gold 5+5 → 10 and Silver 8 → 8 (3→2).
#[test]
fn confidential_3_to_2_two_flavors() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 5,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 5,
                flv: 7,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
            NMInputSpec {
                qty: 8,
                flv: 11,
                qty_blind: 16,
                flv_blind: 17,
                anchor: [0xa3; 32],
            },
        ],
        &[
            NMOutputSpec {
                qty: 10,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 8,
                flv: 11,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
        ],
    );
}

/// N=1, M=3, 1 flavor: split 12 → 4+4+4.
#[test]
fn confidential_1_to_3_single_flavor() {
    let _ = run_confidential_nm(
        &[NMInputSpec {
            qty: 12,
            flv: 7,
            qty_blind: 11,
            flv_blind: 13,
            anchor: [0xa1; 32],
        }],
        &[
            NMOutputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
            NMOutputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 41,
                flv_blind: 42,
                predicate_tag: 0xb3,
            },
        ],
    );
}

/// N=3, M=1, 1 flavor: merge 4+4+4 → 12.
#[test]
fn confidential_3_to_1_single_flavor() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
            NMInputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 16,
                flv_blind: 17,
                anchor: [0xa3; 32],
            },
        ],
        &[NMOutputSpec {
            qty: 12,
            flv: 7,
            qty_blind: 21,
            flv_blind: 22,
            predicate_tag: 0xb1,
        }],
    );
}

/// N=3, M=3, 1 flavor: re-balance 1+2+3 → 2+2+2.
#[test]
fn confidential_3_to_3_single_flavor() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 1,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 2,
                flv: 7,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
            NMInputSpec {
                qty: 3,
                flv: 7,
                qty_blind: 16,
                flv_blind: 17,
                anchor: [0xa3; 32],
            },
        ],
        &[
            NMOutputSpec {
                qty: 2,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 2,
                flv: 7,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
            NMOutputSpec {
                qty: 2,
                flv: 7,
                qty_blind: 41,
                flv_blind: 42,
                predicate_tag: 0xb3,
            },
        ],
    );
}

/// N=2, M=3, 2 flavors. Gold 10 → 4+6; Silver 8 → 8 — total
/// 2 inputs and 3 outputs.
#[test]
fn confidential_2_to_3_two_flavors() {
    let _ = run_confidential_nm(
        &[
            NMInputSpec {
                qty: 10,
                flv: 7,
                qty_blind: 11,
                flv_blind: 13,
                anchor: [0xa1; 32],
            },
            NMInputSpec {
                qty: 8,
                flv: 11,
                qty_blind: 14,
                flv_blind: 15,
                anchor: [0xa2; 32],
            },
        ],
        &[
            NMOutputSpec {
                qty: 4,
                flv: 7,
                qty_blind: 21,
                flv_blind: 22,
                predicate_tag: 0xb1,
            },
            NMOutputSpec {
                qty: 6,
                flv: 7,
                qty_blind: 31,
                flv_blind: 32,
                predicate_tag: 0xb2,
            },
            NMOutputSpec {
                qty: 8,
                flv: 11,
                qty_blind: 41,
                flv_blind: 42,
                predicate_tag: 0xb3,
            },
        ],
    );
}

/// Confidential transfer composed with `op_fee`: one input
/// (qty=10) → one output (qty=7) + fee=3, all flavor 0. The
/// fee opcode produces a `WideToken` debt with q=-3 that is
/// consumed as an *input* of `mix`, balancing the cleartext
/// fee against the surplus on the real inputs.
///
/// Script shape (single flavor 0):
///
///   pushstr <cell>; input(w); <taproot_proof>; push:0; open
///       → stack: [Token(10, 0)]
///   push:3; push:0; fee
///       → stack: [Token(10, 0), WideToken(-3, 0)]
///       → txlog: …Header, Input, Fee(3)
///   push qty_open(7); push flv_open(0)
///   push:2 (m); push:1 (n); mix
///       → stack: [out_Token(7, 0)]
///   push:1; pushpoint <out_pred>; output
///       → stack: empty; txlog: …Header, Input, Fee(3), Output
///
/// Asserts both the txlog ordering and that `TxResult.total_fee`
/// picked up the cleartext 3.
#[test]
fn confidential_1_to_1_with_fee() {
    let pc_gens = PedersenGens::default();

    let inp = NMInputSpec {
        qty: 10,
        flv: 0,
        qty_blind: 11,
        flv_blind: 13,
        anchor: [0xa1; 32],
    };
    let (cell, cp) = build_input_cell(&inp);
    let expected_input_id = cell.id();

    let out = NMOutputSpec {
        qty: 7,
        flv: 0,
        qty_blind: 21,
        flv_blind: 22,
        predicate_tag: 0xb1,
    };
    let (q_out, f_out) = open_commitments_for_output(&out);

    let mut program = ScriptBuilder::new();
    // Consume the input cell — push the witness-bearing String::Cell
    // so the Token's open commitments survive into the CS.
    program = program.push_str(crate::String::cell(cell));
    program = program.input();
    program = push_taproot_proof_to_program(program, &cp);
    program = program
        .push_int(1024u64) // gas
        .push_int(1024u64) // bytes
        .push_int(0u64)    // k args
        .open()
        .verify()          // assert success marker
        .drop_();          // discard count
    // Fee opcode: pushes WideToken(-3, 0).
    program = program.push_int(3u64).push_int(0u64).fee();
    // Output commitment Strings (witness-bearing prover-side).
    // Clone so the post-prove assertion can still read the
    // expected points off the local Open commitments.
    program = program
        .push_str(crate::String::commitment(q_out.clone()))
        .push_str(crate::String::commitment(f_out.clone()));
    // mix: m=2 (real Token + fee WideToken), n=1 (output).
    program = program.push_int(2u64).push_int(1u64).mix();
    // Emit the output cell.
    let out_pred = output_predicate_point(out.predicate_tag);
    program = program
        .push_int(1u64)
        .push_point(*out_pred.as_bytes())
        .output();

    let prover_result =
        Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
            .expect("prove ok");

    // total_fee picked up the cleartext amount.
    assert_eq!(prover_result.total_fee, 3);

    // Txlog layout: Header, Input, Fee(3), Output.
    assert_eq!(prover_result.txlog.len(), 4);
    assert!(matches!(
        prover_result.txlog[0],
        crate::tx::TxEntry::Header(_)
    ));
    match &prover_result.txlog[1] {
        crate::tx::TxEntry::Input(id) => assert_eq!(*id, expected_input_id),
        _ => panic!("txlog[1] must be Input"),
    }
    match &prover_result.txlog[2] {
        crate::tx::TxEntry::Fee(q) => assert_eq!(*q, 3),
        _ => panic!("txlog[2] must be Fee(3)"),
    }
    match &prover_result.txlog[3] {
        crate::tx::TxEntry::Output(c) => {
            assert_eq!(c.predicate.to_point(), out_pred);
            let token = match &c.payload[0] {
                Value::Token(t) => t,
                _ => panic!("output payload[0] must be Token"),
            };
            assert_eq!(token.qty.to_point(), q_out.to_point());
            assert_eq!(token.flv.to_point(), f_out.to_point());
        }
        _ => panic!("txlog[3] must be Output"),
    }

    // Verifier round-trip.
    let txid_p = prover_result.txid;
    let TxResult { bytecode, proof, .. } = prover_result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let v = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        0,
        None,
    )
    .expect("verify ok");
    assert_eq!(v.txid, txid_p);
    assert_eq!(v.total_fee, 3);
}

/// Quantity imbalance: input sum ≠ output sum within a flavor.
/// CS solve must fail → `R1CSError` or `InvalidR1CSProof` on
/// verify. The prover may either error directly or produce a
/// proof the verifier rejects; both are acceptable.
#[test]
fn confidential_unbalanced_inputs_rejected() {
    let pc_gens = PedersenGens::default();
    let inputs = vec![NMInputSpec {
        qty: 10,
        flv: 7,
        qty_blind: 11,
        flv_blind: 13,
        anchor: [0xa1; 32],
    }];
    // Output sums to 11 — imbalance.
    let outputs = vec![NMOutputSpec {
        qty: 11,
        flv: 7,
        qty_blind: 21,
        flv_blind: 22,
        predicate_tag: 0xb1,
    }];
    let program = build_confidential_nm_program(&inputs, &outputs);
    let prove_attempt = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    );
    match prove_attempt {
        Err(_) => { /* prover refused — good */ }
        Ok(result) => {
            let TxResult { bytecode, proof, .. } = result;
            let proof = proof.expect("proof set");
            let pc_gens_v = PedersenGens::default();
            let err = Verifier::verify(
                &pc_gens_v,
                bytecode,
                &proof,
                dummy_header(),
                1_000_000,
                0,
                None,
            )
            .expect_err("verifier must reject imbalance");
            assert!(matches!(err, VMError::InvalidR1CSProof));
        }
    }
}

/// Flavor mismatch: output flavor not present in inputs.
#[test]
fn confidential_flavor_mismatch_rejected() {
    let pc_gens = PedersenGens::default();
    let inputs = vec![NMInputSpec {
        qty: 10,
        flv: 7, // gold
        qty_blind: 11,
        flv_blind: 13,
        anchor: [0xa1; 32],
    }];
    // Output flavor is silver (11) — different from input flavor
    // (7). CS / cloak gadget must reject.
    let outputs = vec![NMOutputSpec {
        qty: 10,
        flv: 11,
        qty_blind: 21,
        flv_blind: 22,
        predicate_tag: 0xb1,
    }];
    let program = build_confidential_nm_program(&inputs, &outputs);
    let prove_attempt = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    );
    match prove_attempt {
        Err(_) => { /* prover refused — good */ }
        Ok(result) => {
            let TxResult { bytecode, proof, .. } = result;
            let proof = proof.expect("proof set");
            let pc_gens_v = PedersenGens::default();
            let err = Verifier::verify(
                &pc_gens_v,
                bytecode,
                &proof,
                dummy_header(),
                1_000_000,
                0,
                None,
            )
            .expect_err("verifier must reject flavor mismatch");
            assert!(matches!(err, VMError::InvalidR1CSProof));
        }
    }
}

/// Fee + undersupply: input qty=10, output qty=10, fee=3.
/// The mix balance is `10 = 10 + (-3)` (i.e. 10 = 7) — false.
/// Cloak must reject.
///
/// The most realistic real-world mistake when composing fees
/// with a transfer: forgetting to reduce the output by the fee
/// amount. The 1→1+fee positive test (qty 10 → qty 7 + fee 3)
/// covers the success path; this one covers the obvious slip.
#[test]
fn confidential_with_fee_undersupply_rejected() {
    let pc_gens = PedersenGens::default();

    let inp = NMInputSpec {
        qty: 10,
        flv: 0,
        qty_blind: 11,
        flv_blind: 13,
        anchor: [0xa1; 32],
    };
    let (cell, cp) = build_input_cell(&inp);

    // Output qty = 10 (NOT 7) — fee is unfunded.
    let out = NMOutputSpec {
        qty: 10,
        flv: 0,
        qty_blind: 21,
        flv_blind: 22,
        predicate_tag: 0xb1,
    };
    let (q_out, f_out) = open_commitments_for_output(&out);

    // Same script shape as the positive fee test, just with
    // the unbalanced output qty.
    let out_pred = output_predicate_point(out.predicate_tag);
    let mut program = ScriptBuilder::new();
    program = program.push_str(crate::String::cell(cell));
    program = program.input();
    program = push_taproot_proof_to_program(program, &cp);
    program = program
        .push_int(1024u64)
        .push_int(1024u64)
        .push_int(0u64)
        .open()
        .verify()
        .drop_();
    program = program.push_int(3u64).push_int(0u64).fee();
    program = program
        .push_str(crate::String::commitment(q_out))
        .push_str(crate::String::commitment(f_out));
    program = program.push_int(2u64).push_int(1u64).mix();
    program = program
        .push_int(1u64)
        .push_point(*out_pred.as_bytes())
        .output();

    let prove_attempt = Prover::prove(
        &pc_gens,
        program,
        dummy_header(),
        1_000_000,
        0,
    );
    match prove_attempt {
        Err(_) => { /* prover refused the bad witness — good */ }
        Ok(result) => {
            let TxResult { bytecode, proof, .. } = result;
            let proof = proof.expect("proof set");
            let pc_gens_v = PedersenGens::default();
            let err = Verifier::verify(
                &pc_gens_v,
                bytecode,
                &proof,
                dummy_header(),
                1_000_000,
                0,
                None,
            )
            .expect_err("verifier must reject fee undersupply");
            assert!(matches!(err, VMError::InvalidR1CSProof));
        }
    }
}

