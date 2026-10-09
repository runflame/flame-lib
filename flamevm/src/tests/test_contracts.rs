//! Tests for contracts.

#![allow(unused_imports)]

use super::test_helpers::*;
use crate::state_root;

// A hostile Dict count must not cause count-sized allocation.
#[test]
fn contract_decode_rejects_payload_count_bomb() {
    let mut dict = CellBuilder::new();
    dict.store_u64(u64::MAX)
        .unwrap()
        .store_u8(3)
        .unwrap()
        .store_ref(CellRef::resident(Cell::new(vec![], vec![]).unwrap()))
        .unwrap();
    let mut contract = CellBuilder::new();
    contract
        .store_bytes(&[2; 32])
        .unwrap()
        .store_bytes(&[0; 32])
        .unwrap()
        .store_u8(2)
        .unwrap()
        .store_ref(CellRef::resident(dict.build()))
        .unwrap();
    assert!(Contract::from_cell(&contract.build(), &mut ()).is_err());
}

#[test]
fn contract_opcode_requires_seeded_anchor() {
    // push:7 (payload), pushpoint(some), contract —
    // run from an ExternalRoot frame whose last_anchor is `None`
    // (no prior `op_input` to seed it). `op_contract` must hard-fail.
    let script = ScriptBuilder::new()
        .push_int(7u64)
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
            Value::Dict(outer),
        ),
        Err(VMError::NonPortableInOutput)
    ));
}

#[test]
fn contract_is_noncopyable_and_nondroppable() {
    // build contract, then dup → TypeNotCopyable
    let script = ScriptBuilder::new()
        .push_int(7u64)
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
        .push_point(*pred_point.as_bytes())
        .contract();
    program = push_taproot_proof_to_program(program, &cp);
    let script = program
        .push_int(1024u64) // gas
        .push_int(0u64) // k = 0 args
        .open();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Parent stack: [count=0, success=1].
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[0], Scalar::from(0u64));
    assert_int(&vm.current_call.stack[1], Scalar::from(1u64));
}

#[test]
fn taproot_program_larger_than_runtime_string_opens_through_cells() {
    let mut branch = ScriptBuilder::new().drop_();
    for _ in 0..=String::MAX_LEN {
        branch = branch.nop();
    }
    let branch = branch.push_int(0u64).return_().to_bytecode();
    assert!(branch.len() > String::MAX_LEN);

    // The program is a Snake-backed predicate leaf, never a runtime String
    // literal. Only its short root/index selector appears in the outer code.
    let tree = PredicateTree::scripts_only(vec![branch], TEST_BLINDING_KEY).unwrap();
    let proof = test_taproot_proof(&tree, 0).unwrap();
    let contract = Contract::new(
        Predicate::opaque(tree.point),
        Anchor([0xb7; 32]),
        Value::Scalar(Scalar::ONE),
    )
    .unwrap();
    let outer = ScriptBuilder::new()
        .push_str(String::contract(contract))
        .input();
    let outer = push_taproot_proof_to_program(outer, &proof)
        .push_int(100_000u64)
        .push_int(0u64)
        .open()
        .verify()
        .drop_();

    let gens = PedersenGens::default();
    let result = Prover::prove(&gens, outer, dummy_header(), 1_000_000).unwrap();
    let verified = Verifier::verify_with_cells(
        &gens,
        result.bytecode.clone(),
        result.proof.as_ref().unwrap(),
        dummy_header(),
        1_000_000,
        None,
        &result.cells,
    )
    .unwrap();
    assert_eq!(verified.txid, result.txid);
    assert_eq!(verified.gas_used, result.gas_used);
}

#[test]
fn open_with_wrong_program_hard_fails() {
    let real = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let fake = ScriptBuilder::new()
        .nop()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let (tree, _) = build_predicate_with_program(&real, 7);
    let (_, fake_proof) = build_predicate_with_program(&fake, 7);
    let program = ScriptBuilder::new()
        .push_int(5u64)
        .push_point(*tree.point.as_bytes())
        .contract();
    let script = push_taproot_proof_to_program(program, &fake_proof)
        .push_int(10_000u64)
        .push_int(0u64)
        .open();
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm),
        Err(VMError::TaprootProofMismatch)
    ));
}

/// Private allocation assignments survive only behind authenticated script bytes.
#[test]
fn open_preserves_alloc_witnesses_from_predicate_builder() {
    let inner = ScriptBuilder::new()
        .drop_()
        .alloc(Some(Scalar::from(7u64)))
        .alloc(Some(Scalar::from(3u64)))
        .add()
        .alloc(Some(Scalar::from(10u64)))
        .eq()
        .verify()
        .push_int(0u64)
        .return_();
    let tree = PredicateTree::from_scripts(None, vec![inner], TEST_BLINDING_KEY).unwrap();
    let contract = Contract::new(
        Predicate::opaque(tree.point),
        Anchor([0xa1; 32]),
        Value::Dict(Dict::new()),
    )
    .unwrap();
    let outer = ScriptBuilder::new()
        .push_str(String::contract(contract))
        .input()
        .push_taproot_proof(&tree, 0)
        .unwrap()
        .push_int(100_000u64)
        .push_int(0u64)
        .open()
        .verify()
        .drop_();
    let gens = bulletproofs::PedersenGens::default();
    let result = Prover::prove(&gens, outer, dummy_header(), 1_000_000).unwrap();
    let verified = Verifier::verify_with_cells(
        &gens,
        result.bytecode.clone(),
        result.proof.as_ref().unwrap(),
        dummy_header(),
        1_000_000,
        None,
        &result.cells,
    )
    .unwrap();
    assert_eq!(verified.txid, result.txid);
    assert_eq!(verified.gas_used, result.gas_used);
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
        .push_point(*pred_point.as_bytes())
        .contract();
    program = push_taproot_proof_to_program(program, &cp);
    let script = program
        .push_int(1024u64) // gas
        .push_int(20u64) // arg[0]
        .push_int(30u64) // arg[1]
        .push_int(2u64) // k=2
        .open();
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
        Value::Token(payload_token.clone()),
    )
    .expect("token payload is portable");
    let contract_id = contract.id();

    let clear = ClearToken::new(Scalar::from(37u64), FLAME_FLAVOR);
    let dict_token = Token::cleartext(Scalar::from(41u64), FLAME_FLAVOR).unwrap();
    let mut dict = Dict::new();
    dict.insert(Scalar::ZERO, Value::Token(dict_token.clone()));
    let expected_dict_root = state_root(&Value::Dict(dict.clone()));

    let mut vm = vm_with_script(ScriptBuilder::new().with_cells(proof.cells.clone()).open());
    vm.last_anchor = Some(Anchor([0x42; 32]));
    vm.current_call.stack = vec![
        Value::Contract(Box::new(contract)),
        Value::Point(Point::from_compressed(proof.internal_key)),
        Value::String(String::from(proof.root.to_vec())),
        Value::Scalar(Scalar::from(proof.index)),
        Value::Scalar(Scalar::from(10_000u64)),
        Value::ClearToken(clear),
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
        Value::Token(token) if token.qty() == payload_token.qty() && token.flv() == payload_token.flv()
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
    let cp = test_taproot_proof(&tree, 0).unwrap();
    let pred_point = tree.point;

    let mut p = ScriptBuilder::new()
        .push_int(5u64) // payload
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p
        .push_int(1024u64) // gas
        .push_int(0u64) // 0 args
        .open();
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
    let programs = (0..3)
        .map(|n| {
            let mut p = ScriptBuilder::new().drop_();
            for _ in 0..n {
                p = p.nop();
            }
            p.push_int(0u64).return_().to_bytecode()
        })
        .collect::<Vec<_>>();
    for index in 0..programs.len() {
        let (tree, proof) = build_multi_leaf_predicate(programs.clone(), index, 11 + index as u64);
        let p = ScriptBuilder::new()
            .push_int(5u64)
            .push_point(*tree.point.as_bytes())
            .contract();
        let p = push_taproot_proof_to_program(p, &proof)
            .push_int(10_000u64)
            .push_int(0u64)
            .open();
        let mut vm = vm_with_script(p);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[0], Scalar::ZERO);
        assert_int(&vm.current_call.stack[1], Scalar::ONE);
    }
}

#[test]
fn multi_leaf_predicate_blinding_leaf_cannot_be_opened() {
    let program = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let (tree, mut proof) = build_multi_leaf_predicate(vec![program.clone(), program], 0, 7);
    proof.proof.index ^= 1; // Each program is paired with one non-program blinding leaf.
    proof.cells = CellIndex::collect(Arc::new(tree.root().as_resident().unwrap().clone())).unwrap();
    let p = ScriptBuilder::new()
        .push_int(5u64)
        .push_point(*tree.point.as_bytes())
        .contract();
    let p = push_taproot_proof_to_program(p, &proof)
        .push_int(10_000u64)
        .push_int(0u64)
        .open();
    let mut vm = vm_with_script(p);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm),
        Err(VMError::TaprootProofMismatch)
    ));
}

#[test]
fn taproot_proof_for_out_of_range_index_errors() {
    let secret = DalekScalar::from(1u64);
    let ik = (RISTRETTO_BASEPOINT_TABLE * &secret).compress();
    let tree =
        PredicateTree::new(Some(ik), vec![vec![0x1d], vec![0x1c]], TEST_BLINDING_KEY).unwrap();
    assert!(matches!(
        test_taproot_proof(&tree, 5).unwrap_err(),
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
    // Build contract A, then attempt to put its handle in a new output payload.
    let script = ScriptBuilder::new()
        .push_int(5u64)
        .push_point([0xaa; 32])
        .contract() // contract A → on stack
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
        .push_point([0xaa; 32])
        .contract() // contract A
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
        .push_point([0xaa; 32])
        .contract() // contract A on stack
        .push_int(0u64) // key = 0
        .push_int(1u64) // 1 pair
        .dict() // non-portable Dict { 0: contractA }
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
fn contract_decode_rejects_missing_header() {
    let cell = Cell::new(vec![], vec![]).unwrap();
    assert!(Contract::from_cell(&cell, &mut ()).is_err());
}

#[test]
fn contract_decode_rejects_short_anchor() {
    let cell = Cell::new(vec![0; 63], vec![]).unwrap();
    assert!(Contract::from_cell(&cell, &mut ()).is_err());
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
    let mut builder = CellBuilder::new();
    builder
        .store_bytes(&[0xaa; 32])
        .unwrap()
        .store_bytes(&[0; 32])
        .unwrap()
        .store(&Value::Dict(outer))
        .unwrap();
    assert!(Contract::from_cell(&builder.build(), &mut ()).is_err());
}

#[test]
fn contract_decode_rejects_unknown_payload_tag() {
    let mut builder = CellBuilder::new();
    builder
        .store_bytes(&[0xaa; 32])
        .unwrap()
        .store_bytes(&[0; 32])
        .unwrap()
        .store_u8(255)
        .unwrap();
    assert!(Contract::from_cell(&builder.build(), &mut ()).is_err());
}

#[test]
fn input_pushes_contract_seeds_anchor_and_emits_txlog() {
    let contract = fixture_contract();
    let expected_id = contract.id();
    let mut vm = vm_external_with_script(
        ScriptBuilder::new()
            .push_str(String::contract(contract))
            .input(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 1);
    assert!(matches!(&vm.current_call.stack[0], Value::Contract(c) if c.id() == expected_id));
    assert_eq!(vm.last_anchor.unwrap().0, expected_id);
    assert_eq!(vm.txlog.len(), 3);
    assert!(matches!(vm.txlog[0], TxEntry::Header(_)));
    assert!(matches!(vm.txlog[1], TxEntry::CellWitness(_)));
    assert!(matches!(vm.txlog[2], TxEntry::Input(id) if id == expected_id));
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
fn input_rejects_trailing_bytes_after_contract_id() {
    let mut bytes = fixture_contract().id().to_vec();
    bytes.push(0);
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(String::from(bytes)));
    assert!(matches!(
        vm.op_input(),
        Err(VMError::MalformedContractEncoding)
    ));
}

#[test]
fn input_in_internal_context_errors_external_only() {
    // Drive `0x90` through `step_internal` — dispatch must surface
    // `ExternalOnly` because internal transactions cannot consume
    // Utreexo entries.
    let mut vm = vm_with_script(ScriptBuilder::new().input().to_bytecode());
    // Even with a well-formed string on the stack, internal context
    // rejects the opcode before any decoding happens.
    let contract_bytes = fixture_contract().id().to_vec();
    vm.push_value(Value::String(String::from(contract_bytes)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn input_then_output_anchor_chain() {
    let contract = fixture_contract();
    let expected = Anchor(contract.id());
    let mut vm = vm_external_with_script(
        ScriptBuilder::new()
            .push_str(String::contract(contract))
            .input(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.last_anchor.unwrap().0, expected.0);
    let _consumed = vm.pop_value().unwrap().to_contract().unwrap();
    vm.push_value(Value::Scalar(Scalar::from(5u64)));
    vm.push_value(Value::Point(Point::from_bytes([0xbb; 32])));
    vm.op_output().unwrap();
    assert!(vm.current_call.stack.is_empty());
    assert_eq!(vm.txlog.len(), 4);
    assert!(matches!(vm.txlog[2], TxEntry::Input(_)));
    assert!(matches!(&vm.txlog[3], TxEntry::Output(c) if c.anchor == expected.split().0));
    assert_eq!(vm.last_anchor.unwrap(), expected.split().1);
}

#[test]
fn input_via_step_external_dispatch() {
    let contract = fixture_contract();
    let expected = contract.id();
    let mut vm = vm_external_with_script(
        ScriptBuilder::new()
            .with_cells(CellIndex::collect(Arc::new(contract.to_cell().unwrap())).unwrap())
            .input(),
    );
    vm.push_value(Value::String(String::from(expected.to_vec())));
    assert!(vm.step_external(&mut StubDelegate::new()).unwrap());
    assert!(matches!(&vm.current_call.stack[0], Value::Contract(c) if c.id() == expected));
    assert_eq!(vm.txlog.len(), 3);
    assert!(matches!(vm.txlog[2], TxEntry::Input(id) if id == expected));
}

#[test]
fn external_tx_one_input_one_output_via_signtx() {
    let input = fixture_contract();
    let input_id = input.id();
    let predicate = input.predicate.to_point();
    let (left, right) = Anchor(input_id).split();
    let program = ScriptBuilder::new()
        .push_str(String::contract(input))
        .input()
        .signtx()
        .drop_()
        .push_int(42u64)
        .push_point([0xbb; 32])
        .output();
    let vm = run_external_workflow(program);
    assert!(vm.current_call.stack.is_empty());
    assert_eq!(vm.txlog.len(), 4);
    assert!(matches!(vm.txlog[1], TxEntry::CellWitness(_)));
    assert!(matches!(vm.txlog[2], TxEntry::Input(id) if id == input_id));
    let TxEntry::Output(output) = &vm.txlog[3] else {
        panic!("expected output")
    };
    assert_int(output.payload(), Scalar::from(42u64));
    assert_eq!(output.predicate.to_point().as_bytes(), &[0xbb; 32]);
    assert_eq!(output.anchor, left);
    assert_eq!(vm.last_anchor.unwrap(), right);
    assert_eq!(vm.deferred_sigs.len(), 1);
    assert!(
        matches!(&vm.deferred_sigs[0], DeferredSig::TxBound { verification_key, .. }
        if verification_key.as_bytes() == predicate.as_bytes())
    );
}

#[test]
fn external_tx_two_inputs_two_outputs_via_open() {
    let program = ScriptBuilder::new()
        .drop_()
        .push_int(0u64)
        .return_()
        .to_bytecode();
    let (tree1, proof1) = build_predicate_with_program(&program, 11);
    let (tree2, proof2) = build_predicate_with_program(&program, 22);
    let contract1 = Contract::new(
        Predicate::opaque(tree1.point),
        Anchor([0xa1; 32]),
        Value::Scalar(Scalar::from(11u64)),
    )
    .unwrap();
    let contract2 = Contract::new(
        Predicate::opaque(tree2.point),
        Anchor([0xa2; 32]),
        Value::Scalar(Scalar::from(22u64)),
    )
    .unwrap();
    let id1 = contract1.id();
    let id2 = contract2.id();
    let p = ScriptBuilder::new()
        .push_str(String::contract(contract1))
        .input();
    let p = push_taproot_proof_to_program(p, &proof1)
        .push_int(10_000u64)
        .push_int(0u64)
        .open()
        .verify()
        .drop_()
        .push_str(String::contract(contract2))
        .input();
    let p = push_taproot_proof_to_program(p, &proof2)
        .push_int(10_000u64)
        .push_int(0u64)
        .open()
        .verify()
        .drop_()
        .push_int(9u64)
        .push_point([0xc1; 32])
        .output()
        .push_int(10u64)
        .push_point([0xc2; 32])
        .output();
    let vm = run_external_workflow(p);
    assert!(vm.current_call.stack.is_empty());
    assert_eq!(vm.txlog.len(), 6);
    assert!(matches!(vm.txlog[2], TxEntry::Input(id) if id == id1));
    assert!(matches!(vm.txlog[3], TxEntry::Input(id) if id == id2));
    let (TxEntry::Output(out1), TxEntry::Output(out2)) = (&vm.txlog[4], &vm.txlog[5]) else {
        panic!("expected outputs")
    };
    assert_int(out1.payload(), Scalar::from(9u64));
    assert_int(out2.payload(), Scalar::from(10u64));
    assert_eq!(out1.predicate.to_point().as_bytes(), &[0xc1; 32]);
    assert_eq!(out2.predicate.to_point().as_bytes(), &[0xc2; 32]);
    let (_, after_open) = Anchor(id2).split();
    let (first, after_first) = after_open.split();
    let (second, after_second) = after_first.split();
    assert_eq!(out1.anchor, first);
    assert_eq!(out2.anchor, second);
    assert_eq!(vm.last_anchor.unwrap(), after_second);
    assert!(vm.deferred_sigs.is_empty());
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
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p.push_int(1024u64).push_int(9u64).push_int(1u64).open();
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
    let inner = ScriptBuilder::new()
        .drop_()
        .push_int(1u64)
        .return_()
        .to_bytecode();
    let (tree, cp) = build_predicate_with_program(&inner, 0);
    let pred_point = tree.point;
    let mut p = ScriptBuilder::new()
        .push_int(0u64)
        .dict() // payload count = 0
        .push_point(*pred_point.as_bytes())
        .contract();
    p = push_taproot_proof_to_program(p, &cp);
    let script = p.push_int(1024u64).push_int(0u64).open();
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
        .dict()
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
        .open();
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
