//! Builder/Slice byte and reference operations.
use super::test_helpers::*;

fn bytes(data: &[u8]) -> Cell {
    Cell::new(data.to_vec(), vec![]).unwrap()
}

fn builder_value(data: &[u8], refs: &[CellRef]) -> Value {
    let mut builder = CellBuilder::new();
    builder.store_bytes(data).unwrap();
    for reference in refs {
        builder.store_ref(reference.clone()).unwrap();
    }
    Value::Builder(builder)
}

fn operation(instruction: Instruction, stack: Vec<Value>) -> VM {
    let mut program = ScriptBuilder::new();
    program.push_instr(instruction);
    let mut vm = vm_with_script(program);
    vm.current_call.stack = stack;
    vm
}

fn slice_value(cell: Cell) -> Value {
    Value::Slice(crate::Slice::new(Arc::new(cell)).unwrap())
}

#[test]
fn unsigned_reads_and_writes_use_byte_widths_including_zero_and_32() {
    for count in 0..=32 {
        let value = Scalar::from(0x12345678u64);
        let program = ScriptBuilder::new()
            .builder()
            .push_int(value)
            .push_int(count as u64)
            .write_uint()
            .endcell()
            .slice()
            .push_int(count as u64)
            .read_uint();
        let mut vm = vm_with_script(program);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[]);
        let mut expected = [0; 32];
        expected[..count].copy_from_slice(&value.to_bytes()[..count]);
        assert_int(
            &vm.current_call.stack[1],
            Scalar::from_bytes(expected).unwrap(),
        );
        assert_int(&vm.current_call.stack[2], Scalar::ONE);
    }
    for value in [Scalar::ZERO, Scalar::ONE, Scalar::from(-1i64)] {
        let mut vm = vm_with_script(
            ScriptBuilder::new()
                .builder()
                .push_int(value)
                .write_scalar()
                .endcell()
                .slice()
                .read_scalar(),
        );
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], value);
    }
}

#[test]
fn scalar_reads_soft_fail_without_advancing_on_short_or_noncanonical_input() {
    let mut modulus = Scalar::from(-1i64).to_bytes();
    modulus[0] += 1;
    for data in [vec![0; 31], modulus.to_vec(), vec![255; 32]] {
        let mut vm = operation(Instruction::ReadScalar, vec![slice_value(bytes(&data))]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_str(&vm.current_call.stack[0], &data);
        assert_int(&vm.current_call.stack[1], Scalar::ZERO);
    }
    for instruction in [Instruction::ReadUint, Instruction::WriteUint] {
        for count in [Scalar::from(33u64), Scalar::from(-1i64)] {
            let mut vm = operation(instruction.clone(), vec![Value::Scalar(count)]);
            assert!(matches!(run_to_end(&mut vm), Err(VMError::IndexOutOfRange)));
        }
    }
}

#[test]
fn readbytes_copies_requested_bytes_and_preserves_refs_and_failure_operands() {
    let child = bytes(&[9]);
    let source = Cell::new(vec![1, 2, 3], vec![child.into()]).unwrap();
    let mut vm = operation(
        Instruction::ReadBytes,
        vec![
            slice_value(source.clone()),
            builder_value(&[8], &[]),
            Value::Scalar(Scalar::from(2u64)),
        ],
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[3]);
    let Value::Slice(slice) = &vm.current_call.stack[0] else {
        panic!()
    };
    assert_eq!(slice.remaining_refs(), 1);
    let Value::Builder(builder) = &vm.current_call.stack[1] else {
        panic!()
    };
    assert_eq!(builder.clone().build().payload(), &[8, 1, 2]);
    assert_int(&vm.current_call.stack[2], Scalar::ONE);

    let mut vm = operation(
        Instruction::ReadBytes,
        vec![
            slice_value(source),
            builder_value(&[8], &[]),
            Value::Scalar(Scalar::from(4u64)),
        ],
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[1, 2, 3]);
    let Value::Builder(builder) = &vm.current_call.stack[1] else {
        panic!()
    };
    assert_eq!(builder.clone().build().payload(), &[8]);
    assert_int(&vm.current_call.stack[2], Scalar::ZERO);
}

#[test]
fn append_streams_are_independent_and_refs_do_not_resolve_children() {
    let target = bytes(&[9]);
    let pruned = target.prune(1).unwrap();
    let refs = vec![CellRef::from(target).to_unloaded(), pruned.into()];
    let source = Cell::new(vec![1, 2], refs.clone()).unwrap();
    for refs_first in [false, true] {
        let mut program = ScriptBuilder::new()
            .push_cell(source.clone())
            .slice()
            .builder();
        program = if refs_first {
            program.append_refs().append_bytes()
        } else {
            program.append_bytes().append_refs()
        };
        let mut vm = vm_with_script(program.endcell());
        run_to_end(&mut vm).unwrap();
        let Value::Slice(slice) = &vm.current_call.stack[0] else {
            panic!()
        };
        assert_eq!((slice.remaining_bytes(), slice.remaining_refs()), (0, 0));
        let Value::Cell(result) = &vm.current_call.stack[1] else {
            panic!()
        };
        assert_eq!(result.id(), source.id());
        assert!(result.as_resident().unwrap().refs()[0].is_unloaded());
    }
}

#[test]
fn byte_and_reference_overflow_hard_fail_without_implicit_spill() {
    let full = vec![0; cells::MAX_CELL_PAYLOAD];
    for instruction in [Instruction::AppendBytes, Instruction::ReadBytes] {
        let mut stack = vec![slice_value(bytes(&[1])), builder_value(&full, &[])];
        if matches!(instruction, Instruction::ReadBytes) {
            stack.push(Value::Scalar(Scalar::ONE));
        }
        assert!(matches!(
            run_to_end(&mut operation(instruction, stack)),
            Err(VMError::Cell(CellError::PayloadCapacity))
        ));
    }
    assert!(matches!(
        run_to_end(&mut operation(
            Instruction::WriteScalar,
            vec![builder_value(&full, &[]), Value::Scalar(Scalar::ZERO)]
        )),
        Err(VMError::Cell(CellError::PayloadCapacity))
    ));
    let reference: CellRef = bytes(&[]).into();
    let four = vec![reference.clone(); 4];
    let source = Cell::new(vec![], vec![reference]).unwrap();
    assert!(matches!(
        run_to_end(&mut operation(
            Instruction::AppendRefs,
            vec![slice_value(source), builder_value(&[], &four)]
        )),
        Err(VMError::Cell(CellError::ReferenceCapacity))
    ));
}

#[test]
fn cells_and_slices_dup_but_builders_are_transient_and_not_copyable() {
    let mut vm = vm_with_script(
        ScriptBuilder::new()
            .push_cell(bytes(&[7, 8]))
            .dup_k(0)
            .slice()
            .dup_k(0)
            .push_int(1u64)
            .read_uint(),
    );
    run_to_end(&mut vm).unwrap();
    assert_str(&vm.current_call.stack[0], &[7, 8]);
    assert_str(&vm.current_call.stack[1], &[7, 8]);
    assert_str(&vm.current_call.stack[2], &[8]);
    assert!(vm.current_call.stack[0].is_copyable());
    assert!(!vm.current_call.stack[1].is_portable());
    let builder = builder_value(&[], &[]);
    assert!(builder.is_droppable());
    assert!(!builder.is_copyable());
    assert!(!builder.is_portable());
    let mut vm = vm_with_script(ScriptBuilder::new().builder().dup_k(0));
    assert!(matches!(run_to_end(&mut vm), Err(VMError::TypeNotCopyable)));
}

#[test]
fn slice_access_rejects_missing_or_pruned_bodies_and_old_pushstr_is_not_an_opcode() {
    let cell = bytes(&[1]);
    for reference in [
        CellRef::from(cell.clone()).to_unloaded(),
        cell.prune(1).unwrap().into(),
    ] {
        let mut vm = vm_with_script(ScriptBuilder::new().push_cell(reference).slice());
        assert!(matches!(
            run_to_end(&mut vm),
            Err(VMError::Cell(
                CellError::MissingCell(_) | CellError::PrunedCell
            ))
        ));
    }
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::from_cell(
            bytes(&[0x19]),
            CallKind::InternalRoot {
                actor: ActorID::Hash([0; 32]),
                caller: None,
            },
            1_000_000,
        )
        .unwrap(),
    );
    assert!(matches!(
        run_to_end(&mut vm),
        Err(VMError::UnknownOpcode(0x19))
    ));
    let program = ScriptBuilder::new().push_str(String::from(vec![0; cells::MAX_CELL_PAYLOAD]));
    let root = program.to_cell().unwrap();
    assert_eq!(root.payload(), &[0xa6]);
    assert_eq!(
        root.refs()[0].as_resident().unwrap().payload().len(),
        cells::MAX_CELL_PAYLOAD
    );
}

#[test]
fn public_cell_operation_execution_has_identical_prover_and_verifier_gas() {
    let program = ScriptBuilder::new()
        .builder()
        .push_int(513u64)
        .push_int(2u64)
        .write_uint()
        .endcell()
        .slice()
        .builder()
        .push_int(2u64)
        .read_bytes()
        .verify()
        .endcell()
        .slice()
        .push_int(2u64)
        .read_uint()
        .verify()
        .push_int(513u64)
        .eq()
        .verify()
        .drop_()
        .drop_()
        .drop_()
        .drop_();
    let unsigned = program
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap();
    let gas = unsigned.metrics().gas_used;
    let tx = unsigned.without_signature().unwrap();
    let decoded =
        crate::ExternalTx::from_bytes_bounded(&tx.to_bytes().unwrap(), 1, 4095, 2497).unwrap();
    let (_, metrics) = decoded
        .verify_with_metrics(crate::Limits { gas: 1_000_000 })
        .unwrap();
    assert_eq!(metrics.gas_used, gas);
}

#[test]
fn cursor_and_builder_values_can_return_upward_but_cannot_be_call_arguments() {
    for value in [slice_value(bytes(&[7])), builder_value(&[7], &[])] {
        assert!(matches!(
            VM::require_portable_call_args(&[value]),
            Err(VMError::NonPortableInCall)
        ));
    }
    let branch = ScriptBuilder::new()
        .builder()
        .push_int(7u64)
        .push_int(1u64)
        .write_uint()
        .push_int(1u64)
        .return_();
    let mut registry = StubRegistry {
        script: branch.to_bytecode(),
    };
    let mut vm = vm_with_script(call_to(&ActorID::Hash([9; 32])).verify().drop_());
    vm.last_anchor = Some(Anchor([3; 32]));
    while !vm.current_call.is_finished() || !vm.call_stack.is_empty() {
        vm.step_internal_with_registry(&mut registry).unwrap();
    }
    let Value::Builder(builder) = &vm.current_call.stack[0] else {
        panic!()
    };
    assert_eq!(builder.clone().build().payload(), &[7]);
}

#[test]
fn byte_copy_is_charged_before_mutation_and_zero_length_read_preserves_streams() {
    let source = Cell::new(vec![7; 100], vec![bytes(&[]).into()]).unwrap();
    let mut vm = operation(
        Instruction::ReadBytes,
        vec![
            slice_value(source),
            builder_value(&[9], &[]),
            Value::Scalar(Scalar::ZERO),
        ],
    );
    run_to_end(&mut vm).unwrap();
    let Value::Slice(slice) = &vm.current_call.stack[0] else {
        panic!()
    };
    assert_eq!((slice.remaining_bytes(), slice.remaining_refs()), (100, 1));
    let Value::Builder(builder) = &vm.current_call.stack[1] else {
        panic!()
    };
    assert_eq!(builder.clone().build().payload(), &[9]);

    let mut vm = operation(
        Instruction::AppendBytes,
        vec![slice_value(bytes(&[7; 100])), builder_value(&[], &[])],
    );
    vm.current_call.gas_limit = 50;
    assert!(matches!(run_to_end(&mut vm), Err(VMError::OutOfGas)));
}

#[test]
fn equal_cell_bytes_do_not_overwrite_per_value_private_annotations() {
    let program = ScriptBuilder::new()
        .push_str(String::scalar(Scalar::ZERO))
        .push_str(String::point(Point::from_bytes([0; 32])))
        .drop_()
        .scalar()
        .push_int(0u64)
        .eq()
        .verify();
    let tx = program
        .build_tx(dummy_header(), crate::Limits { gas: 1_000_000 })
        .unwrap()
        .without_signature()
        .unwrap();
    tx.verify(crate::Limits { gas: 1_000_000 }).unwrap();
}
