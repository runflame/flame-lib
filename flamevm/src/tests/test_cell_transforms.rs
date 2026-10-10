//! In-place payload transforms and factual commitment equality.
use super::test_helpers::*;

fn cell(bytes: &[u8], refs: &[CellRef]) -> Cell {
    Cell::new(bytes.to_vec(), refs.to_vec()).unwrap()
}

fn builder(bytes: &[u8], refs: &[CellRef]) -> CellBuilder {
    let mut builder = CellBuilder::new();
    builder.store_bytes(bytes).unwrap();
    for reference in refs {
        builder.store_ref(reference.clone()).unwrap();
    }
    builder
}

fn run(op: Instruction, stack: Vec<Value>) -> Result<VM, VMError> {
    let mut program = ScriptBuilder::new();
    program.push_instr(op);
    let mut vm = vm_with_script(program);
    vm.current_call.stack = stack;
    run_to_end(&mut vm)?;
    Ok(vm)
}

fn slice(bytes: &[u8], refs: &[CellRef]) -> Value {
    Value::Slice(crate::Slice::new(Arc::new(cell(bytes, refs))).unwrap())
}

#[test]
fn bit_transforms_reuse_the_buffer_and_preserve_destination_refs() {
    let child: CellRef = cell(&[9], &[]).into();
    for (op, expected) in [
        (Instruction::BitOr, [0xaf, 0xff]),
        (Instruction::BitAnd, [0, 0]),
        (Instruction::BitXor, [0xaf, 0xff]),
    ] {
        for use_slice in [false, true] {
            let destination = builder(&[0xa0, 0x0f], std::slice::from_ref(&child));
            let pointer = destination.payload().as_ptr();
            let source = if use_slice {
                let mut source = crate::Slice::new(Arc::new(cell(&[0, 0x0f, 0xf0], &[]))).unwrap();
                source.advance_bytes(1);
                Value::Slice(source)
            } else {
                Value::Cell(cell(&[0x0f, 0xf0], &[]).into())
            };
            let vm = run(op.clone(), vec![Value::Builder(destination), source]).unwrap();
            let Value::Builder(result) = &vm.current_call.stack[0] else {
                panic!()
            };
            assert_eq!(pointer, result.payload().as_ptr());
            assert_eq!(result.payload(), expected);
            assert_eq!(result.refs()[0].id(), child.id());
        }
    }
    let destination = builder(&[0, 255], std::slice::from_ref(&child));
    let pointer = destination.payload().as_ptr();
    let vm = run(Instruction::BitNot, vec![Value::Builder(destination)]).unwrap();
    let Value::Builder(result) = &vm.current_call.stack[0] else {
        panic!()
    };
    assert_eq!(pointer, result.payload().as_ptr());
    assert_eq!(result.payload(), [255, 0]);
    assert_eq!(result.refs()[0].id(), child.id());
}

#[test]
fn bit_operations_check_operand_types_lengths_and_source_refs() {
    assert!(matches!(
        run(
            Instruction::BitNot,
            vec![Value::Cell(cell(&[0], &[]).into())]
        ),
        Err(VMError::TypeNotBuilder)
    ));
    for source in [Value::Cell(cell(&[0, 0], &[]).into()), slice(&[0, 0], &[])] {
        assert!(matches!(
            run(
                Instruction::BitOr,
                vec![Value::Builder(builder(&[0], &[])), source]
            ),
            Err(VMError::BitwiseSizeMismatch)
        ));
    }
    assert!(matches!(
        run(
            Instruction::BitAnd,
            vec![
                Value::Builder(builder(&[0], &[])),
                Value::Builder(builder(&[0], &[]))
            ]
        ),
        Err(VMError::TypeNotCellOrSlice)
    ));
    let child: CellRef = cell(&[], &[]).into();
    assert!(matches!(
        run(
            Instruction::BitXor,
            vec![Value::Builder(builder(&[0], &[])), slice(&[0], &[child])]
        ),
        Err(VMError::Cell(CellError::InvalidFormat))
    ));
}

#[test]
fn in_place_shifts_match_a_bitwise_reference_for_all_supported_counts() {
    let reference: CellRef = cell(&[], &[]).into();
    for len in [0, 1, 2, 31, 32, 65] {
        let bytes: Vec<u8> = (0..len).map(|index| (index * 73 + 19) as u8).collect();
        for count in 0..=256 {
            for left in [false, true] {
                let mut expected = vec![0; len];
                for index in 0..8 * len {
                    let source = if left {
                        index.checked_add(count)
                    } else {
                        index.checked_sub(count)
                    };
                    if let Some(source) = source.filter(|source| *source < 8 * len) {
                        expected[index / 8] |=
                            ((bytes[source / 8] >> (7 - source % 8)) & 1) << (7 - index % 8);
                    }
                }
                let mut destination = builder(&bytes, std::slice::from_ref(&reference));
                let pointer = destination.payload().as_ptr();
                shift_bytes(destination.payload_mut(), count, left);
                assert_eq!(pointer, destination.payload().as_ptr());
                assert_eq!(
                    destination.payload(),
                    expected,
                    "len={len}, count={count}, left={left}"
                );
                assert_eq!(destination.refs()[0].id(), reference.id());
            }
        }
    }
}

#[test]
fn factual_equality_is_cross_container_and_uses_unread_slice_streams() {
    let first: CellRef = cell(&[9], &[]).into();
    let last: CellRef = cell(&[8], &[]).into();
    let expected = cell(&[1, 2], std::slice::from_ref(&last));
    let mut remainder =
        crate::Slice::new(Arc::new(cell(&[0, 1, 2], &[first, last.clone()]))).unwrap();
    remainder.advance_bytes(1);
    remainder.advance_refs(1);
    let variants = [
        Value::Cell(expected.clone().into()),
        Value::Slice(remainder),
        Value::Builder(builder(&[1, 2], std::slice::from_ref(&last))),
    ];
    for left in &variants {
        for right in &variants {
            let vm = run(Instruction::Eq, vec![left.clone(), right.clone()]).unwrap();
            assert_eq!(vm.current_call.stack.len(), 3);
            assert_int(&vm.current_call.stack[2], Scalar::ONE);
        }
    }
    assert!(!variants[2]
        .try_eq(&Value::Builder(builder(&[1, 2], &[])))
        .unwrap());
    let mut changed = builder(&[1, 2], &[last]);
    changed.payload_mut()[0] = 7;
    assert!(!variants[0].try_eq(&Value::Builder(changed)).unwrap());
}

#[test]
fn pruning_changes_eq_but_explicit_level_hashes_can_compare_semantic_content() {
    let child = cell(&[9], &[]);
    let full = cell(&[1], &[child.clone().into()]);
    let partial = cell(&[1], &[child.prune(1).unwrap().into()]);
    assert_eq!(full.hash(0).unwrap(), partial.hash(0).unwrap());
    let vm = run(
        Instruction::Eq,
        vec![
            Value::Cell(full.clone().into()),
            Value::Cell(partial.clone().into()),
        ],
    )
    .unwrap();
    assert_int(&vm.current_call.stack[2], Scalar::ZERO);
    for (source, level, expected) in [
        (full, 0, partial.hash(0).unwrap()),
        (partial.clone(), 1, partial.id()),
    ] {
        let reference: CellRef = source.into();
        let vm = run(
            Instruction::CellHash,
            vec![
                Value::Cell(reference.to_unloaded().into()),
                Value::Scalar(Scalar::from(level as u64)),
                Value::Builder(builder(&[7], &[])),
            ],
        )
        .unwrap();
        let mut bytes = vec![7];
        bytes.extend_from_slice(&expected);
        assert_str(&vm.current_call.stack[0], &bytes);
    }
    assert!(matches!(
        run(
            Instruction::CellHash,
            vec![
                Value::Cell(partial.into()),
                Value::Scalar(Scalar::from(16u64)),
                Value::Builder(builder(&[], &[]))
            ]
        ),
        Err(VMError::IndexOutOfRange)
    ));
}

#[test]
fn size_peeks_payload_counts_and_dictionary_entries_without_flattening() {
    let child: CellRef = cell(&[9; 100], &[]).into();
    for value in [
        Value::Cell(cell(&[1, 2], std::slice::from_ref(&child)).into()),
        slice(&[1, 2], std::slice::from_ref(&child)),
        Value::Builder(builder(&[1, 2], &[child])),
    ] {
        let vm = run(Instruction::Size, vec![value]).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Scalar::from(2u64));
    }
    let vm = run(
        Instruction::Size,
        vec![Value::Dict(Dict::from_values(vec![Value::Scalar(
            Scalar::ONE,
        )]))],
    )
    .unwrap();
    assert_int(&vm.current_call.stack[1], Scalar::ONE);
    assert!(matches!(
        run(
            Instruction::Size,
            vec![Value::String(String::from(vec![1]))]
        ),
        Err(VMError::TypeHasNoLength)
    ));
}

#[test]
fn hash_sources_and_destinations_are_byte_oriented_and_capacity_checked() {
    use sha2::Digest;
    let child: CellRef = cell(&[], &[]).into();
    for (op, expected) in [
        (Instruction::Sha256, sha2::Sha256::digest(b"abc").to_vec()),
        (Instruction::Sha512, sha2::Sha512::digest(b"abc").to_vec()),
        (Instruction::Sha3, sha3::Sha3_256::digest(b"abc").to_vec()),
        (
            Instruction::Keccak256,
            sha3::Keccak256::digest(b"abc").to_vec(),
        ),
    ] {
        let mut remainder = crate::Slice::new(Arc::new(cell(b"xabc", &[]))).unwrap();
        remainder.advance_bytes(1);
        for source in [
            Value::Cell(cell(b"abc", &[]).into()),
            Value::Slice(remainder),
            Value::Builder(builder(b"abc", &[])),
        ] {
            let mut bytes = vec![7];
            bytes.extend_from_slice(&expected);
            let vm = run(
                op.clone(),
                vec![
                    source,
                    Value::Builder(builder(&[7], std::slice::from_ref(&child))),
                ],
            )
            .unwrap();
            assert_str(&vm.current_call.stack[0], &bytes);
            let Value::Builder(destination) = &vm.current_call.stack[0] else {
                panic!()
            };
            assert_eq!(destination.refs()[0].id(), child.id());
        }
        let full = builder(&vec![0; cells::MAX_CELL_PAYLOAD - expected.len() + 1], &[]);
        assert!(matches!(
            run(op, vec![slice(b"abc", &[]), Value::Builder(full)]),
            Err(VMError::Cell(CellError::PayloadCapacity))
        ));
    }
    assert!(matches!(
        run(
            Instruction::Sha256,
            vec![slice(b"abc", &[child]), Value::Builder(builder(&[], &[]))]
        ),
        Err(VMError::Cell(CellError::InvalidFormat))
    ));
}

#[test]
fn public_transform_execution_has_identical_prover_and_verifier_gas() {
    let expected = cell(&[0xfe], &[]);
    let program = ScriptBuilder::new()
        .builder()
        .push_int(1u64)
        .push_int(1u64)
        .write_uint()
        .bit_not()
        .push_cell(expected)
        .eq()
        .verify()
        .drop_()
        .endcell()
        .builder()
        .sha256()
        .endcell()
        .drop_();
    let unsigned = program
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
}
