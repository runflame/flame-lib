//! Tests for contracts.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::encoding::{write_list_prefix, write_value};
use crate::state_root;

/// `Contract::decode` must reject an oversized payload-count prefix before
/// allocating — a tiny hostile input claiming billions of payload items
/// is unsatisfiable and must error, not OOM (audit p7 critical).
#[test]
fn contract_decode_rejects_payload_count_bomb() {
    use readerwriter::Decodable;
    // A valid empty-payload contract ends with the payload list prefix
    // (LIST_IMM 0 = 128). Swap it for LIST_VAR + a ~4.3 GB sub-varint
    // count (U32 form, below the U64-overflow guard so we exercise the
    // remaining-bytes bound itself).
    let contract = Contract::new(
        Predicate::opaque(CompressedRistretto([2u8; 32])),
        Anchor([0u8; 32]),
        Vec::new(),
    )
    .expect("empty payload is portable");
    let mut bytes = contract.to_bytes();
    assert_eq!(*bytes.last().unwrap(), 128, "empty-payload list prefix");
    bytes.pop();
    bytes.push(187); // LIST_VAR
    bytes.push(2); // SUBVARINT_U32
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    let mut r: &[u8] = &bytes;
    assert!(Contract::decode(&mut r).is_err());
}

#[test]
fn contract_opcode_requires_seeded_anchor() {
    // push:7 (payload), push:1 (count), pushpoint(some), contract —
    // run from an ExternalRoot frame whose last_anchor is `None`
    // (no prior `op_input` to seed it). `op_contract` must hard-fail.
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract()
        .to_bytecode();
    let mut vm = vm_external_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::AnchorMissing));
}

#[test]
fn contract_opcode_builds_a_contract_and_ratchets_anchor() {
    // Seed an anchor, then build a contract.
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    let seed = Anchor([0x42; 32]);
    vm.last_anchor = Some(seed);
    run_to_end(&mut vm).unwrap();
    // The contract took the LEFT half of split(seed) as its anchor;
    // last_anchor is now the RIGHT half.
    let (expected_left, expected_right) = seed.split();
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Contract(c) => {
            assert_eq!(c.anchor.0, expected_left.0);
            assert_eq!(vm.last_anchor.unwrap().0, expected_right.0);
        }
        other => panic!("expected Contract, got {}", value_kind(other)),
    }
}

#[test]
fn contract_opcode_rejects_non_portable_payload() {
    // Merlin is always non-portable.
    let script = ScriptBuilder::new()
        .push_str(String::from(Vec::<u8>::new())) // empty label
        .transcript() // → Merlin (non-portable)
        .push_int(1u64) // count
        .push_point([0xaa; 32])
        .contract()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn contract_constructor_rejects_nested_non_portable_payload() {
    let mut inner = Dict::new();
    inner.insert(Scalar::ZERO, Value::Merlin(Merlin::new(b"test")));
    let mut outer = Dict::new();
    outer.insert(Scalar::ZERO, Value::Dict(inner));
    assert!(!outer.is_portable());

    assert!(matches!(
        Contract::new(
            Predicate::opaque(CompressedRistretto([2u8; 32])),
            Anchor([0u8; 32]),
            vec![Value::Dict(outer)],
        ),
        Err(VMError::NonPortableInOutput)
    ));
}

#[test]
fn contract_is_noncopyable_and_nondroppable() {
    // build contract, then dup → TypeNotCopyable
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract()
        .dup_k(0)
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));

    // build contract, then drop → TypeNotDroppable
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract()
        .drop_()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotDroppable
    ));
}

#[test]
fn output_opcode_emits_to_txlog_without_pushing() {
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .output()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Stack is empty.
    assert!(vm.current_call.stack.is_empty());
    // Txlog has Header + one Output entry. The Header is always
    // emitted at VM::new so TxID::from_log binds to
    // version + locktime alongside the effects.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Output(_) => {}
        _ => panic!("expected Output entry"),
    }
}

#[test]
fn open_with_valid_taproot_proof_runs_program() {
    // Contract payload: 5. Inner program: drop, push:0, return — drains
    // the payload inside the isolated ContractOpen frame and returns 0
    // items to parent (ADR 0013). On clean return parent's stack
    // gets [count=0, success=1].
    let inner_program = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let (tree, cp) = build_predicate_with_program(&inner_program, 7);
    let pred_point = tree.point;

    let mut program = ScriptBuilder::new()
        .push_int(5u64) // payload
        .push_int(1u64) // count
        .push_point(*pred_point.as_bytes())
        .contract();
    program = push_taproot_proof_to_program(program, &cp);
    let script = program
        .push_int(1024u64) // gas
        .push_int(0u64) // k = 0 args
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Parent stack: [count=0, success=1].
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Scalar::from(0u64));
    assert_int(&vm.current_call.stack[1], Scalar::from(1u64));
}

#[test]
fn open_with_wrong_program_hard_fails() {
    // Predicate commits to one leaf; taproot_proof claims a different one.
    let real_program = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let fake_program = ScriptBuilder::new()
        .nop()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let (tree, _real_cp) = build_predicate_with_program(&real_program, 7);
    let cp = TaprootProof {
        internal_key: tree.internal_key,
        neighbors: Vec::new(),
        position: Vec::new(),
        program: fake_program,
    };
    let pred_point = tree.point;

    let mut program = ScriptBuilder::new()
        .push_int(5u64)
        .push_int(1u64)
        .push_point(*pred_point.as_bytes())
        .contract();
    program = push_taproot_proof_to_program(program, &cp);
    let script = program
        .push_int(1024u64)
        .push_int(0u64)
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TaprootProofMismatch
    ));
}

/// Prover-side: the unlock script pushed as the taproot_proof's
/// `program` component can be a `StringWitness::Script(instrs)` carrying
/// witnesses. `op_open` verifies the taproot_proof against the contract's
/// predicate (the bytes must match the leaf stored in the
/// predicate tree), then uses `program_str.to_instructions()` so
/// the witness slots survive into the new Run. End-to-end via
/// Prover::prove → Verifier::verify.
///
/// Regression guard for the witness-erasing path that existed
/// before `op_open` switched from re-parsing `verify_taproot_proof`'s
/// returned bytes to using the stack `program_str` directly.
#[test]
fn open_preserves_alloc_witnesses_via_script_string() {
    // Inner unlock script with `alloc(Some(_))` witnesses. Ends with
    // `push:0, return` so the isolated ContractOpen frame exits cleanly
    // after `verify` drains the constraint (ADR 0013).
    let inner = ScriptBuilder::new()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(10u64)))
        .eq()
        .verify()
        .push_int(0u64)
        .return_();
    let inner_bytes = inner.to_bytecode();

    // Single-leaf predicate tree whose leaf == inner_bytes. The
    // NUMS-unspendable internal key means the only spend path is
    // the script leaf.
    let tree = PredicateTree::scripts_only(vec![inner_bytes.clone()], TEST_BLINDING_KEY)
        .expect("scripts_only tree");
    let cp = tree.taproot_proof_for(0).expect("taproot_proof for leaf 0");
    let pred_point = tree.point;

    // Construct the input contract with an empty payload — the witness
    // we care about lives in the unlock script, not the payload.
    let contract = Contract::new(Predicate::opaque(pred_point), Anchor([0xa1; 32]), vec![])
        .expect("empty payload is portable");
    let contract_bytes = encode_contract_to_bytes(&contract);

    // Outer ScriptBuilder:
    //   pushstr <contract_bytes>; input;
    //   pushpoint <internal_key>;
    //   for each neighbor: pushstr <h>; push:i;     // N neighbors
    //   push:N; dict;
    //   pushstr <position>;
    //   push_script(inner);                          // witness-bearing
    //   push:0; open
    let mut outer = ScriptBuilder::new()
        .push_str(String::from(contract_bytes))
        .input()
        .push_point(*cp.internal_key.as_bytes());
    for (i, h) in cp.neighbors.iter().enumerate() {
        outer = outer.push_str(String::from(h.to_vec())).push_int(i as u64);
    }
    let outer = outer
        .push_int(cp.neighbors.len() as u64)
        .dict()
        .push_str(String::from(cp.position.clone()))
        .push_script(inner) // ← Script(instrs), witnesses intact
        .push_int(1024u64) // gas
        .push_int(0u64) // k args
        .open()
        .verify() // pops success marker (1); errors if 0
        .drop_(); // pops count

    // Prover round-trip.
    let pc_gens = bulletproofs::PedersenGens::default();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000)
        .expect("prove with witness-bearing open");
    let txid_p = result.txid;
    let TxResult {
        bytecode, proof, ..
    } = result;
    let proof = proof.expect("proof set");

    // Verifier round-trip — same bytecode, no witnesses on the
    // wire, parses the unlock script back to `Alloc(None)` via
    // `String::Opaque(bytes).to_instructions()`.
    let pc_gens_v = bulletproofs::PedersenGens::default();
    let verified = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect("verify ok");
    assert_eq!(verified.txid, txid_p);
}

#[test]
fn open_passes_args_after_payload() {
    // Contract payload: [10]. args: [20, 30]. Inside the isolated ContractOpen
    // frame the stack starts as [10, 20, 30] (payload then args). The
    // leaf drops all three and exits via `return 0`.
    let inner_program = ScriptBuilder::new()
        .drop_()
        .drop_()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let (tree, cp) = build_predicate_with_program(&inner_program, 11);
    let pred_point = tree.point;

    let mut program = ScriptBuilder::new()
        .push_int(10u64) // payload
        .push_int(1u64) // count
        .push_point(*pred_point.as_bytes())
        .contract();
    program = push_taproot_proof_to_program(program, &cp);
    let script = program
        .push_int(1024u64) // gas
        .push_int(20u64) // arg[0]
        .push_int(30u64) // arg[1]
        .push_int(2u64) // k=2
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Parent stack: [count=0, success=1].
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Scalar::from(0u64));
    assert_int(&vm.current_call.stack[1], Scalar::from(1u64));
}

#[test]
fn failed_open_restores_contract_and_explicit_bearers() {
    let inner = ScriptBuilder::new().push_int(0u64).verify().to_bytecode();
    let (tree, proof) = build_predicate_with_program(&inner, 12);
    let payload_token = Token::cleartext(Scalar::from(31u64), FLAME_FLAVOR).unwrap();
    let contract = Contract::new(
        Predicate::opaque(tree.point),
        Anchor([0x31; 32]),
        vec![Value::Token(payload_token.clone())],
    )
    .expect("token payload is portable");
    let contract_id = contract.id();

    let clear = ClearToken::new(Scalar::from(37u64), FLAME_FLAVOR);
    let dict_token = Token::cleartext(Scalar::from(41u64), FLAME_FLAVOR).unwrap();
    let mut dict = Dict::new();
    dict.insert(Scalar::ZERO, Value::Token(dict_token.clone()));
    let expected_dict_root = state_root(&Value::Dict(dict.clone()));

    let mut neighbors = Dict::new();
    for (i, neighbor) in proof.neighbors.iter().enumerate() {
        neighbors.insert(
            Scalar::from(i as u64),
            Value::String(String::from(neighbor.to_vec())),
        );
    }
    let mut vm = vm_with_script(ScriptBuilder::new().open().to_bytecode());
    vm.last_anchor = Some(Anchor([0x42; 32]));
    vm.current_call.stack = vec![
        Value::Contract(Box::new(contract)),
        Value::Point(Point::from_compressed(proof.internal_key)),
        Value::Dict(neighbors),
        Value::String(String::from(proof.position)),
        Value::String(String::from(proof.program)),
        Value::Scalar(Scalar::from(10_000u64)),
        Value::ClearToken(clear.clone()),
        Value::Dict(dict),
        Value::Scalar(Scalar::from(2u64)),
    ];

    run_to_end(&mut vm).unwrap();

    assert_eq!(vm.current_call.stack.len(), 5);
    let Value::Contract(restored_contract) = &vm.current_call.stack[0] else {
        panic!("expected restored Contract");
    };
    assert_eq!(restored_contract.id(), contract_id);
    assert!(matches!(
        restored_contract.payload(),
        [Value::Token(token)] if token.qty() == payload_token.qty() && token.flv() == payload_token.flv()
    ));
    assert!(matches!(
        &vm.current_call.stack[1],
        Value::ClearToken(token) if token.qty() == clear.qty() && token.flv() == clear.flv()
    ));
    assert_eq!(state_root(&vm.current_call.stack[2]), expected_dict_root);
    let Value::Dict(restored_dict) = &vm.current_call.stack[2] else {
        panic!("expected restored Dict");
    };
    assert!(matches!(
        restored_dict.get(&Scalar::ZERO),
        Some(Value::Token(token)) if token.qty() == dict_token.qty() && token.flv() == dict_token.flv()
    ));
    assert_int(&vm.current_call.stack[3], Scalar::from(2u64));
    assert_int(&vm.current_call.stack[4], Scalar::ZERO);
}

#[test]
fn predicate_tree_new_validates_inputs() {
    // Empty programs → EmptyPredicateTree.
    let secret = DalekScalar::from(1u64);
    let ik = (RISTRETTO_BASEPOINT_TABLE * &secret).compress();
    assert!(matches!(
        PredicateTree::new(Some(ik), Vec::new(), TEST_BLINDING_KEY).unwrap_err(),
        VMError::EmptyPredicateTree
    ));
    // Garbage internal_key bytes → InvalidPoint.
    let bad = CompressedRistretto([0xff; 32]); // not a valid Ristretto point
    assert!(matches!(
        PredicateTree::new(Some(bad), vec![vec![0x1d]], TEST_BLINDING_KEY).unwrap_err(),
        VMError::InvalidPoint
    ));
    // None → unspendable internal key (B_blinding). Succeeds and
    // produces a tree whose internal key is the unspendable point.
    let tree = PredicateTree::new(None, vec![vec![0x1d]], TEST_BLINDING_KEY).unwrap();
    assert_eq!(*tree.internal_key(), Predicate::unspendable_key());
    // scripts_only is the documented convenience wrapper for the same
    // pattern; it must produce an identical tree.
    let via_helper = PredicateTree::scripts_only(vec![vec![0x1d]], TEST_BLINDING_KEY).unwrap();
    assert_eq!(via_helper.point, tree.point);
}

#[test]
fn scripts_only_predicate_opens_via_program_path() {
    // End-to-end: build a scripts-only predicate, lock a contract under
    // it, and unlock via `open` with the script-path proof. Leaf
    // `drop, push:0, return` drains the 1-item payload and exits the
    // isolated frame (ADR 0013).
    let program = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let tree = PredicateTree::scripts_only(vec![program.clone()], TEST_BLINDING_KEY).unwrap();
    let cp = tree.taproot_proof_for(0).unwrap();
    let pred_point = tree.point;

    let mut p = ScriptBuilder::new()
        .push_int(5u64) // payload
        .push_int(1u64) // count = 1
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p
        .push_int(1024u64) // gas
        .push_int(0u64) // 0 args
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Parent stack: [count=0, success=1].
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Scalar::from(0u64));
    assert_int(&vm.current_call.stack[1], Scalar::from(1u64));
}

#[test]
fn multi_leaf_predicate_each_program_unlocks_via_its_path() {
    // Three programs; each drops its payload then `push:0, return` —
    // exit the isolated ContractOpen frame with zero results.
    let leaf = |drops: usize| -> Vec<u8> {
        let mut p = ScriptBuilder::new();
        for _ in 0..drops {
            p = p.drop_();
        }
        p.push_int(0u64).return_().to_bytecode()
    };
    let programs: Vec<Vec<u8>> = vec![leaf(1), leaf(2), leaf(3)];
    for i in 0..programs.len() {
        let (tree, cp) = build_multi_leaf_predicate(programs.clone(), i, 11 + i as u64);
        let pred_point = tree.point;

        // payload = i+1 copies of `5` (so program of length i+1
        // can drop them all and end with empty stack)
        let payload_count = i + 1;
        let mut p = ScriptBuilder::new();
        for _ in 0..payload_count {
            p = p.push_int(5u64);
        }
        p = p
            .push_int(payload_count as u64)
            .push_point(*pred_point.as_bytes())
            .contract();
        p = push_taproot_proof_to_program(p, &cp);
        let script = p.push_int(1024u64).push_int(0u64).open().to_bytecode();
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm)
            .unwrap_or_else(|e| panic!("program index {} did not open cleanly: {:?}", i, e));
        // Parent stack: [count=0, success=1].
        assert_eq!(
            vm.current_call.stack.len(),
            2,
            "program index {} expected [count, marker] on stack",
            i
        );
        assert_int(&vm.current_call.stack[0], Scalar::from(0u64));
        assert_int(&vm.current_call.stack[1], Scalar::from(1u64));
    }
}

#[test]
fn multi_leaf_predicate_wrong_leaf_path_hard_fails() {
    // Build a 3-leaf tree. Construct a TaprootProof claiming program[0]
    // but with the path that opens program[1]. Verification must fail.
    let leaf = |drops: usize| -> Vec<u8> {
        let mut p = ScriptBuilder::new();
        for _ in 0..drops {
            p = p.drop_();
        }
        p.push_int(0u64).return_().to_bytecode()
    };
    let programs: Vec<Vec<u8>> = vec![leaf(1), leaf(2), leaf(3)];
    let (tree, valid_cp_for_1) = build_multi_leaf_predicate(programs.clone(), 1, 7);
    // Forge: use program[0]'s bytes but program[1]'s path/neighbors.
    let forged = TaprootProof {
        internal_key: valid_cp_for_1.internal_key,
        neighbors: valid_cp_for_1.neighbors.clone(),
        position: valid_cp_for_1.position.clone(),
        program: programs[0].clone(),
    };
    let pred_point = tree.point;

    let mut p = ScriptBuilder::new()
        .push_int(5u64)
        .push_int(1u64)
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &forged);
    let script = p.push_int(1024u64).push_int(0u64).open().to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TaprootProofMismatch
    ));
}

#[test]
fn taproot_proof_for_out_of_range_index_errors() {
    let secret = DalekScalar::from(1u64);
    let ik = (RISTRETTO_BASEPOINT_TABLE * &secret).compress();
    let tree =
        PredicateTree::new(Some(ik), vec![vec![0x1d], vec![0x1c]], TEST_BLINDING_KEY).unwrap();
    assert!(matches!(
        tree.taproot_proof_for(5).unwrap_err(),
        VMError::ProgramIndexOutOfRange
    ));
}

//
// A contract's payload bytes can only return to the stack via `open`,
// `signtx`, or `signcall` — each of which consumes the source contract
// and records (open) or defers (signtx/signcall) an authorization
// check. There is no "transfer the contract handle into a new output"
// shortcut, because `Value::Contract` is non-portable and the contract
// construction opcodes (`contract`, `output`) reject non-portable
// payload items.

#[test]
fn output_rejects_contract_as_payload_item() {
    // Build contract A on stack. Then try: count=1, predicate_point, output
    //   — the output op pops pred + count + 1 payload item (contract A)
    //     and checked Contract construction must reject contract A.
    let script = ScriptBuilder::new()
        .push_int(5u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract() // contract A → on stack
        .push_int(1u64) // outer payload count = 1
        .push_point([0xbb; 32])
        .output()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn contract_opcode_rejects_contract_as_payload_item() {
    // Symmetric protection on the `contract` construction op.
    let script = ScriptBuilder::new()
        .push_int(5u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract() // contract A
        .push_int(1u64) // outer payload count
        .push_point([0xbb; 32])
        .contract() // attempted outer contract
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn output_rejects_dict_containing_a_contract() {
    // A stack-local Dict may hold a Contract, but its sticky portability flag
    // prevents the whole Dict from crossing an output boundary.
    let script = ScriptBuilder::new()
        .push_int(5u64)
        .push_int(1u64)
        .push_point([0xaa; 32])
        .contract() // contract A on stack
        .push_int(0u64) // key = 0
        .push_int(1u64) // 1 pair
        .dict() // non-portable Dict { 0: contractA }
        .push_int(1u64) // outer count = 1
        .push_point([0xbb; 32])
        .output()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn contract_decode_rejects_empty_input() {
    let err = decode_contract_dropping_ok(&[]).unwrap_err();
    assert!(matches!(err, VMError::MalformedContractEncoding));
}

#[test]
fn contract_decode_rejects_wrong_outer_count() {
    // Outer list-Dict with two entries instead of three (anchor +
    // payload prefix, no predicate). Bytes are hand-rolled to make
    // the outer prefix valid but the inner shape wrong.
    // Outer count = 2 (immediate small list-Dict tag in the encoding).
    // We exploit the fact that any prefix that successfully reads as
    // a list-Dict with count != 3 must fail.
    // Construct a real contract and then patch the outer count.
    let mut bytes = encode_contract_to_bytes(&fixture_contract());
    // First byte encodes the outer list-Dict prefix; just rewrite the
    // top-level prefix byte to a list-Dict of count 2. We use the
    // round-trip helper: build a 2-element list-Dict by hand.
    // Simpler: replace the *whole* string with a list-Dict of count 0,
    // which is canonical but wrong arity.
    bytes.clear();
    write_list_prefix(&mut bytes, 0).expect("write prefix");
    let err = decode_contract_dropping_ok(&bytes).unwrap_err();
    assert!(matches!(err, VMError::MalformedContractEncoding));
}

#[test]
fn contract_decode_rejects_wrong_anchor_length() {
    // Build an outer list-Dict of 3 entries by hand: Point predicate,
    // a String of wrong (31-byte) anchor, then an empty payload list.
    let mut bytes = Vec::new();
    write_list_prefix(&mut bytes, 3).expect("write outer prefix");
    write_value(&mut bytes, &Value::Point(Point::from_bytes([0xaa; 32]))).expect("write point");
    write_value(&mut bytes, &Value::String(String::from(vec![0u8; 31])))
        .expect("write short anchor");
    write_list_prefix(&mut bytes, 0).expect("write payload prefix");
    let err = decode_contract_dropping_ok(&bytes).unwrap_err();
    assert!(matches!(err, VMError::MalformedContractEncoding));
}

#[test]
fn contract_decode_rejects_nested_nonportable_payload_at_contract_boundary() {
    let mut inner = Dict::new();
    inner.insert(
        Scalar::ZERO,
        Value::ClearToken(ClearToken::new(Scalar::from(-1i64), FLAME_FLAVOR)),
    );
    let mut outer = Dict::new();
    outer.insert(Scalar::ZERO, Value::Dict(inner));

    let mut bytes = Vec::new();
    write_list_prefix(&mut bytes, 3).unwrap();
    write_value(&mut bytes, &Value::Point(Point::from_bytes([0xaa; 32]))).unwrap();
    write_value(&mut bytes, &Value::String(String::from(vec![0u8; 32]))).unwrap();
    write_list_prefix(&mut bytes, 1).unwrap();
    write_value(&mut bytes, &Value::Dict(outer)).unwrap();

    assert!(decode_contract_dropping_ok(&bytes).is_err());
}

#[test]
fn contract_decode_rejects_predicate_not_a_point() {
    // First entry is a String where a Point is expected.
    let mut bytes = Vec::new();
    write_list_prefix(&mut bytes, 3).expect("write outer prefix");
    write_value(&mut bytes, &Value::String(String::from(vec![0u8; 32])))
        .expect("write wrong predicate");
    write_value(&mut bytes, &Value::String(String::from(vec![0u8; 32]))).expect("write anchor");
    write_list_prefix(&mut bytes, 0).expect("write payload prefix");
    let err = decode_contract_dropping_ok(&bytes).unwrap_err();
    assert!(matches!(err, VMError::MalformedContractEncoding));
}

#[test]
fn input_pushes_contract_seeds_anchor_and_emits_txlog() {
    let contract = fixture_contract();
    let expected_id = contract.id();
    let bytes = encode_contract_to_bytes(&contract);

    // Build an ExternalRoot VM with the wire bytes on the stack as a String.
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(String::from(bytes)));
    vm.op_input().expect("input succeeds");

    // Top of stack is the decoded Contract.
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Contract(c) => {
            assert_eq!(c.id(), expected_id);
        }
        other => panic!("expected Contract on stack, got {}", value_kind(other)),
    }

    // last_anchor seeded to the input contract's id (split design — no
    // extra ratchet; contract.id() is the unique spend-once source).
    assert_eq!(vm.last_anchor.expect("anchor seeded").0, expected_id);

    // Txlog has Header + one Input entry committing the contract id.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Input(id) => assert_eq!(*id, expected_id),
        _ => panic!("expected TxEntry::Input"),
    }
}

#[test]
fn input_requires_string_on_top() {
    // Non-String top → TypeNotString. (Use an Scalar.)
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::Scalar(Scalar::from(7u64)));
    let err = vm.op_input().unwrap_err();
    assert!(matches!(err, VMError::TypeNotString));
}

#[test]
fn input_rejects_malformed_bytes() {
    // Random non-canonical bytes on the stack.
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(String::from(vec![0xffu8; 8])));
    let err = vm.op_input().unwrap_err();
    assert!(matches!(err, VMError::MalformedContractEncoding));
}

#[test]
fn input_rejects_trailing_bytes_after_contract() {
    // Append a stray byte after a canonical encoding so the inner
    // reader leaves bytes unread → MalformedContractEncoding.
    let contract = fixture_contract();
    let mut bytes = encode_contract_to_bytes(&contract);
    bytes.push(0x00); // trailing garbage

    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(String::from(bytes)));
    let err = vm.op_input().unwrap_err();
    assert!(matches!(err, VMError::MalformedContractEncoding));
}

#[test]
fn input_in_internal_context_errors_external_only() {
    // Drive `0x90` through `step_internal` — dispatch must surface
    // `ExternalOnly` because internal transactions cannot consume
    // Utreexo entries.
    let mut vm = vm_with_script(ScriptBuilder::new().input().to_bytecode());
    // Even with a well-formed string on the stack, internal context
    // rejects the opcode before any decoding happens.
    let contract_bytes = encode_contract_to_bytes(&fixture_contract());
    vm.push_value(Value::String(String::from(contract_bytes)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn input_then_output_anchor_chain() {
    // Round-trip: input a contract, then output a new contract whose anchor
    // is derived from the consumed contract. Confirms `last_anchor` is
    // wired through `input` so a subsequent `output` doesn't need an
    // external seed.
    let contract = fixture_contract();
    let bytes = encode_contract_to_bytes(&contract);
    let expected_anchor_after_input = Anchor(contract.id());

    let mut vm = vm_external_with_script(Vec::new());

    // Step 1: feed contract bytes into op_input.
    vm.push_value(Value::String(String::from(bytes)));
    vm.op_input().expect("input ok");
    // Stack: [Contract]. last_anchor: Some(contract.id()).
    assert_eq!(
        vm.last_anchor.expect("anchor").0,
        expected_anchor_after_input.0
    );

    // The consumed contract's handle is still on the stack. For a stand-alone
    // anchor-chain test we don't care about authorizing it — drop it
    // directly so we can exercise `op_output` against the seeded anchor.
    let _consumed = vm
        .pop_value()
        .expect("pop value")
        .to_contract()
        .expect("pop contract handle");

    // Step 2: build an output through the real op_output handler.
    // Stack pre-output: [payload(5), count(1), predicate(Point)].
    vm.push_value(Value::Scalar(Scalar::from(5u64)));
    vm.push_value(Value::Scalar(Scalar::from(1u64)));
    vm.push_value(Value::Point(Point::from_bytes([0xbb; 32])));
    vm.op_output().expect("output ok");

    // Txlog now has: Header, Input(consumed_id), Output(new_contract).
    assert_eq!(vm.txlog.len(), 3);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Input(_) => {}
        _ => panic!("second entry must be Input"),
    }
    match &vm.txlog[2] {
        TxEntry::Output(_) => {}
        _ => panic!("third entry must be Output"),
    }
    // last_anchor advanced again past the output contract.
    assert_ne!(
        vm.last_anchor.expect("anchor").0,
        expected_anchor_after_input.0
    );
}

#[test]
fn input_via_step_external_dispatch() {
    // Build a one-byte external script `[0x90]` and dispatch a single
    // step through `step_external` to confirm 0x90 routes to op_input.
    // Use a stub delegate that never actually runs (we only step once,
    // and the input opcode does not consult the delegate).
    let contract = fixture_contract();
    let expected_id = contract.id();
    let bytes = encode_contract_to_bytes(&contract);

    let mut vm = vm_external_with_script(ScriptBuilder::new().input().to_bytecode());
    vm.push_value(Value::String(String::from(bytes)));

    let mut delegate = StubDelegate::new();
    let cont = vm.step_external(&mut delegate).expect("step ok");
    assert!(cont, "still running (script not exhausted)");

    // Stack now has the decoded contract; txlog has Header + Input entry.
    match &vm.current_call.stack[0] {
        Value::Contract(c) => assert_eq!(c.id(), expected_id),
        other => panic!("expected Contract, got {}", value_kind(other)),
    }
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Input(id) => assert_eq!(*id, expected_id),
        _ => panic!("expected TxEntry::Input"),
    }
}

#[test]
fn external_tx_one_input_one_output_via_signtx() {
    // A single external transaction consumes one contract (authorized
    // via signtx — the contract holder signs the whole tx via the
    // envelope) and emits a single fresh contract.
    //
    // Contract life-cycle traced end-to-end:
    //   bytes  → input  → contract on stack → signtx (deferred sig +
    //   payload poured) → drop payload → push fresh payload →
    //   output → TxEntry::Output → finalize.

    // 1. Construct the input contract, capture its identity, encode it.
    let input_contract = fixture_contract();
    let input_id = input_contract.id();
    let input_predicate_point = input_contract.predicate.to_point();
    // After op_input, last_anchor == Anchor(input.id()) — split design
    // (no extra ratchet at input). When output consumes via split,
    // the output's stored anchor is the LEFT half.
    let (input_left, _input_right) = Anchor(input_contract.id()).split();
    let input_bytes = encode_contract_to_bytes(&input_contract);

    // 2. Assemble the script.
    //
    // Stack diagram (top of stack on the right):
    //   pushstr <bytes>       []                  → [String]
    //   input                 [String]            → [Contract]
    //   signtx                [Contract]              → [Scalar(7), String, Scalar(2)]
    //                          (payload + count poured; TxBound recorded)
    //   drop                  [..7, "hello", 2]   → [..7, "hello"]
    //   drop                  [..7, "hello"]      → [..7]
    //   drop                  [..7]               → []
    //   push:42               []                  → [Scalar(42)]
    //   push:1                [Scalar(42)]        → [Scalar(42), Scalar(1)]
    //   pushpoint <P_out>     [..1]               → [..1, Point]
    //   output                [..Point]           → []  (Output effect emitted)
    let out_pred_bytes = [0xbb; 32];
    let script = ScriptBuilder::new()
        .push_str(String::from(input_bytes))
        .input()
        .signtx()
        .drop_() // drop count
        .drop_() // drop "hello"
        .drop_() // drop 7
        .push_int(42u64)
        .push_int(1u64) // count = 1
        .push_point(out_pred_bytes)
        .output()
        .to_bytecode();

    // 3. Run through `step_external` to completion + finalize.
    let vm = run_external_workflow(script);

    // 4. Assertions on the final VM state.

    // 4a. Clean exit: stack must be empty.
    assert!(
        vm.current_call.stack.is_empty(),
        "leftover stack at end of tx: {:?}",
        vm.current_call.stack.len()
    );

    // 4b. Txlog: Header(index 0), Input(contract_in_id) at 1,
    //     Output(contract_out) at 2.
    assert_eq!(vm.txlog.len(), 3, "expected Header + Input + Output txlog");
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Input(id) => assert_eq!(*id, input_id),
        _ => panic!("txlog[1] must be Input"),
    }
    let output_contract_anchor = match &vm.txlog[2] {
        TxEntry::Output(c) => {
            // Output payload was [Scalar(42)].
            assert_eq!(c.payload().len(), 1);
            match &c.payload()[0] {
                Value::Scalar(i) => assert_eq!(*i, Scalar::from(42u64)),
                _ => panic!("output payload[0] must be Scalar(42)"),
            }
            // Output predicate is the point we pushed.
            assert_eq!(c.predicate.to_point().as_bytes(), &out_pred_bytes);
            c.anchor
        }
        _ => panic!("txlog[2] must be Output"),
    };

    // 4c. Anchor chain: input seeds last_anchor = Anchor(input.id()),
    //     then output's split gives the LEFT half to the output contract
    //     and keeps RIGHT in last_anchor.
    assert_eq!(output_contract_anchor.0, input_left.0);

    // 4d. Deferred sigs: exactly one TxBound entry, with verification
    //     key matching the input contract's predicate point.
    assert_eq!(vm.deferred_sigs.len(), 1);
    match &vm.deferred_sigs[0] {
        DeferredSig::TxBound {
            verification_key, ..
        } => {
            assert_eq!(
                verification_key.as_bytes(),
                input_predicate_point.as_bytes()
            );
        }
        DeferredSig::Explicit { .. } => {
            panic!("expected TxBound, got Explicit")
        }
    }

    // 4e. last_anchor is the RIGHT half of the split that emitted
    //     the output — distinct from the output's stored anchor
    //     (which is the LEFT half).
    assert!(vm.last_anchor.is_some());
    assert_ne!(vm.last_anchor.unwrap().0, output_contract_anchor.0);
}

#[test]
fn external_tx_two_inputs_two_outputs_via_open() {
    // External tx consumes two distinct contracts via `open` (each
    // unlocked by a valid Taproot TaprootProof against its predicate
    // tree), then emits two fresh output contracts. No `signtx` /
    // `signcall` here, so `deferred_sigs` stays empty.
    //
    // Each input contract's program is `drop` — it consumes the single
    // payload item the contract-open pours onto the stack.

    let prog = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();

    let (tree1, cp1) = build_predicate_with_program(&prog, 11);
    let contract1 = Contract::new(
        Predicate::opaque(tree1.point),
        Anchor([0xa1; 32]),
        vec![Value::Scalar(Scalar::from(11u64))],
    )
    .expect("payload is portable");
    let contract1_id = contract1.id();
    let contract1_bytes = encode_contract_to_bytes(&contract1);

    let (tree2, cp2) = build_predicate_with_program(&prog, 22);
    let contract2 = Contract::new(
        Predicate::opaque(tree2.point),
        Anchor([0xa2; 32]),
        vec![Value::Scalar(Scalar::from(22u64))],
    )
    .expect("payload is portable");
    let contract2_id = contract2.id();
    let contract2_bytes = encode_contract_to_bytes(&contract2);

    //
    //   ┌─── consume contract 1 ─────────────────────────────────┐
    //   │ pushstr <contract1_bytes>                              │
    //   │ input                — pops String → pushes Contract1  │
    //   │ <taproot_proof1 pieces>                                │
    //   │ push:0               — k = 0 args                  │
    //   │ open                 — verifies cp1, pours [11],   │
    //   │                       enters Run over `drop`;      │
    //   │                       inner Run pops the 11        │
    //   └────────────────────────────────────────────────────┘
    //   ┌─── consume contract 2 ─────────────────────────────────┐
    //   │ pushstr <contract2_bytes>                              │
    //   │ input                                              │
    //   │ <taproot_proof2 pieces>                                │
    //   │ push:0                                             │
    //   │ open                                               │
    //   └────────────────────────────────────────────────────┘
    //   ┌─── emit output 1 ──────────────────────────────────┐
    //   │ push:9   push:1   pushpoint <P_out1>   output      │
    //   └────────────────────────────────────────────────────┘
    //   ┌─── emit output 2 ──────────────────────────────────┐
    //   │ push:10  push:1   pushpoint <P_out2>   output      │
    //   └────────────────────────────────────────────────────┘
    let out1_pred_bytes = [0xc1; 32];
    let out2_pred_bytes = [0xc2; 32];
    // Consume contract 1
    let mut p = ScriptBuilder::new()
        .push_str(String::from(contract1_bytes))
        .input();
    p = push_taproot_proof_to_program(p, &cp1);
    p = p.push_int(1024u64).push_int(0u64).open().verify().drop_(); // verify pops success marker; drop discards count

    // Consume contract 2
    p = p.push_str(String::from(contract2_bytes)).input();
    p = push_taproot_proof_to_program(p, &cp2);
    p = p.push_int(1024u64).push_int(0u64).open().verify().drop_();

    // Emit output 1
    p = p
        .push_int(9u64)
        .push_int(1u64)
        .push_point(out1_pred_bytes)
        .output();
    // Emit output 2
    p = p
        .push_int(10u64)
        .push_int(1u64)
        .push_point(out2_pred_bytes)
        .output();

    let script = p.to_bytecode();

    let vm = run_external_workflow(script);

    // Clean stack.
    assert!(vm.current_call.stack.is_empty());

    // Txlog: Header, 2 × Input, 2 × Output, in that order.
    assert_eq!(vm.txlog.len(), 5, "expected Header + 2 inputs + 2 outputs");
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    match &vm.txlog[1] {
        TxEntry::Input(id) => assert_eq!(*id, contract1_id),
        _ => panic!("txlog[1] must be Input(contract1)"),
    }
    match &vm.txlog[2] {
        TxEntry::Input(id) => assert_eq!(*id, contract2_id),
        _ => panic!("txlog[2] must be Input(contract2)"),
    }
    let (out1, out2) = match (&vm.txlog[3], &vm.txlog[4]) {
        (TxEntry::Output(o1), TxEntry::Output(o2)) => (o1, o2),
        _ => panic!("txlog[3..5] must be Output entries"),
    };

    // Output 1's payload is [Scalar(9)], predicate matches what we
    // pushed.
    assert_eq!(out1.payload().len(), 1);
    match &out1.payload()[0] {
        Value::Scalar(i) => assert_eq!(*i, Scalar::from(9u64)),
        _ => panic!("out1.payload[0] must be Scalar(9)"),
    }
    assert_eq!(out1.predicate.to_point().as_bytes(), &out1_pred_bytes);
    assert_eq!(out2.payload().len(), 1);
    match &out2.payload()[0] {
        Value::Scalar(i) => assert_eq!(*i, Scalar::from(10u64)),
        _ => panic!("out2.payload[0] must be Scalar(10)"),
    }
    assert_eq!(out2.predicate.to_point().as_bytes(), &out2_pred_bytes);

    // Anchor chain (split-at-each-call design):
    //   - input₁: last_anchor = Anchor(contract1.id())
    //   - open₁: split(Anchor(contract1.id())) → (left to child, right to
    //     parent's post_call_anchor). After child returns:
    //     last_anchor = right.
    //   - input₂: replaces last_anchor = Anchor(contract2.id())
    //   - open₂: split(Anchor(contract2.id())) → (left to child, right to
    //     parent's post_call_anchor). After return:
    //     last_anchor = r2 = Anchor(contract2.id()).split().1.
    //   - output₁: split(r2) → (out1.anchor = left, last_anchor = right).
    //   - output₂: split that → (out2.anchor = left, last_anchor = right).
    let (_, r2) = Anchor(contract2_id).split();
    let (out1_expected, after_out1) = r2.split();
    let (out2_expected, after_out2) = after_out1.split();
    assert_eq!(out1.anchor.0, out1_expected.0);
    assert_eq!(out2.anchor.0, out2_expected.0);
    assert_ne!(out1.anchor.0, out2.anchor.0);
    let final_anchor = vm.last_anchor.expect("anchor set after output 2");
    assert_eq!(final_anchor.0, after_out2.0);

    // No `signtx` / `signcall` were used → no deferred sigs.
    assert!(
        vm.deferred_sigs.is_empty(),
        "open does not record deferred sigs"
    );
}

// ── ADR 0013 isolation invariants ──────────────────────────────────

/// `op_open` creates an isolated CallFrame with no actor identity;
/// `op_selfid` inside the leaf errors `OpcodeRequiresActorContext`,
/// which the step wrapper catches. The parent recovers the locked Contract
/// followed by its explicit argument, `count=1, success=0`; the contextual
/// Contract is not counted.
#[test]
fn op_open_selfid_errors_no_actor_context() {
    // `selfid` errors before reaching a return — that's fine, the
    // child-frame error is caught and converted to a marker.
    let inner = ScriptBuilder::new().selfid().to_bytecode();
    let (tree, cp) = build_predicate_with_program(&inner, 5);
    let pred_point = tree.point;
    let mut p = ScriptBuilder::new()
        .push_int(5u64) // payload
        .push_int(1u64) // count
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p
        .push_int(1024u64)
        .push_int(9u64)
        .push_int(1u64)
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 4);
    assert!(matches!(vm.current_call.stack[0], Value::Contract(_)));
    assert_int(&vm.current_call.stack[1], Scalar::from(9u64));
    assert_int(&vm.current_call.stack[2], Scalar::ONE);
    assert_int(&vm.current_call.stack[3], Scalar::ZERO);
}

/// `op_open` leaf returning the wrong arity errors `BadReturnArity`;
/// caught by step while restoring the locked Contract to the parent.
#[test]
fn op_open_return_arity_mismatch_errors() {
    // push:1, return — pops k=1 from stack, then stack.len()(0) < 1.
    let inner = ScriptBuilder::new().push_int(1u64).return_().to_bytecode();
    let (tree, cp) = build_predicate_with_program(&inner, 0);
    let pred_point = tree.point;
    let mut p = ScriptBuilder::new()
        .push_int(0u64) // payload count = 0
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p.push_int(1024u64).push_int(0u64).open().to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert!(matches!(vm.current_call.stack[0], Value::Contract(_)));
    assert_int(&vm.current_call.stack[1], Scalar::ZERO);
    assert_int(&vm.current_call.stack[2], Scalar::ZERO);
}

#[test]
fn open_rejects_negative_token_argument() {
    let inner = ScriptBuilder::new().push_int(0u64).verify().to_bytecode();
    let (tree, cp) = build_predicate_with_program(&inner, 0);
    let mut p = ScriptBuilder::new()
        .push_int(0u64)
        .push_point(*tree.point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p
        .push_int(1024u64)
        .push_int(7u64)
        .push_int(FLAME_FLAVOR)
        .borrow()
        .retire()
        .push_int(1u64)
        .open()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm),
        Err(VMError::NonPortableInCall)
    ));
}

/// CS context propagates: a ContractOpen frame with `external_context:
/// false` (as if opened from inside an actor method) must reject
/// `alloc` with `ExternalOnly`. Constructs the frame directly since
/// the only path that produces `external_context: false` is opening
/// from an internal-context parent.
#[test]
fn op_open_cs_blocked_when_external_context_false() {
    use crate::vm::{Anchor, CallFrame, CallKind, VM};
    let parent = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500);
    // alloc is 0x62 — external-only CS op. push:0 + alloc + return.
    let child_kind = CallKind::ContractOpen {
        predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
        external_context: false,
        caller_id: None,
    };
    let child = CallFrame::new(vec![Instruction::Alloc(None)], child_kind, 500);
    let mut vm = VM::new(dummy_header(), parent);
    let parent_saved = std::mem::replace(&mut vm.current_call, child);
    vm.call_stack.push(parent_saved);
    // alloc errors ExternalOnly inside the child; step catches and
    // unwinds, leaving `[count=0, success=0]` on the parent's stack.
    vm.step_internal()
        .expect("step ok — error swallowed into marker");
    assert!(vm.call_stack.is_empty());
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Scalar::ZERO);
    assert_int(&vm.current_call.stack[1], Scalar::ZERO);
}
