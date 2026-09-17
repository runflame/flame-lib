//! Context-admission matrix for every opcode with a global context gate.

use super::test_helpers::*;

#[derive(Clone, Copy, Debug)]
enum Context {
    ExternalRoot,
    InternalRoot,
    ActorCall,
    ExternalContractOpen,
    InternalContractOpen,
}

const CONTEXTS: [Context; 5] = [
    Context::ExternalRoot,
    Context::InternalRoot,
    Context::ActorCall,
    Context::ExternalContractOpen,
    Context::InternalContractOpen,
];

#[derive(Clone, Copy)]
enum Gate {
    External,
    Actor,
    ExternalPredicate,
}

fn kind(context: Context) -> CallKind {
    let actor = ActorID::Hash([0x11; 32]);
    let caller = ActorID::Hash([0x22; 32]);
    match context {
        Context::ExternalRoot => CallKind::ExternalRoot,
        Context::InternalRoot => CallKind::InternalRoot {
            actor,
            caller: Some(caller),
        },
        Context::ActorCall => CallKind::ActorCall { actor, caller },
        Context::ExternalContractOpen => CallKind::ContractOpen {
            predicate: Predicate::opaque(Predicate::unspendable_key()),
            external_context: true,
            caller_id: None,
        },
        Context::InternalContractOpen => CallKind::ContractOpen {
            predicate: Predicate::opaque(Predicate::unspendable_key()),
            external_context: false,
            caller_id: Some([0x22; 32]),
        },
    }
}

fn is_external(context: Context) -> bool {
    matches!(
        context,
        Context::ExternalRoot | Context::ExternalContractOpen
    )
}

fn is_actor(context: Context) -> bool {
    matches!(context, Context::InternalRoot | Context::ActorCall)
}

fn step(context: Context, instruction: Instruction) -> Result<bool, VMError> {
    let frame =
        CallFrame::new(vec![instruction], kind(context), 1_000_000).with_anchor(Anchor([0x33; 32]));
    let mut vm = VM::new(dummy_header(), frame);
    if is_external(context) {
        vm.step_external(&mut make_stub_delegate())
    } else {
        vm.step_internal_with_registry(&mut MemRegistry::new())
    }
}

fn assert_gate(name: &str, instruction: Instruction, gate: Gate) {
    for context in CONTEXTS {
        let allowed = match gate {
            Gate::External => is_external(context),
            Gate::Actor => is_actor(context),
            Gate::ExternalPredicate => matches!(context, Context::ExternalContractOpen),
        };
        let result = step(context, instruction.clone());
        if allowed {
            if let Err(error) = result {
                assert!(
                    !matches!(
                        error,
                        VMError::ExternalOnly
                            | VMError::OpcodeRequiresActorContext
                            | VMError::OpcodeRequiresPredicateContext
                    ),
                    "{} unexpectedly rejected {:?}: {:?}",
                    name,
                    context,
                    error,
                );
            }
            continue;
        }

        let error = result.expect_err("disallowed context must fail");
        let expected = match gate {
            Gate::External => matches!(error, VMError::ExternalOnly),
            Gate::Actor => matches!(error, VMError::OpcodeRequiresActorContext),
            Gate::ExternalPredicate if matches!(context, Context::InternalContractOpen) => {
                matches!(error, VMError::ExternalOnly)
            }
            Gate::ExternalPredicate => {
                matches!(error, VMError::OpcodeRequiresPredicateContext)
            }
        };
        assert!(
            expected,
            "{} accepted wrong error in {:?}: {:?}",
            name, context, error,
        );
    }
}

#[test]
fn restricted_opcode_context_matrix() {
    for (name, instruction) in [
        ("scalar", Instruction::Scalar),
        ("commit", Instruction::Commit),
        ("alloc", Instruction::Alloc(None)),
        ("expr", Instruction::Expr),
        ("input", Instruction::Input),
        ("signtx", Instruction::Signtx),
        ("mix", Instruction::Mix),
        ("fee", Instruction::Fee),
    ] {
        assert_gate(name, instruction, Gate::External);
    }

    assert_gate("issuepriv", Instruction::IssuePriv, Gate::ExternalPredicate);

    for (name, instruction) in [
        ("issuepub", Instruction::IssuePub),
        ("call", Instruction::Call),
        ("load", Instruction::Load),
        ("save", Instruction::Save),
        ("setcode", Instruction::Setcode),
        ("addstorage", Instruction::AddStorage),
        ("quotestorage", Instruction::QuoteStorage),
        ("selfid", Instruction::Selfid),
        ("usage", Instruction::Usage),
        ("capacity", Instruction::Capacity),
    ] {
        assert_gate(name, instruction, Gate::Actor);
    }
}

#[test]
fn raw_scalar_range_checks_in_every_context_and_preserves_type() {
    let cases = [
        (Scalar::ZERO, 1u64, None),
        (Scalar::ONE, 1, None),
        (Scalar::from(255u64), 8, None),
        (Scalar::from(u64::MAX), 64, None),
        (Scalar::from(256u64), 8, Some(VMError::InvalidBitrange)),
        (
            Scalar::from(1u128 << 64),
            64,
            Some(VMError::InvalidBitrange),
        ),
        (-Scalar::ONE, 64, Some(VMError::InvalidBitrange)),
        (Scalar::ZERO, 0, Some(VMError::BitCountOutOfRange)),
        (Scalar::ZERO, 65, Some(VMError::BitCountOutOfRange)),
    ];
    for context in CONTEXTS {
        for (value, bits, expected_error) in &cases {
            let frame = CallFrame::new(vec![Instruction::Range], kind(context), 1_000_000);
            let mut vm = VM::new(dummy_header(), frame);
            vm.push_value(Value::Scalar(*value));
            vm.push_value(Value::Scalar(Scalar::from(*bits)));
            let result = if is_external(context) {
                vm.step_external(&mut make_stub_delegate())
            } else {
                vm.step_internal_with_registry(&mut MemRegistry::new())
            };
            match expected_error {
                Some(error) => assert_eq!(
                    core::mem::discriminant(&result.unwrap_err()),
                    core::mem::discriminant(error),
                    "range({value:?}, {bits}) in {context:?}",
                ),
                None => {
                    result.unwrap();
                    assert_eq!(vm.current_call.stack.len(), 1);
                    assert_int(&vm.current_call.stack[0], *value);
                }
            }
        }
    }
}

#[test]
fn range_expression_context_matrix() {
    for context in CONTEXTS {
        let frame = CallFrame::new(vec![Instruction::Range], kind(context), 1_000_000);
        let mut vm = VM::new(dummy_header(), frame);
        vm.push_value(Value::Expression(Expression::Constant(Scalar::ONE)));
        vm.push_value(Value::Scalar(Scalar::from(64u64)));
        if is_external(context) {
            vm.step_external(&mut make_stub_delegate()).unwrap();
            assert!(matches!(
                vm.current_call.stack.as_slice(),
                [Value::Expression(Expression::Constant(value))] if *value == Scalar::ONE
            ));
        } else {
            let error = vm
                .step_internal_with_registry(&mut MemRegistry::new())
                .unwrap_err();
            assert!(matches!(error, VMError::ExternalOnly));
        }
    }
}

#[test]
fn callerid_reports_attribution_without_granting_actor_context() {
    for (context, expected) in [
        (Context::InternalRoot, [0x22; 32]),
        (Context::ActorCall, [0x22; 32]),
        (Context::ExternalContractOpen, [0; 32]),
        (Context::InternalContractOpen, [0x22; 32]),
    ] {
        let frame = CallFrame::new(vec![Instruction::Callerid], kind(context), 100);
        let mut vm = VM::new(dummy_header(), frame);
        if is_external(context) {
            vm.step_external(&mut make_stub_delegate()).unwrap();
        } else {
            vm.step_internal_with_registry(&mut MemRegistry::new())
                .unwrap();
        }
        assert_str(vm.current_call.stack.last().unwrap(), &expected);
    }

    assert!(matches!(
        step(Context::ExternalRoot, Instruction::Callerid),
        Err(VMError::OpcodeRequiresActorContext)
    ));
}

#[test]
fn nested_contractopen_does_not_transitively_inherit_actor_identity() {
    let constructor = ActorID::Constructor(vec![1, 2, 3]);
    let expected = constructor.to_hash();
    let frame = CallFrame::new(
        Vec::new(),
        CallKind::InternalRoot {
            actor: constructor,
            caller: None,
        },
        1_000,
    );
    let mut vm = VM::new(dummy_header(), frame);
    let make_contract = || {
        Contract::new(
            Predicate::opaque(Predicate::unspendable_key()),
            Anchor([0x44; 32]),
            test_payload(Vec::new()),
        )
        .unwrap()
    };

    vm.enter_contract_open_frame(
        make_contract(),
        Script::Transparent(Vec::new()),
        0,
        100,
        Vec::new(),
        Anchor([0x55; 32]),
    )
    .unwrap();
    assert!(matches!(
        vm.current_call.kind,
        CallKind::ContractOpen {
            caller_id: Some(id),
            ..
        } if id == expected
    ));

    vm.enter_contract_open_frame(
        make_contract(),
        Script::Transparent(Vec::new()),
        0,
        50,
        Vec::new(),
        Anchor([0x66; 32]),
    )
    .unwrap();
    assert!(matches!(
        vm.current_call.kind,
        CallKind::ContractOpen {
            caller_id: None,
            ..
        }
    ));
}

#[test]
fn contractopen_call_rejects_before_consuming_operands() {
    let frame = CallFrame::new(
        vec![Instruction::Call],
        CallKind::ContractOpen {
            predicate: Predicate::opaque(Predicate::unspendable_key()),
            external_context: false,
            caller_id: Some([0x22; 32]),
        },
        1_000,
    );
    let mut vm = VM::new(dummy_header(), frame);
    vm.current_call.stack = vec![
        Value::Scalar(Scalar::from(7u64)),
        Value::Scalar(Scalar::ONE),
        Value::Scalar(Scalar::from(100u64)),
        Value::String(String::from(vec![0x44; 32])),
    ];

    assert!(matches!(
        vm.step_internal_with_registry(&mut MemRegistry::new()),
        Err(VMError::OpcodeRequiresActorContext)
    ));
    assert_eq!(vm.current_call.stack.len(), 4);
    assert_int(&vm.current_call.stack[0], Scalar::from(7u64));
}

#[test]
fn issuepub_is_available_in_actorcall() {
    let actor = ActorID::Hash([0x11; 32]);
    let script = ScriptBuilder::new()
        .push_int(7u64)
        .push_str(String::from(b"tag".to_vec()))
        .issuepub()
        .to_bytecode();
    let frame = CallFrame::new(
        ScriptBuilder::parse(&script).unwrap().into_instructions(),
        CallKind::ActorCall {
            actor,
            caller: ActorID::Hash([0x22; 32]),
        },
        1_000,
    );
    let mut vm = VM::new(dummy_header(), frame);

    run_to_end(&mut vm).unwrap();

    assert!(matches!(
        vm.current_call.stack.as_slice(),
        [Value::ClearToken(_)]
    ));
    assert!(matches!(vm.txlog.last(), Some(TxEntry::IssuePub(_, _))));
}

#[test]
fn send_is_available_everywhere_but_contractopen_never_delegates_its_caller() {
    let script = ScriptBuilder::new()
        .push_int(0u64)
        .push_str(String::from(vec![0; 32]))
        .push_int(0u64)
        .push_str(String::from(vec![0x77; 32]))
        .send()
        .to_bytecode();

    for context in CONTEXTS {
        let frame = CallFrame::new(
            ScriptBuilder::parse(&script).unwrap().into_instructions(),
            kind(context),
            1_000,
        )
        .with_anchor(Anchor([0x33; 32]));
        let mut vm = VM::new(dummy_header(), frame);
        while !vm.current_call.is_finished() {
            if is_external(context) {
                vm.step_external(&mut make_stub_delegate()).unwrap();
            } else {
                vm.step_internal_with_registry(&mut MemRegistry::new())
                    .unwrap();
            }
        }

        let caller = vm.txlog.iter().find_map(|entry| match entry {
            TxEntry::Send(message) => Some(message.caller.as_ref()),
            _ => None,
        });
        match context {
            Context::InternalRoot | Context::ActorCall => {
                assert_eq!(caller.flatten().map(ActorID::to_hash), Some([0x11; 32]));
            }
            Context::ExternalRoot
            | Context::ExternalContractOpen
            | Context::InternalContractOpen => {
                assert_eq!(caller, Some(None))
            }
        }
    }
}
