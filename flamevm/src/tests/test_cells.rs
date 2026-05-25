//! Tests for cells.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn anchor_ratchet_changes_value_and_is_deterministic() {
    let a = Anchor([0xaa; 32]);
    let b = a.ratchet();
    assert_ne!(a.0, b.0);
    // deterministic: same input → same output
    let b2 = a.ratchet();
    assert_eq!(b.0, b2.0);
    // ratcheting again diverges further
    let c = b.ratchet();
    assert_ne!(b.0, c.0);
}

#[test]
fn cell_opcode_requires_seeded_anchor() {
    // push:7 (payload), push:1 (count), pushpoint(some), cell
    let mut script = vec![0x07, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91);
    let mut vm = vm_with_script(script);
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::AnchorMissing));
}

#[test]
fn cell_opcode_builds_a_cell_and_ratchets_anchor() {
    // Seed an anchor, then build a cell.
    let mut script = vec![0x07, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91);
    let mut vm = vm_with_script(script);
    let seed = Anchor([0x42; 32]);
    vm.last_anchor = Some(seed);
    run_to_end(&mut vm).unwrap();
    // Stack should have a single Cell.
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Cell(c) => {
            // The cell took the seed as its anchor.
            assert_eq!(c.anchor.0, seed.0);
            // last_anchor advanced.
            let next = vm.last_anchor.unwrap();
            assert_ne!(next.0, seed.0);
        }
        other => panic!("expected Cell, got {}", value_kind(other)),
    }
}

#[test]
fn cell_opcode_rejects_non_portable_payload() {
    // Build a ClearToken (linear, can be non-portable if qty < 0;
    // even zero-qty cleartoken is non-portable only if qty < 0 — but
    // ClearToken is still considered non-portable when we push it
    // because is_portable returns true for zero-qty.
    // Instead test with a `Merlin` (always non-portable).
    let mut script = Vec::new();
    push_string_bytes(&mut script, b""); // empty label
    script.push(0x69); // merlin → pushes Merlin (non-portable)
    script.push(0x01); // push:1 (count)
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn cell_is_noncopyable_and_nondroppable() {
    // build cell, then dup → TypeNotCopyable
    let mut script = vec![0x07, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91);
    script.push(0x20); // dup:0
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));

    // build cell, then drop → TypeNotDroppable
    let mut script = vec![0x07, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91);
    script.push(0x1c); // drop
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotDroppable
    ));
}

#[test]
fn output_opcode_emits_to_txlog_without_pushing() {
    let mut script = vec![0x07, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x92); // output
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    // Stack is empty.
    assert!(vm.current_call.stack.is_empty());
    // Txlog has Header + one Output entry. The Header is always
    // emitted at VM::new so TxID::from_log binds to
    // version + locktime alongside the effects.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Output(_) => {}
        _ => panic!("expected Output entry"),
    }
}

#[test]
fn open_with_valid_callproof_runs_program() {
    // Cell payload: 5. Program: drop. After open: payload poured to
    // stack, program drops it → empty stack.
    let inner_program = vec![0x1c]; // drop
    let (tree, cp) = build_predicate_with_program(&inner_program, 7);
    let pred_point = tree.compute_point();

    // Script: push payload(5), push count(1), pushpoint(pred), cell,
    //         push callproof pieces (internal_key, neighbors, pos, prog),
    //         push k=0 (no args), open.
    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, pred_point.as_bytes());
    script.push(0x91); // cell
    push_callproof_pieces(&mut script, &cp);
    script.push(0x00); // k=0 args
    script.push(0x93); // open
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn open_with_wrong_program_hard_fails() {
    // Predicate commits to `drop`; callproof claims `nop` instead.
    let real_program = vec![0x1c];
    let fake_program = vec![0x1d];
    let (tree, _real_cp) = build_predicate_with_program(&real_program, 7);
    let cp = CallProof {
        internal_key: tree.internal_key,
        neighbors: Vec::new(),
        position: Vec::new(),
        program: fake_program,
    };
    let pred_point = tree.compute_point();

    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, pred_point.as_bytes());
    script.push(0x91);
    push_callproof_pieces(&mut script, &cp);
    script.push(0x00);
    script.push(0x93);
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::CallProofMismatch
    ));
}

/// Prover-side: the unlock script pushed as the callproof's
/// `program` component can be a `String::Script(instrs)` carrying
/// witnesses. `op_open` verifies the callproof against the cell's
/// predicate (the bytes must match the leaf stored in the
/// predicate tree), then uses `program_str.to_instructions()` so
/// the witness slots survive into the new Run. End-to-end via
/// Prover::prove → Verifier::verify.
///
/// Regression guard for the witness-erasing path that existed
/// before `op_open` switched from re-parsing `verify_callproof`'s
/// returned bytes to using the stack `program_str` directly.
#[test]
fn open_preserves_alloc_witnesses_via_script_string() {
    // Inner unlock script with `alloc(Some(_))` witnesses.
    let inner = Program::new()
        .alloc(Some(Int253::from(7u64)))
        .alloc(Some(Int253::from(3u64)))
        .add()
        .alloc(Some(Int253::from(10u64)))
        .eq()
        .verify();
    let inner_bytes = inner.to_bytecode();

    // Single-leaf predicate tree whose leaf == inner_bytes. The
    // NUMS-unspendable internal key means the only spend path is
    // the script leaf.
    let tree = PredicateTree::scripts_only(
        vec![inner_bytes.clone()],
        TEST_BLINDING_KEY,
    )
    .expect("scripts_only tree");
    let cp = tree.callproof_for(0).expect("callproof for leaf 0");
    let pred_point = tree.compute_point();

    // Construct the input cell with an empty payload — the witness
    // we care about lives in the unlock script, not the payload.
    let cell = Cell::new(
        Predicate::Opaque(pred_point),
        Anchor([0xa1; 32]),
        vec![],
    );
    let cell_bytes = encode_cell_to_bytes(&cell);

    // Outer Program:
    //   pushstr <cell_bytes>; input;
    //   pushpoint <internal_key>;
    //   for each neighbor: pushstr <h>; push:i;     // N neighbors
    //   push:N; dict;
    //   pushstr <position>;
    //   push_script(inner);                          // witness-bearing
    //   push:0; open
    let mut outer = Program::new()
        .push_str(String::from(cell_bytes))
        .input()
        .push_point(*cp.internal_key.as_bytes());
    for (i, h) in cp.neighbors.iter().enumerate() {
        outer = outer
            .push_str(String::from(h.to_vec()))
            .push_int(i as u64);
    }
    let outer = outer
        .push_int(cp.neighbors.len() as u64)
        .dict()
        .push_str(String::from(cp.position.clone()))
        .push_script(inner) // ← Script(instrs), witnesses intact
        .push_int(0u64)
        .open();

    // Prover round-trip.
    let pc_gens = bulletproofs::PedersenGens::default();
    let result = Prover::prove(&pc_gens, outer, dummy_header(), 1_000_000, 0)
        .expect("prove with witness-bearing open");
    let txid_p = result.txid;
    let TxResult { bytecode, proof, .. } = result;
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
        0,
        None,
    )
    .expect("verify ok");
    assert_eq!(verified.txid, txid_p);
}

#[test]
fn open_passes_args_after_payload() {
    // Cell payload: [10]. args: [20, 30]. Program: stack must end with
    // exactly the args + payload arrangement; cleanup leaves stack
    // empty. Inside the cell-run, stack = [10, 20, 30]. Program: drop
    // three items.
    let inner_program = vec![0x1c, 0x1c, 0x1c]; // drop, drop, drop
    let (tree, cp) = build_predicate_with_program(&inner_program, 11);
    let pred_point = tree.compute_point();

    let mut script = vec![0x0a, 0x01]; // payload=10, count=1
    push_point_bytes(&mut script, pred_point.as_bytes());
    script.push(0x91);                  // cell
    push_callproof_pieces(&mut script, &cp);
    // push args 20, 30 (deepest first) and k=2
    script.push(0x14);                  // pushint64 positive
    script.extend_from_slice(&20u64.to_le_bytes());
    script.push(0x14);
    script.extend_from_slice(&30u64.to_le_bytes());
    script.push(0x02);                  // k=2
    script.push(0x93);                  // open
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn predicate_tree_new_validates_inputs() {
    // Empty programs → EmptyPredicateTree.
    let secret = Scalar::from(1u64);
    let ik = (&secret * &RISTRETTO_BASEPOINT_TABLE).compress();
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
    let via_helper = PredicateTree::scripts_only(
        vec![vec![0x1d]],
        TEST_BLINDING_KEY,
    )
    .unwrap();
    assert_eq!(via_helper.compute_point(), tree.compute_point());
}

#[test]
fn scripts_only_predicate_opens_via_program_path() {
    // End-to-end: build a scripts-only predicate, lock a cell under
    // it, and unlock via `open` with the script-path proof. Verifies
    // the unspendable-internal-key construction is wire-compatible
    // with the existing `open` opcode.
    let program = vec![0x1c]; // drop (cell payload is one item)
    let tree = PredicateTree::scripts_only(
        vec![program.clone()],
        TEST_BLINDING_KEY,
    )
    .unwrap();
    let cp = tree.callproof_for(0).unwrap();
    let pred_point = tree.compute_point();

    let mut script = vec![0x05, 0x01]; // payload: push:5; k=1
    push_point_bytes(&mut script, pred_point.as_bytes());
    script.push(0x91); // cell
    push_callproof_pieces(&mut script, &cp);
    script.push(0x00); // 0 args
    script.push(0x93); // open
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn multi_leaf_predicate_each_program_unlocks_via_its_path() {
    // Three programs; build a CallProof for each and confirm open succeeds.
    let programs: Vec<Vec<u8>> = vec![
        vec![0x1c],           // drop
        vec![0x1c, 0x1c],     // drop, drop
        vec![0x1c, 0x1c, 0x1c], // drop, drop, drop
    ];
    // payload size must match the program's drop count so the cell-open
    // run leaves an empty stack. Test each program with that exact payload.
    for i in 0..programs.len() {
        let (tree, cp) =
            build_multi_leaf_predicate(programs.clone(), i, 11 + i as u64);
        let pred_point = tree.compute_point();

        // payload = i+1 copies of push:5 (so program of length i+1
        // can drop them all and end with empty stack)
        let payload_count = i + 1;
        let mut script = Vec::new();
        for _ in 0..payload_count {
            script.push(0x05); // push:5
        }
        push_small_uint(&mut script, payload_count as u32);
        push_point_bytes(&mut script, pred_point.as_bytes());
        script.push(0x91); // cell
        push_callproof_pieces(&mut script, &cp);
        script.push(0x00); // k=0 args
        script.push(0x93); // open
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap_or_else(|e| {
            panic!("program index {} did not open cleanly: {:?}", i, e)
        });
        assert!(
            vm.current_call.stack.is_empty(),
            "program index {} left stack non-empty",
            i
        );
    }
}

#[test]
fn multi_leaf_predicate_wrong_leaf_path_hard_fails() {
    // Build a 3-leaf tree. Construct a CallProof claiming program[0]
    // but with the path that opens program[1]. Verification must fail.
    let programs: Vec<Vec<u8>> = vec![
        vec![0x1c],
        vec![0x1c, 0x1c],
        vec![0x1c, 0x1c, 0x1c],
    ];
    let (tree, valid_cp_for_1) =
        build_multi_leaf_predicate(programs.clone(), 1, 7);
    // Forge: use program[0]'s bytes but program[1]'s path/neighbors.
    let forged = CallProof {
        internal_key: valid_cp_for_1.internal_key,
        neighbors: valid_cp_for_1.neighbors.clone(),
        position: valid_cp_for_1.position.clone(),
        program: programs[0].clone(),
    };
    let pred_point = tree.compute_point();

    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, pred_point.as_bytes());
    script.push(0x91);
    push_callproof_pieces(&mut script, &forged);
    script.push(0x00);
    script.push(0x93);
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::CallProofMismatch
    ));
}

#[test]
fn callproof_for_out_of_range_index_errors() {
    let secret = Scalar::from(1u64);
    let ik = (&secret * &RISTRETTO_BASEPOINT_TABLE).compress();
    let tree = PredicateTree::new(
        Some(ik),
        vec![vec![0x1d], vec![0x1c]],
        TEST_BLINDING_KEY,
    )
    .unwrap();
    assert!(matches!(
        tree.callproof_for(5).unwrap_err(),
        VMError::ProgramIndexOutOfRange
    ));
}

//
// A cell's payload bytes can only return to the stack via `open`,
// `signtx`, or `signcall` — each of which consumes the source cell
// and records (open) or defers (signtx/signcall) an authorization
// check. There is no "transfer the cell handle into a new output"
// shortcut, because `Value::Cell` is non-portable and the cell
// construction opcodes (`cell`, `output`) reject non-portable
// payload items.

#[test]
fn output_rejects_cell_as_payload_item() {
    // Build cell A on stack. Then try: count=1, predicate_point, output
    //   — the output op pops pred + count + 1 payload item (cell A)
    //     and `pop_n_portable` must reject cell A.
    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell A → on stack
    // Now build the outer: 1-item payload = [cell A], pred, output.
    script.push(0x01); // count = 1
    push_point_bytes(&mut script, &[0xbb; 32]);
    script.push(0x92); // output
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn cell_opcode_rejects_cell_as_payload_item() {
    // Symmetric protection on the `cell` construction op.
    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell A
    script.push(0x01); // count=1
    push_point_bytes(&mut script, &[0xbb; 32]);
    script.push(0x91); // cell (attempted outer)
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn output_rejects_dict_containing_a_cell() {
    // Even if a script hides a cell inside a Dict and puts the Dict
    // (otherwise portable) into the payload, the Dict's sticky
    // portability flag rejects it.
    //
    //   build cell A
    //   push key=0, push count=1, dict        // Dict { 0: cellA }
    //   push count=1, pushpoint, output
    let mut script = vec![0x05, 0x01];
    push_point_bytes(&mut script, &[0xaa; 32]);
    script.push(0x91); // cell A on stack
    script.push(0x00); // key = 0
    script.push(0x01); // count = 1 pair
    script.push(0x60); // dict — pops (cellA, 0, 1) → Dict { 0: cellA }
    script.push(0x01); // outer count = 1
    push_point_bytes(&mut script, &[0xbb; 32]);
    script.push(0x92); // output
    let mut vm = vm_with_script(script);
    vm.last_anchor = Some(Anchor([0x42; 32]));
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInOutput
    ));
}

#[test]
fn cell_id_changes_when_payload_value_changes() {
    // Two cells with the same predicate + same anchor + same payload
    // type-shape but different values must have different ids.
    // Per architect response 9.3: payload bytes are bound via the
    // canonical encoding API.
    let pred = Predicate::Opaque(CompressedRistretto([0xaa; 32]));
    let a = Anchor([0x42; 32]);
    let c1 = Cell::new(
        pred.clone(),
        a,
        vec![Value::Int253(Int253::from(5u64))],
    );
    let c2 = Cell::new(
        pred,
        a,
        vec![Value::Int253(Int253::from(99u64))],
    );
    assert_ne!(c1.id(), c2.id());
}

#[test]
fn cell_encode_decode_roundtrip() {
    let original = fixture_cell();
    let bytes = encode_cell_to_bytes(&original);

    // Decode and confirm equivalence by cell id (the canonical
    // identity hash binds predicate point + anchor + payload bytes).
    let mut reader: &[u8] = &bytes;
    let decoded = Cell::decode(&mut reader).expect("decodes");
    assert!(reader.is_empty(), "decoder must consume the full input");
    assert_eq!(original.id(), decoded.id());
    assert_eq!(original.anchor.0, decoded.anchor.0);
    assert_eq!(
        original.predicate.to_point().as_bytes(),
        decoded.predicate.to_point().as_bytes()
    );
    assert_eq!(original.payload.len(), decoded.payload.len());
}

#[test]
fn cell_decode_rejects_empty_input() {
    let err = decode_cell_dropping_ok(&[]).unwrap_err();
    assert!(matches!(err, VMError::MalformedCellEncoding));
}

#[test]
fn cell_decode_rejects_wrong_outer_count() {
    // Outer list-Dict with two entries instead of three (anchor +
    // payload prefix, no predicate). Bytes are hand-rolled to make
    // the outer prefix valid but the inner shape wrong.
    // Outer count = 2 (immediate small list-Dict tag in the encoding).
    // We exploit the fact that any prefix that successfully reads as
    // a list-Dict with count != 3 must fail.
    // Construct a real cell and then patch the outer count.
    let mut bytes = encode_cell_to_bytes(&fixture_cell());
    // First byte encodes the outer list-Dict prefix; just rewrite the
    // top-level prefix byte to a list-Dict of count 2. We use the
    // round-trip helper: build a 2-element list-Dict by hand.
    // Simpler: replace the *whole* string with a list-Dict of count 0,
    // which is canonical but wrong arity.
    bytes.clear();
    crate::encoding::write_list_prefix(&mut bytes, 0)
        .expect("write prefix");
    let err = decode_cell_dropping_ok(&bytes).unwrap_err();
    assert!(matches!(err, VMError::MalformedCellEncoding));
}

#[test]
fn cell_decode_rejects_wrong_anchor_length() {
    // Build an outer list-Dict of 3 entries by hand: Point predicate,
    // a String of wrong (31-byte) anchor, then an empty payload list.
    let mut bytes = Vec::new();
    crate::encoding::write_list_prefix(&mut bytes, 3)
        .expect("write outer prefix");
    crate::encoding::write_value(
        &mut bytes,
        &Value::Point(Point::from_bytes([0xaa; 32])),
    )
    .expect("write point");
    crate::encoding::write_value(
        &mut bytes,
        &Value::String(crate::String::from(vec![0u8; 31])),
    )
    .expect("write short anchor");
    crate::encoding::write_list_prefix(&mut bytes, 0)
        .expect("write payload prefix");
    let err = decode_cell_dropping_ok(&bytes).unwrap_err();
    assert!(matches!(err, VMError::MalformedCellEncoding));
}

#[test]
fn cell_decode_rejects_predicate_not_a_point() {
    // First entry is a String where a Point is expected.
    let mut bytes = Vec::new();
    crate::encoding::write_list_prefix(&mut bytes, 3)
        .expect("write outer prefix");
    crate::encoding::write_value(
        &mut bytes,
        &Value::String(crate::String::from(vec![0u8; 32])),
    )
    .expect("write wrong predicate");
    crate::encoding::write_value(
        &mut bytes,
        &Value::String(crate::String::from(vec![0u8; 32])),
    )
    .expect("write anchor");
    crate::encoding::write_list_prefix(&mut bytes, 0)
        .expect("write payload prefix");
    let err = decode_cell_dropping_ok(&bytes).unwrap_err();
    assert!(matches!(err, VMError::MalformedCellEncoding));
}

#[test]
fn input_pushes_cell_seeds_anchor_and_emits_txlog() {
    let cell = fixture_cell();
    let expected_id = cell.id();
    let expected_anchor = cell.to_anchor();
    let bytes = encode_cell_to_bytes(&cell);

    // Build an ExternalRoot VM with the wire bytes on the stack as a String.
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(bytes)));
    vm.op_input().expect("input succeeds");

    // Top of stack is the decoded Cell.
    assert_eq!(vm.current_call.stack.len(), 1);
    match &vm.current_call.stack[0] {
        Value::Cell(c) => {
            assert_eq!(c.id(), expected_id);
        }
        other => panic!("expected Cell on stack, got {}", value_kind(other)),
    }

    // last_anchor seeded to the cell's ratcheted anchor.
    assert_eq!(vm.last_anchor.expect("anchor seeded").0, expected_anchor.0);

    // Txlog has Header + one Input entry committing the cell id.
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Input(id) => assert_eq!(*id, expected_id),
        _ => panic!("expected TxEntry::Input"),
    }
}

#[test]
fn input_requires_string_on_top() {
    // Non-String top → TypeNotString. (Use an Int253.)
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::Int253(Int253::from(7u64)));
    let err = vm.op_input().unwrap_err();
    assert!(matches!(err, VMError::TypeNotString));
}

#[test]
fn input_rejects_malformed_bytes() {
    // Random non-canonical bytes on the stack.
    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(vec![0xffu8; 8])));
    let err = vm.op_input().unwrap_err();
    assert!(matches!(err, VMError::MalformedCellEncoding));
}

#[test]
fn input_rejects_trailing_bytes_after_cell() {
    // Append a stray byte after a canonical encoding so the inner
    // reader leaves bytes unread → MalformedCellEncoding.
    let cell = fixture_cell();
    let mut bytes = encode_cell_to_bytes(&cell);
    bytes.push(0x00); // trailing garbage

    let mut vm = vm_external_with_script(Vec::new());
    vm.push_value(Value::String(crate::String::from(bytes)));
    let err = vm.op_input().unwrap_err();
    assert!(matches!(err, VMError::MalformedCellEncoding));
}

#[test]
fn input_in_internal_context_errors_external_only() {
    // Drive `0x90` through `step_internal` — dispatch must surface
    // `ExternalOnly` because internal transactions cannot consume
    // Utreexo entries.
    let mut vm = vm_with_script(vec![0x90]);
    // Even with a well-formed string on the stack, internal context
    // rejects the opcode before any decoding happens.
    let cell_bytes = encode_cell_to_bytes(&fixture_cell());
    vm.push_value(Value::String(crate::String::from(cell_bytes)));
    let err = vm.step_internal().unwrap_err();
    assert!(matches!(err, VMError::ExternalOnly));
}

#[test]
fn input_then_output_anchor_chain() {
    // Round-trip: input a cell, then output a new cell whose anchor
    // is derived from the consumed cell. Confirms `last_anchor` is
    // wired through `input` so a subsequent `output` doesn't need an
    // external seed.
    let cell = fixture_cell();
    let bytes = encode_cell_to_bytes(&cell);
    let expected_anchor_after_input = cell.to_anchor();

    let mut vm = vm_external_with_script(Vec::new());

    // Step 1: feed cell bytes into op_input.
    vm.push_value(Value::String(crate::String::from(bytes)));
    vm.op_input().expect("input ok");
    // Stack: [Cell]. last_anchor: Some(ratcheted anchor from input).
    assert_eq!(
        vm.last_anchor.expect("anchor").0,
        expected_anchor_after_input.0
    );

    // The consumed cell's handle is still on the stack. For a stand-alone
    // anchor-chain test we don't care about authorizing it — drop it
    // directly so we can exercise `op_output` against the seeded anchor.
    let _consumed = vm.pop_value().expect("pop value").to_cell().expect("pop cell handle");

    // Step 2: build an output through the real op_output handler.
    // Stack pre-output: [payload(5), count(1), predicate(Point)].
    vm.push_value(Value::Int253(Int253::from(5u64)));
    vm.push_value(Value::Int253(Int253::from(1u64)));
    vm.push_value(Value::Point(Point::from_bytes([0xbb; 32])));
    vm.op_output().expect("output ok");

    // Txlog now has: Header, Input(consumed_id), Output(new_cell).
    assert_eq!(vm.txlog.len(), 3);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Input(_) => {}
        _ => panic!("second entry must be Input"),
    }
    match &vm.txlog[2] {
        crate::tx::TxEntry::Output(_) => {}
        _ => panic!("third entry must be Output"),
    }
    // last_anchor advanced again past the output cell.
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
    let cell = fixture_cell();
    let expected_id = cell.id();
    let bytes = encode_cell_to_bytes(&cell);

    let mut vm = vm_external_with_script(vec![0x90]);
    vm.push_value(Value::String(crate::String::from(bytes)));

    let mut delegate = StubDelegate::new();
    let cont = vm.step_external(&mut delegate).expect("step ok");
    assert!(cont, "still running (script not exhausted)");

    // Stack now has the decoded cell; txlog has Header + Input entry.
    match &vm.current_call.stack[0] {
        Value::Cell(c) => assert_eq!(c.id(), expected_id),
        other => panic!("expected Cell, got {}", value_kind(other)),
    }
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Input(id) => assert_eq!(*id, expected_id),
        _ => panic!("expected TxEntry::Input"),
    }
}

#[test]
fn external_tx_one_input_one_output_via_signtx() {

    // A single external transaction consumes one cell (authorized
    // via signtx — the cell holder signs the whole tx via the
    // envelope) and emits a single fresh cell.
    //
    // Cell life-cycle traced end-to-end:
    //   bytes  → input  → cell on stack → signtx (deferred sig +
    //   payload poured) → drop payload → push fresh payload →
    //   output → TxEntry::Output → finalize.

    // 1. Construct the input cell, capture its identity, encode it.
    let input_cell = fixture_cell();
    let input_id = input_cell.id();
    let input_predicate_point =
        input_cell.predicate.to_point();
    let input_anchor_post = input_cell.to_anchor();
    let input_bytes = encode_cell_to_bytes(&input_cell);

    // 2. Assemble the script.
    //
    // Stack diagram (top of stack on the right):
    //   pushstr <bytes>       []                  → [String]
    //   input                 [String]            → [Cell]
    //   signtx                [Cell]              → [Int253(7), String, Int253(2)]
    //                          (payload + count poured; TxBound recorded)
    //   drop                  [..7, "hello", 2]   → [..7, "hello"]
    //   drop                  [..7, "hello"]      → [..7]
    //   drop                  [..7]               → []
    //   push:42               []                  → [Int253(42)]
    //   push:1                [Int253(42)]        → [Int253(42), Int253(1)]
    //   pushpoint <P_out>     [..1]               → [..1, Point]
    //   output                [..Point]           → []  (Output effect emitted)
    let mut script = Vec::new();
    push_string_bytes(&mut script, &input_bytes);
    script.push(0x90); // input
    script.push(0x98); // signtx
    script.push(0x1c); // drop count
    script.push(0x1c); // drop "hello"
    script.push(0x1c); // drop 7
    script.push(0x10); // pushint8 positive
    script.push(42);
    script.push(0x01); // count = 1
    let out_pred_bytes = [0xbb; 32];
    push_point_bytes(&mut script, &out_pred_bytes);
    script.push(0x92); // output

    // 3. Run through `step_external` to completion + finalize.
    let vm = run_external_workflow(script);

    // 4. Assertions on the final VM state.

    // 4a. Clean exit: stack must be empty.
    assert!(
        vm.current_call.stack.is_empty(),
        "leftover stack at end of tx: {:?}",
        vm.current_call.stack.len()
    );

    // 4b. Txlog: Header(index 0), Input(cell_in_id) at 1,
    //     Output(cell_out) at 2.
    assert_eq!(vm.txlog.len(), 3, "expected Header + Input + Output txlog");
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Input(id) => assert_eq!(*id, input_id),
        _ => panic!("txlog[1] must be Input"),
    }
    let output_cell_anchor = match &vm.txlog[2] {
        crate::tx::TxEntry::Output(c) => {
            // Output payload was [Int253(42)].
            assert_eq!(c.payload.len(), 1);
            match &c.payload[0] {
                Value::Int253(i) => assert_eq!(*i, Int253::from(42u64)),
                _ => panic!("output payload[0] must be Int253(42)"),
            }
            // Output predicate is the point we pushed.
            assert_eq!(
                c.predicate.to_point().as_bytes(),
                &out_pred_bytes
            );
            c.anchor
        }
        _ => panic!("txlog[2] must be Output"),
    };

    // 4c. Anchor chain: the output's anchor is the post-input anchor
    //     (i.e. cell_in.to_anchor()), since no other cell was created
    //     between input and output.
    assert_eq!(output_cell_anchor.0, input_anchor_post.0);

    // 4d. Deferred sigs: exactly one TxBound entry, with verification
    //     key matching the input cell's predicate point.
    assert_eq!(vm.deferred_sigs.len(), 1);
    match &vm.deferred_sigs[0] {
        DeferredSig::TxBound { verification_key, .. } => {
            assert_eq!(
                verification_key.as_bytes(),
                input_predicate_point.as_bytes()
            );
        }
        DeferredSig::Explicit { .. } => {
            panic!("expected TxBound, got Explicit")
        }
    }

    // 4e. last_anchor has advanced past the output cell's own
    //     ratcheted anchor (so a hypothetical subsequent output
    //     would land at a different anchor).
    assert!(vm.last_anchor.is_some());
    assert_ne!(vm.last_anchor.unwrap().0, output_cell_anchor.0);
}

#[test]
fn external_tx_two_inputs_two_outputs_via_open() {

    // External tx consumes two distinct cells via `open` (each
    // unlocked by a valid Taproot CallProof against its predicate
    // tree), then emits two fresh output cells. No `signtx` /
    // `signcall` here, so `deferred_sigs` stays empty.
    //
    // Each input cell's program is `drop` — it consumes the single
    // payload item the cell-open pours onto the stack.

    let prog = vec![0x1c]; // drop

    let (tree1, cp1) = build_predicate_with_program(&prog, 11);
    let cell1 = Cell::new(
        Predicate::Opaque(tree1.compute_point()),
        Anchor([0xa1; 32]),
        vec![Value::Int253(Int253::from(11u64))],
    );
    let cell1_id = cell1.id();
    let cell1_bytes = encode_cell_to_bytes(&cell1);

    let (tree2, cp2) = build_predicate_with_program(&prog, 22);
    let cell2 = Cell::new(
        Predicate::Opaque(tree2.compute_point()),
        Anchor([0xa2; 32]),
        vec![Value::Int253(Int253::from(22u64))],
    );
    let cell2_id = cell2.id();
    let cell2_anchor_post = cell2.to_anchor();
    let cell2_bytes = encode_cell_to_bytes(&cell2);

    //
    //   ┌─── consume cell 1 ─────────────────────────────────┐
    //   │ pushstr <cell1_bytes>                              │
    //   │ input                — pops String → pushes Cell1  │
    //   │ <callproof1 pieces>                                │
    //   │ push:0               — k = 0 args                  │
    //   │ open                 — verifies cp1, pours [11],   │
    //   │                       enters Run over `drop`;      │
    //   │                       inner Run pops the 11        │
    //   └────────────────────────────────────────────────────┘
    //   ┌─── consume cell 2 ─────────────────────────────────┐
    //   │ pushstr <cell2_bytes>                              │
    //   │ input                                              │
    //   │ <callproof2 pieces>                                │
    //   │ push:0                                             │
    //   │ open                                               │
    //   └────────────────────────────────────────────────────┘
    //   ┌─── emit output 1 ──────────────────────────────────┐
    //   │ push:9   push:1   pushpoint <P_out1>   output      │
    //   └────────────────────────────────────────────────────┘
    //   ┌─── emit output 2 ──────────────────────────────────┐
    //   │ push:10  push:1   pushpoint <P_out2>   output      │
    //   └────────────────────────────────────────────────────┘
    let mut script = Vec::new();

    // Consume cell 1
    push_string_bytes(&mut script, &cell1_bytes);
    script.push(0x90); // input
    push_callproof_pieces(&mut script, &cp1);
    script.push(0x00); // k = 0 args
    script.push(0x93); // open

    // Consume cell 2
    push_string_bytes(&mut script, &cell2_bytes);
    script.push(0x90); // input
    push_callproof_pieces(&mut script, &cp2);
    script.push(0x00); // k = 0 args
    script.push(0x93); // open

    // Emit output 1
    script.push(0x09); // push:9
    script.push(0x01); // count = 1
    let out1_pred_bytes = [0xc1; 32];
    push_point_bytes(&mut script, &out1_pred_bytes);
    script.push(0x92); // output

    // Emit output 2
    script.push(0x0a); // push:10
    script.push(0x01); // count = 1
    let out2_pred_bytes = [0xc2; 32];
    push_point_bytes(&mut script, &out2_pred_bytes);
    script.push(0x92); // output

    let vm = run_external_workflow(script);

    // Clean stack.
    assert!(vm.current_call.stack.is_empty());

    // Txlog: Header, 2 × Input, 2 × Output, in that order.
    assert_eq!(vm.txlog.len(), 5, "expected Header + 2 inputs + 2 outputs");
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Input(id) => assert_eq!(*id, cell1_id),
        _ => panic!("txlog[1] must be Input(cell1)"),
    }
    match &vm.txlog[2] {
        crate::tx::TxEntry::Input(id) => assert_eq!(*id, cell2_id),
        _ => panic!("txlog[2] must be Input(cell2)"),
    }
    let (out1, out2) = match (&vm.txlog[3], &vm.txlog[4]) {
        (
            crate::tx::TxEntry::Output(o1),
            crate::tx::TxEntry::Output(o2),
        ) => (o1, o2),
        _ => panic!("txlog[3..5] must be Output entries"),
    };

    // Output 1's payload is [Int253(9)], predicate matches what we
    // pushed.
    assert_eq!(out1.payload.len(), 1);
    match &out1.payload[0] {
        Value::Int253(i) => assert_eq!(*i, Int253::from(9u64)),
        _ => panic!("out1.payload[0] must be Int253(9)"),
    }
    assert_eq!(out1.predicate.to_point().as_bytes(), &out1_pred_bytes);
    assert_eq!(out2.payload.len(), 1);
    match &out2.payload[0] {
        Value::Int253(i) => assert_eq!(*i, Int253::from(10u64)),
        _ => panic!("out2.payload[0] must be Int253(10)"),
    }
    assert_eq!(out2.predicate.to_point().as_bytes(), &out2_pred_bytes);

    // Anchor chain:
    //   - cell1 input ratchets last_anchor → cell1.to_anchor()
    //   - cell2 input overwrites last_anchor → cell2.to_anchor()
    //   - output1 consumes last_anchor → out1.anchor == cell2.to_anchor()
    //   - output1 ratchets → out1.to_anchor()
    //   - output2 consumes last_anchor → out2.anchor == out1.to_anchor()
    //   - output2 ratchets → final last_anchor
    assert_eq!(out1.anchor.0, cell2_anchor_post.0);
    assert_eq!(out2.anchor.0, out1.to_anchor().0);
    assert_ne!(out1.anchor.0, out2.anchor.0);
    let final_anchor = vm.last_anchor.expect("anchor set after output 2");
    assert_eq!(final_anchor.0, out2.to_anchor().0);

    // No `signtx` / `signcall` were used → no deferred sigs.
    assert!(
        vm.deferred_sigs.is_empty(),
        "open does not record deferred sigs"
    );
}

