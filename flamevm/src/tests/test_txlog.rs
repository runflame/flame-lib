//! Tests for txlog.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn log_opcode_emits_txentry_data() {
    // pushstr "hello", log → txlog has Header + TxEntry::Data(b"hello").
    let script = ScriptBuilder::new()
        .push_str(String::from(b"hello".to_vec()))
        .log()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).expect("log ok");
    assert!(vm.current_call.stack.is_empty());
    assert_eq!(vm.txlog.len(), 2);
    assert!(matches!(vm.txlog[0], crate::tx::TxEntry::Header(_)));
    match &vm.txlog[1] {
        crate::tx::TxEntry::Data(bytes) => assert_eq!(bytes, b"hello"),
        _ => panic!("expected Data entry"),
    }
}

#[test]
fn log_opcode_requires_string() {
    // push:5, log — top is Int253 not String.
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(5u64).log().to_bytecode());
    let err = run_to_end(&mut vm).unwrap_err();
    assert!(matches!(err, VMError::TypeNotString));
}

