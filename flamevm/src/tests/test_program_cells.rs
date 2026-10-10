//! Native program Cells, consuming literal refs, and same-frame tail execution.

use super::test_helpers::*;

fn native_vm(program: Cell) -> VM {
    VM::new(
        dummy_header(),
        CallFrame::from_cell(
            program,
            CallKind::InternalRoot {
                actor: ActorID::Hash([1; 32]),
                caller: Some(ActorID::Hash([2; 32])),
            },
            1_000_000,
        )
        .unwrap()
        .with_anchor(Anchor([3; 32])),
    )
}

fn cell_id(value: &Value) -> CellID {
    match value {
        Value::Cell(reference) => reference.id(),
        _ => panic!("expected Cell"),
    }
}

#[test]
fn program_payload_is_raw_bytecode_and_pushcell_consumes_refs_in_order() {
    let first = Cell::new(vec![9], vec![]).unwrap();
    let second = Cell::new(vec![8], vec![]).unwrap();
    let program = ScriptBuilder::new()
        .push_cell(first.clone())
        .push_cell(second.clone())
        .to_cell()
        .unwrap();
    assert_eq!(program.payload(), [0xa6, 0xa6]);
    assert_eq!(program.refs().len(), 2);
    let mut vm = native_vm(program);
    run_to_end(&mut vm).unwrap();
    assert_eq!(cell_id(&vm.current_call.stack[0]), first.id());
    assert_eq!(cell_id(&vm.current_call.stack[1]), second.id());
    assert!(matches!(
        Instruction::parse(&mut &[0xa6][..]).unwrap(),
        Instruction::PushCell(None)
    ));
    assert!(matches!(
        Instruction::parse(&mut &[0xa7][..]).unwrap(),
        Instruction::Exec
    ));
}

#[test]
fn cell_handles_are_portable_droppable_and_copyable() {
    let cell = Cell::new(vec![1], vec![]).unwrap();
    let value = Value::Cell(cell.clone().into());
    assert!(value.is_portable());
    assert!(value.is_droppable());
    assert!(value.is_copyable());
    assert_eq!(cell_id(&value.try_clone().unwrap()), cell.id());
    assert_eq!(value.type_code(), 13);
    let encoded = value.to_cell().unwrap();
    assert_eq!(encoded.payload(), [13]);
    let decoded = Value::from_cell(&encoded, &mut ()).unwrap();
    assert_eq!(cell_id(&decoded), cell.id());
    let mut vm = native_vm(
        ScriptBuilder::new()
            .push_cell(cell)
            .dup_k(0)
            .to_cell()
            .unwrap(),
    );
    run_to_end(&mut vm).unwrap();
    assert_eq!(
        cell_id(&vm.current_call.stack[0]),
        cell_id(&vm.current_call.stack[1])
    );
}

#[test]
fn jumping_back_does_not_rewind_the_reference_cursor() {
    let reference: CellRef = Cell::new(vec![9], vec![]).unwrap().into();
    let mut source = ScriptBuilder::new().label(0);
    source.push_instr(Instruction::PushCell(None));
    let bytes = source.drop_().jump(0).to_bytecode();
    let mut vm = native_vm(Cell::new(bytes, vec![reference]).unwrap());
    assert!(matches!(
        run_to_end(&mut vm),
        Err(VMError::Cell(CellError::InsufficientReferences))
    ));
}

#[test]
fn jump_scanning_does_not_execute_pushcell() {
    let first = Cell::new(vec![9], vec![]).unwrap();
    let second = Cell::new(vec![8], vec![]).unwrap();
    let builder = ScriptBuilder::new()
        .jump(0)
        .push_cell(first.clone())
        .drop_()
        .label(0)
        .push_cell(second);
    let mut vm = native_vm(builder.to_cell().unwrap());
    run_to_end(&mut vm).unwrap();
    assert_eq!(cell_id(&vm.current_call.stack[0]), first.id());
}

#[test]
fn exec_preserves_stack_identity_budget_and_anchor_and_discards_old_code() {
    let continuation = ScriptBuilder::new()
        .push_int(9u64)
        .add()
        .selfid()
        .callerid()
        .gaslimit()
        .to_cell()
        .unwrap();
    let mut source = ScriptBuilder::new()
        .push_int(7u64)
        .push_cell(continuation.clone())
        .exec();
    source.push_instr(Instruction::Ext(0xff));
    let original = source.to_cell().unwrap();
    let mut vm = native_vm(original.clone());
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Scalar::from(16u64));
    assert_str(&vm.current_call.stack[1], &[1; 32]);
    assert_str(&vm.current_call.stack[2], &[2; 32]);
    assert_int(&vm.current_call.stack[3], Scalar::from(1_000_000u64));
    assert_eq!(
        vm.current_call.program.as_ref().unwrap().root.id(),
        continuation.id()
    );
    assert_eq!(
        vm.current_call.entry_program.as_ref().unwrap().id(),
        original.id()
    );
    assert_eq!(vm.last_anchor, Some(Anchor([3; 32])));
    assert!(vm.call_stack.is_empty());
}

#[test]
fn exec_resets_both_cursors_and_cell_local_labels() {
    let data = Cell::new(vec![9], vec![]).unwrap();
    let continuation = ScriptBuilder::new()
        .label(0)
        .push_cell(data.clone())
        .to_cell()
        .unwrap();
    let source = ScriptBuilder::new()
        .label(0)
        .push_cell(continuation)
        .exec()
        .to_cell()
        .unwrap();
    let mut vm = native_vm(source);
    run_to_end(&mut vm).unwrap();
    assert_eq!(cell_id(&vm.current_call.stack[0]), data.id());
}

#[test]
fn exec_reads_only_when_executed_and_prices_resident_and_unloaded_equally() {
    let continuation = Cell::new(vec![0x1d], vec![]).unwrap();
    let reference: CellRef = continuation.clone().into();
    let make = |reference| {
        ScriptBuilder::new()
            .push_cell(reference)
            .exec()
            .to_cell()
            .unwrap()
    };
    let mut resident = native_vm(make(reference.clone()));
    run_to_end(&mut resident).unwrap();
    let mut bodies = CellIndex::new();
    bodies.insert(Arc::new(continuation.clone())).unwrap();
    let mut unloaded = native_vm(make(reference.to_unloaded()))
        .with_cells(Arc::new(bodies))
        .unwrap();
    run_to_end(&mut unloaded).unwrap();
    assert_eq!(
        resident.current_call.gas_used,
        unloaded.current_call.gas_used
    );
    let mut missing = native_vm(make(reference.to_unloaded()));
    assert!(missing.step_internal().unwrap()); // pushcell does not load the body
    assert!(
        matches!(missing.step_internal(), Err(VMError::Cell(CellError::MissingCell(id))) if id == continuation.id())
    );
    let pruned = Cell::from_pruned(1, vec![continuation.id()], vec![0]).unwrap();
    let mut pruned_vm = native_vm(make(pruned.into()));
    assert!(matches!(
        run_to_end(&mut pruned_vm),
        Err(VMError::Cell(CellError::PrunedCell))
    ));
}

#[test]
fn compiler_continuations_preserve_private_assignments_and_transaction_identity() {
    let mut script = ScriptBuilder::new();
    for _ in 0..5000 {
        script = script.nop();
    }
    script = script
        .alloc(Some(Scalar::from(7u64)))
        .push_int(7u64)
        .eq()
        .verify();
    let root = script.to_cell().unwrap();
    assert!(root.payload().ends_with(&[0xa6, 0xa7]));
    assert!(root.refs().len() == 1);
    let unsigned = script
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap();
    let txid = unsigned.txid();
    let tx = unsigned.without_signature().unwrap();
    assert_eq!(tx.program().unwrap().id(), root.id());
    let decoded =
        crate::ExternalTx::from_bytes_bounded(&tx.to_bytes().unwrap(), 1, 4095, 2497).unwrap();
    assert_eq!(decoded.txid, txid);
    let (_, metrics) = decoded
        .verify_with_metrics(crate::Limits { gas: 1_000_000 })
        .unwrap();
    let proved = Prover::prove(
        &PedersenGens::default(),
        ScriptBuilder::new().push_cell(root).exec(),
        dummy_header(),
        1_000_000,
    );
    assert!(
        proved.is_err(),
        "private assignment must not be invented from public Cell bytes"
    );
    assert!(metrics.gas_used > 5000);
}

#[test]
fn taproot_exec_failure_returns_the_contract_and_cell_argument() {
    let failure = ScriptBuilder::new()
        .push_int(0u64)
        .verify()
        .to_cell()
        .unwrap();
    let branch = ScriptBuilder::new().push_cell(failure).exec();
    let recovery = ScriptBuilder::new().push_int(1u64).return_();
    let tree = PredicateTree::from_scripts(None, vec![branch, recovery], [7; 32]).unwrap();
    let contract = Contract::new(
        Predicate::tree(tree.clone()),
        Anchor([4; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(5u64), Scalar::from(3u64))),
    )
    .unwrap();
    let arg = Cell::new(vec![9], vec![]).unwrap();
    let source = ScriptBuilder::new()
        .push_str(String::contract(contract))
        .input()
        .push_taproot_proof(&tree, 0)
        .unwrap()
        .push_int(10_000u64)
        .push_cell(arg)
        .push_int(1u64)
        .open()
        .push_int(0u64)
        .eq()
        .verify()
        .drop_()
        .drop_()
        .push_int(1u64)
        .eq()
        .verify()
        .drop_()
        .drop_()
        .drop_()
        .push_taproot_proof(&tree, 1)
        .unwrap()
        .push_int(10_000u64)
        .push_int(0u64)
        .open()
        .verify()
        .drop_()
        .push_point(*Predicate::unspendable_key().as_bytes())
        .output();
    let tx = source
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap()
        .without_signature()
        .unwrap();
    let decoded =
        crate::ExternalTx::from_bytes_bounded(&tx.to_bytes().unwrap(), 1, 4095, 2497).unwrap();
    decoded.verify(crate::Limits { gas: 1_000_000 }).unwrap();
}

#[test]
fn exec_failure_restores_entry_assets_without_touching_the_parent_loan() {
    let failed = ScriptBuilder::new()
        .push_int(0u64)
        .verify()
        .to_cell()
        .unwrap();
    let source = ScriptBuilder::new()
        .push_cell(failed)
        .exec()
        .to_cell()
        .unwrap();
    let mut vm = vm_with_nested_child_script(vec![]);
    vm.current_call = CallFrame::from_cell(
        source,
        CallKind::ContractOpen {
            predicate: Predicate::opaque(Predicate::unspendable_key()),
            external_context: true,
            caller_id: None,
        },
        500,
    )
    .unwrap();
    let credit = Value::ClearToken(ClearToken::new(Scalar::from(5u64), Scalar::from(3u64)));
    vm.current_call.stack.push(credit.clone());
    let parent = vm.call_stack.last_mut().unwrap();
    parent.stack.push(Value::ClearToken(ClearToken::new(
        Scalar::from(-5i64),
        Scalar::from(3u64),
    )));
    parent.snap_failure_values = vec![credit];
    parent.snap_failure_arg_count = 1;
    while !vm.call_stack.is_empty() {
        vm.step_internal().unwrap();
    }
    assert_eq!(vm.current_call.stack.len(), 4);
    for (value, quantity) in vm.current_call.stack[..2].iter().zip([-5i64, 5]) {
        match value {
            Value::ClearToken(token) => assert_eq!(token.qty(), Scalar::from(quantity)),
            _ => panic!("expected loan counterpart"),
        }
    }
    assert_int(&vm.current_call.stack[2], Scalar::ONE);
    assert_int(&vm.current_call.stack[3], Scalar::ZERO);
}

#[test]
fn compiler_does_not_split_jump_scopes_or_instructions() {
    let mut labeled = ScriptBuilder::new().label(0);
    for _ in 0..5000 {
        labeled = labeled.nop();
    }
    assert!(matches!(labeled.to_cell(), Err(CellError::InvalidFormat)));
    let oversized = ScriptBuilder::new().push_str(String::from(vec![0; String::MAX_LEN + 1]));
    assert!(matches!(
        oversized.to_cell(),
        Err(CellError::PayloadTooLarge { .. })
    ));
    let mut many_refs = ScriptBuilder::new();
    for _ in 0..9 {
        many_refs = many_refs
            .push_cell(Cell::new(vec![], vec![]).unwrap())
            .drop_();
    }
    let mut vm = native_vm(many_refs.to_cell().unwrap());
    run_until_tx_done(&mut vm).unwrap();
    let mut full_suffix = ScriptBuilder::new();
    for _ in 0..32 {
        full_suffix = full_suffix.nop();
    }
    full_suffix = full_suffix.nop();
    for _ in 0..8155 {
        full_suffix = full_suffix.nop();
    }
    let program = full_suffix.to_cell().unwrap();
    assert_eq!(
        program.refs()[0].as_resident().unwrap().payload().len(),
        cells::MAX_CELL_PAYLOAD
    );
    run_until_tx_done(&mut native_vm(program)).unwrap();
}

#[test]
fn program_frontier_availability_survives_transport_normalization() {
    let target = Cell::new(vec![], vec![]).unwrap();
    let reference: CellRef = target.clone().into();
    // The body is supplied by a later, unused ref to the same content. Whether
    // the first handle is resident must not select success versus failure.
    let script = ScriptBuilder::new()
        .push_cell(reference.to_unloaded())
        .exec()
        .push_cell(target)
        .drop_();
    let unsigned = script
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap();
    let gas = unsigned.metrics().gas_used;
    let tx = unsigned.without_signature().unwrap();
    let decoded =
        crate::ExternalTx::from_bytes_bounded(&tx.to_bytes().unwrap(), 1, 4095, 2497).unwrap();
    assert_eq!(
        decoded
            .verify_with_metrics(crate::Limits { gas: 1_000_000 })
            .unwrap()
            .1
            .gas_used,
        gas
    );
    assert_eq!(
        decoded.execution_cells().unwrap().len(),
        tx.execution_cells().unwrap().len()
    );
}

#[test]
fn program_refs_do_not_need_an_extra_availability_pruning_level() {
    let pruned = Cell::from_pruned(1 << 14, vec![[9; 32]], vec![0]).unwrap();
    let script = ScriptBuilder::new().push_cell(pruned).drop_();
    let tx = script
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap()
        .without_signature()
        .unwrap();
    let decoded =
        crate::ExternalTx::from_bytes_bounded(&tx.to_bytes().unwrap(), 1, 4095, 2497).unwrap();
    decoded.verify(crate::Limits { gas: 1_000_000 }).unwrap();
}

#[test]
fn exec_cannot_create_gas_or_execute_other_value_types() {
    let target = Cell::new(vec![0x1d; 4095], vec![]).unwrap();
    let mut vm = native_vm(
        ScriptBuilder::new()
            .push_cell(target)
            .exec()
            .to_cell()
            .unwrap(),
    );
    vm.current_call.gas_limit = 10;
    assert!(matches!(run_to_end(&mut vm), Err(VMError::OutOfGas)));
    let mut wrong = native_vm(
        ScriptBuilder::new()
            .push_int(0u64)
            .exec()
            .to_cell()
            .unwrap(),
    );
    assert!(matches!(run_to_end(&mut wrong), Err(VMError::TypeNotCell)));
}

#[test]
fn script_cell_codec_preserves_refs_and_string_execution_rejects_hidden_refs() {
    let cell = Cell::new(vec![9], vec![]).unwrap();
    let instructions = vec![Instruction::PushCell(Some(cell.into()))];
    let script = Script::Transparent(instructions.clone());
    let encoded = script.to_cell().unwrap();
    let decoded = Script::from_cell(&encoded, &mut ()).unwrap();
    assert_eq!(decoded.to_cell().unwrap().id(), encoded.id());
    assert!(matches!(
        String::script(instructions).into_script(),
        Err(VMError::Cell(CellError::InvalidFormat))
    ));
}

#[test]
fn log_reads_a_full_payload_cell_but_does_not_ignore_child_refs() {
    let bytes = vec![9; 4095];
    let script = ScriptBuilder::new()
        .push_cell(Cell::new(bytes.clone(), vec![]).unwrap())
        .log();
    let tx = script
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap()
        .without_signature()
        .unwrap();
    let decoded =
        crate::ExternalTx::from_bytes_bounded(&tx.to_bytes().unwrap(), 1, 4095, 2497).unwrap();
    assert!(
        matches!(&decoded.verify(crate::Limits { gas: 1_000_000 }).unwrap().entries()[1], TxEntry::Data(actual) if *actual == bytes)
    );
    let parent = Cell::new(vec![9], vec![Cell::new(vec![], vec![]).unwrap().into()]).unwrap();
    let mut vm = native_vm(
        ScriptBuilder::new()
            .push_cell(parent)
            .log()
            .to_cell()
            .unwrap(),
    );
    assert!(matches!(
        run_to_end(&mut vm),
        Err(VMError::Cell(CellError::InvalidFormat))
    ));
}
