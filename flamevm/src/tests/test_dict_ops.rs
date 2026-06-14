//! Tests for dict ops.

#![allow(unused_imports)]

use super::test_helpers::*;

#[test]
fn dict_construction_zero_pairs() {
    // push:0, dict — empty dict
    let mut vm = vm_with_script(ScriptBuilder::new().push_int(0u64).dict().to_bytecode());
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Dict(d) => assert!(d.is_empty()),
        other => panic!("expected Dict, got {}", value_kind(other)),
    }
}

#[test]
fn dict_construction_two_pairs() {
    // Build {5: 50, 1: 10}. Pairs popped top-first, so push:
    //   val_for_pair_2 (50), key_for_pair_2 (5),
    //   val_for_pair_1 (10), key_for_pair_1 (1),
    //   2, dict.
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64)
        .push_int(10u64).push_int(1u64)
        .push_int(2u64).dict()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_dict_keys(
        &vm.current_call.stack[0],
        &[Int253::from(1u64), Int253::from(5u64)],
    );
}

#[test]
fn dict_construction_duplicate_keys_errors() {
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64)      // (5, 50)
        .push_int(60u64).push_int(5u64)      // (5, 60) — duplicate!
        .push_int(2u64).dict()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DictKeyOccupied
    ));
}

#[test]
fn put_inserts_into_empty() {
    // push:0, dict (empty)  →  put k=3, v=99
    let script = ScriptBuilder::new()
        .push_int(0u64).dict()               // empty dict
        .push_int(3u64)                      // key
        .push_int(99u64)                     // value
        .put()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Dict(d) => {
            assert_eq!(d.len(), 1);
            match d.get(&Int253::from(3u64)) {
                Some(Value::Int253(i)) => assert_eq!(*i, Int253::from(99u64)),
                _ => panic!("expected Int253"),
            }
        }
        _ => panic!("expected Dict"),
    }
}

#[test]
fn put_on_occupied_key_errors() {
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()   // {5: 50}
        .push_int(5u64).push_int(99u64).put()                   // put k=5 → conflict
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DictKeyOccupied
    ));
}

#[test]
fn put_rejects_non_portable() {
    // `put`ting a Merlin (always non-portable) into a dict fails the
    // portability gate (todo #5), mirroring cell-payload rejection.
    let script = ScriptBuilder::new()
        .push_int(0u64).dict()                         // {}
        .push_int(0u64)                                // key
        .push_str(String::from(Vec::<u8>::new())).transcript()  // → Merlin
        .put()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::NonPortableInDict
    ));
}

#[test]
fn replace_existing_returns_prev() {
    // Build {5: 50}, then replace v at key 5 with 99.
    // Spec stack: dict k v → dict' {prev 1 | 0}
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()
        .push_int(5u64).push_int(99u64).replace()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[1], Int253::from(50u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn replace_absent_returns_zero() {
    let script = ScriptBuilder::new()
        .push_int(0u64).dict()                                  // empty
        .push_int(5u64).push_int(99u64).replace()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn get_existing_returns_dict_k_v() {
    // {5: 50}, get key 5.
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()
        .push_int(5u64).get()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[1], Int253::from(5u64));
    assert_int(&vm.current_call.stack[2], Int253::from(50u64));
    match &vm.current_call.stack[0] {
        Value::Dict(d) => assert!(d.is_empty()),
        _ => panic!("expected Dict"),
    }
}

#[test]
fn get_missing_errors() {
    let script = ScriptBuilder::new()
        .push_int(0u64).dict().push_int(5u64).get().to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DictKeyNotFound
    ));
}

#[test]
fn getopt_existing() {
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()
        .push_int(5u64).get_opt()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(50u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn getopt_missing() {
    let script = ScriptBuilder::new()
        .push_int(0u64).dict().push_int(5u64).get_opt().to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn getdup_copyable() {
    // {5: 50}; getdup k=5 → dict unchanged + 50 + 1
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()
        .push_int(5u64).get_dup()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[1], Int253::from(50u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    match &vm.current_call.stack[0] {
        Value::Dict(d) => assert_eq!(d.len(), 1),
        _ => panic!("expected Dict"),
    }
}

#[test]
fn getdup_missing_pushes_zero() {
    let script = ScriptBuilder::new()
        .push_int(0u64).dict().push_int(5u64).get_dup().to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn getdup_noncopyable_errors() {
    // {5: ClearToken(0, 7)}; getdup k=5 → TypeNotCopyable
    let script = ScriptBuilder::new()
        .push_int(7u64).pushtoken()              // value = ClearToken(0, flavor=7)
        .push_int(5u64)                          // key
        .push_int(1u64).dict()                   // dict
        .push_int(5u64).get_dup()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

/// Conservation: `getdup` of a key whose value is a *nested* Dict
/// containing a token must also fail `TypeNotCopyable` — the recursive
/// `Dict::try_clone` copyability check blocks duplicating tokens buried
/// inside sub-dicts, not just top-level ones (audit p9).
#[test]
fn getdup_nested_token_dict_errors() {
    // outer = { 9: inner } where inner = { 5: ClearToken(0, 7) }.
    let script = ScriptBuilder::new()
        // build inner dict { 5: ClearToken(0,7) }
        .push_int(7u64).pushtoken()
        .push_int(5u64)
        .push_int(1u64).dict()
        // build outer { 9: inner }
        .push_int(9u64)
        .push_int(1u64).dict()
        // getdup key 9 → would copy the token-bearing inner dict
        .push_int(9u64).get_dup()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

#[test]
fn first_of_empty_pushes_zero() {
    let script = ScriptBuilder::new().push_int(0u64).dict().first().to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn first_returns_smallest_key() {
    // Build dict {5: 50, 1: 10}.
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64)
        .push_int(10u64).push_int(1u64)
        .push_int(2u64).dict()
        .first()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn last_returns_largest_key() {
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64)
        .push_int(10u64).push_int(1u64)
        .push_int(2u64).dict()
        .last()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(5u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn next_finds_strictly_greater_key() {
    // {1: 10, 5: 50}; next of 1 → 5.
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64)
        .push_int(10u64).push_int(1u64)
        .push_int(2u64).dict()
        .push_int(1u64).next()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(5u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn next_past_last_pushes_zero() {
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()
        .push_int(5u64).next()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn dict_with_token_is_noncopyable() {
    // Build {5: ClearToken(0, flavor=7)}; the dict should be marked
    // non-copyable.
    let script = ScriptBuilder::new()
        .push_int(7u64).pushtoken()
        .push_int(5u64).push_int(1u64).dict()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Dict(d) => {
            assert!(!d.is_copyable());
            assert!(d.is_portable()); // zero-qty ClearToken is portable
        }
        _ => panic!("expected Dict"),
    }
}

#[test]
fn empty_dict_is_droppable() {
    let mut vm = vm_with_script(
        ScriptBuilder::new().push_int(0u64).dict().drop_().to_bytecode(),
    );
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn nonempty_dict_of_droppable_values_is_droppable() {
    // Under the revised drop rules, a dict is droppable iff every
    // value it has ever held is droppable. A dict of plain Int253s
    // is droppable, including non-empty ones — dropping it has no
    // value-loss semantics.
    let script = ScriptBuilder::new()
        .push_int(50u64).push_int(5u64).push_int(1u64).dict()
        .drop_()
        .to_bytecode();
    let mut vm = vm_with_script(script);
    run_to_end(&mut vm).expect("non-empty dict of droppable values is droppable");
    assert!(vm.current_call.stack.is_empty());
}
