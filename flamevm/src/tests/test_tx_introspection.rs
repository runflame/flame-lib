//! Tests for transaction, frame, storage, and block introspection.

use super::test_helpers::*;
use crate::vm::LOCKTIME_TIMESTAMP_THRESHOLD;
use crate::{empty_state, ActorID, ActorRegistry, Int253, StoragePurchase};

fn external_vm(header: TxHeader, script: Vec<u8>, gas: u64) -> VM {
    VM::new(
        header,
        CallFrame::new(
            ScriptBuilder::parse(&script).unwrap().into_instructions(),
            CallKind::ExternalRoot,
            gas,
        ),
    )
}

#[test]
fn timelock_reports_bip65_kind() {
    for (locktime, expected_flag) in [(800_000, 0u64), (LOCKTIME_TIMESTAMP_THRESHOLD, 1u64)] {
        let mut vm = external_vm(
            TxHeader {
                version: 1,
                locktime,
            },
            ScriptBuilder::new().timelock().to_bytecode(),
            1_000_000,
        );
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(locktime as u64));
        assert_int(&vm.current_call.stack[1], Int253::from(expected_flag));
    }
}

#[test]
fn version_and_gas_limit_report_frame_values() {
    let script = ScriptBuilder::new()
        .version()
        .gaslimit()
        .to_bytecode();
    let mut vm = external_vm(
        TxHeader {
            version: 42,
            locktime: 0,
        },
        script,
        99_999,
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(42u64));
    assert_int(&vm.current_call.stack[1], Int253::from(99_999u64));
}

#[test]
fn gas_reports_remaining_budget_after_its_own_cost() {
    let mut vm = external_vm(
        dummy_header(),
        ScriptBuilder::new().gas().to_bytecode(),
        12_345,
    );
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::from(12_344u64));
}

#[test]
fn usage_and_capacity_read_actor_storage() {
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0xab; 32]);
    reg.deploy(
        id.clone(),
        ScriptBuilder::new().nop().to_bytecode(),
        empty_state(),
        12_345,
    )
    .unwrap();
    let expected_usage = reg.actor_usage(&id).unwrap();
    let kind = CallKind::InternalRoot {
        actor: id,
        caller: None,
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            vec![
                Instruction::Usage,
                Instruction::PushInt(Int253::from(100u64)),
                Instruction::Capacity,
            ],
            kind,
            1_000_000,
        )
        .with_anchor(Anchor([0u8; 32])),
    );
    for _ in 0..3 {
        vm.step_internal_with_registry(&mut reg).unwrap();
    }
    assert_int(&vm.current_call.stack[0], Int253::from(expected_usage));
    assert_int(&vm.current_call.stack[1], Int253::from(12_345u64));
}

#[test]
fn storage_introspection_requires_registry() {
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0xab; 32]),
        caller: None,
    };
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(vec![Instruction::Usage], kind, 1_000_000)
            .with_anchor(Anchor([0u8; 32])),
    );
    assert!(matches!(
        vm.step_internal(),
        Err(VMError::RegistryUnavailable)
    ));
}

#[test]
fn height_is_zero_for_external_transaction() {
    let mut vm = vm_with_script(ScriptBuilder::new().height().to_bytecode());
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[0], Int253::ZERO);
}

#[test]
fn storage_quote_and_purchase_use_host_result() {
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0xcd; 32]);
    reg.deploy(
        id.clone(),
        ScriptBuilder::new().nop().to_bytecode(),
        empty_state(),
        2_048,
    )
    .unwrap();
    reg.set_storage_quote(Some(StoragePurchase {
        fee_sparks: Int253::from(77u64),
        expiry_height: 52_500,
    }));

    let kind = CallKind::InternalRoot {
        actor: id.clone(),
        caller: None,
    };
    let mut quote_vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::new()
                .push_int(1_024u64)
                .quotestorage()
                .into_instructions(),
            CallKind::InternalRoot {
                actor: id.clone(),
                caller: None,
            },
            1_000_000,
        )
        .with_anchor(Anchor([0u8; 32])),
    );
    quote_vm.step_internal_with_registry(&mut reg).unwrap();
    quote_vm.step_internal_with_registry(&mut reg).unwrap();
    assert_int(&quote_vm.current_call.stack[0], Int253::from(77u64));
    assert_int(&quote_vm.current_call.stack[1], Int253::ONE);

    let mut buy_vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::new()
                .push_int(1_024u64)
                .addstorage()
                .into_instructions(),
            kind,
            1_000_000,
        )
        .with_anchor(Anchor([0u8; 32])),
    );
    buy_vm.step_internal_with_registry(&mut reg).unwrap();
    buy_vm.step_internal_with_registry(&mut reg).unwrap();
    match &buy_vm.current_call.stack[0] {
        Value::ClearToken(token) => {
            assert_eq!(token.qty(), Int253::from(-77i64));
            assert_eq!(token.flv(), FLAME_FLAVOR);
        }
        value => panic!("expected storage debt, got {:?}", value),
    }
    assert_int(&buy_vm.current_call.stack[1], Int253::ONE);
    assert_eq!(reg.actor_capacity(&id, 0).unwrap(), 3_072);
    assert!(buy_vm.txlog.iter().any(|entry| matches!(
        entry,
        TxEntry::StoragePurchase {
            actor,
            bytes: 1_024,
            expiry_height: 52_500,
            fee_sparks,
        } if actor == &id && *fee_sparks == Int253::from(77u64)
    )));
}
