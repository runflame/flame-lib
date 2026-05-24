//! Tests for dict ops.

#![allow(unused_imports)]

use super::test_helpers::*;

// ── dict (0x60) ──────────────────────────────────────────────

#[test]
fn dict_construction_zero_pairs() {
    // push:0, dict — empty dict
    let mut vm = vm_with_script(vec![0x00, 0x60]);
    run_to_end(&mut vm).unwrap();
    match &vm.current_call.stack[0] {
        Value::Dict(d) => assert!(d.is_empty()),
        other => panic!("expected Dict, got {}", value_kind(other)),
    }
}

#[test]
fn dict_construction_two_pairs() {
    // Stack order: val key val key n
    // Build {5: 50, 1: 10}: push 50, push 5, push 10, push 1, push 2, dict
    // (Pairs popped top-first: pair1 = (1, 10), pair2 = (5, 50).)
    // After construction, keys sorted: [1, 5].
    let mut vm = vm_with_script(vec![
        0x10, 50, // val for first pair (will end up at key 5)
        0x05,     // key 5
        0x10, 10, // val for second pair (key 1)
        0x01,     // key 1
        0x02,     // push:2
        0x60,     // dict
    ]);
    run_to_end(&mut vm).unwrap();
    assert_dict_keys(
        &vm.current_call.stack[0],
        &[Int253::from(1u64), Int253::from(5u64)],
    );
}

#[test]
fn dict_construction_duplicate_keys_errors() {
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, // (5, 50)
        0x10, 60, 0x05, // (5, 60)  duplicate!
        0x02, 0x60,
    ]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DictKeyOccupied
    ));
}

// ── put (0x61) ───────────────────────────────────────────────

#[test]
fn put_inserts_into_empty() {
    // push:0, dict (empty)  →  put k=3, v=99
    let mut vm = vm_with_script(vec![
        0x00, 0x60,       // empty dict
        0x03,             // key 3
        0x10, 99,         // value 99
        0x61,
    ]);
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
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, // (5, 50)
        0x01, 0x60,     // dict (1 pair)
        0x05,           // key 5
        0x10, 99,
        0x61,           // put → conflict
    ]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DictKeyOccupied
    ));
}

// ── replace (0x62) ───────────────────────────────────────────

#[test]
fn replace_existing_returns_prev() {
    // Build {5: 50}, then replace v at key 5 with 99.
    // Spec stack: dict k v → dict' {prev 1 | 0}
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, // (5, 50)
        0x01, 0x60,     // dict
        0x05,           // k
        0x10, 99,       // v
        0x62,           // replace
    ]);
    run_to_end(&mut vm).unwrap();
    // Stack: [dict', 50, 1]
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[1], Int253::from(50u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn replace_absent_returns_zero() {
    let mut vm = vm_with_script(vec![
        0x00, 0x60, // empty dict
        0x05,       // k
        0x10, 99,   // v
        0x62,
    ]);
    run_to_end(&mut vm).unwrap();
    // Stack: [dict', 0]
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

// ── get (0x63) ───────────────────────────────────────────────

#[test]
fn get_existing_returns_dict_k_v() {
    // {5: 50}, get key 5.
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x01, 0x60, // dict
        0x05,                       // k
        0x63,
    ]);
    run_to_end(&mut vm).unwrap();
    // Stack: [dict', k=5, v=50]
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[1], Int253::from(5u64));
    assert_int(&vm.current_call.stack[2], Int253::from(50u64));
    // Dict should now be empty.
    match &vm.current_call.stack[0] {
        Value::Dict(d) => assert!(d.is_empty()),
        _ => panic!("expected Dict"),
    }
}

#[test]
fn get_missing_errors() {
    let mut vm = vm_with_script(vec![0x00, 0x60, 0x05, 0x63]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::DictKeyNotFound
    ));
}

// ── getopt (0x64) ────────────────────────────────────────────

#[test]
fn getopt_existing() {
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x01, 0x60, // dict {5: 50}
        0x05,                       // k
        0x64,
    ]);
    run_to_end(&mut vm).unwrap();
    // Stack: [dict', 50, 1]
    assert_int(&vm.current_call.stack[1], Int253::from(50u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn getopt_missing() {
    let mut vm = vm_with_script(vec![0x00, 0x60, 0x05, 0x64]);
    run_to_end(&mut vm).unwrap();
    // Stack: [dict', 0]
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

// ── getdup (0x65) ────────────────────────────────────────────

#[test]
fn getdup_copyable() {
    // {5: 50}; getdup k=5 → dict unchanged + 50 + 1
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x01, 0x60, 0x05, 0x65,
    ]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 3);
    assert_int(&vm.current_call.stack[1], Int253::from(50u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    // Dict still has the entry.
    match &vm.current_call.stack[0] {
        Value::Dict(d) => assert_eq!(d.len(), 1),
        _ => panic!("expected Dict"),
    }
}

#[test]
fn getdup_missing_pushes_zero() {
    let mut vm = vm_with_script(vec![0x00, 0x60, 0x05, 0x65]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn getdup_noncopyable_errors() {
    // {5: ClearToken(0, 7)}; getdup k=5 → TypeNotCopyable
    // push:7 (flavor), pushtoken (value), push:5 (key), push:1 (count),
    //   dict, push:5 (k), getdup.
    let mut vm = vm_with_script(vec![
        0x07, 0x1b, // value = ClearToken with flavor 7
        0x05,       // key 5
        0x01,       // count 1
        0x60,       // dict
        0x05,       // k
        0x65,       // getdup
    ]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotCopyable
    ));
}

// ── first/last/next (0x66-0x68) ──────────────────────────────

#[test]
fn first_of_empty_pushes_zero() {
    let mut vm = vm_with_script(vec![0x00, 0x60, 0x66]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

#[test]
fn first_returns_smallest_key() {
    // Build dict {5: 50, 1: 10}.
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x10, 10, 0x01, 0x02, 0x60, // dict
        0x66,                                       // first
    ]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn last_returns_largest_key() {
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x10, 10, 0x01, 0x02, 0x60, // dict
        0x67,                                       // last
    ]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(5u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn next_finds_strictly_greater_key() {
    // {1: 10, 5: 50}; next of 1 → 5.
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x10, 10, 0x01, 0x02, 0x60, // dict
        0x01,                                       // k = 1
        0x68,
    ]);
    run_to_end(&mut vm).unwrap();
    assert_int(&vm.current_call.stack[1], Int253::from(5u64));
    assert_int(&vm.current_call.stack[2], Int253::from(1u64));
}

#[test]
fn next_past_last_pushes_zero() {
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x01, 0x60, // {5: 50}
        0x05,                       // k = 5
        0x68,
    ]);
    run_to_end(&mut vm).unwrap();
    assert_eq!(vm.current_call.stack.len(), 2);
    assert_int(&vm.current_call.stack[1], Int253::from(0u64));
}

// ── Flag propagation ─────────────────────────────────────────

#[test]
fn dict_with_token_is_noncopyable() {
    // Build {5: ClearToken(0, flavor=7)}; the dict should be marked
    // non-copyable.  push:7 (flavor), pushtoken, push:5, push:1, dict
    let mut vm = vm_with_script(vec![0x07, 0x1b, 0x05, 0x01, 0x60]);
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
    let mut vm = vm_with_script(vec![0x00, 0x60, 0x1c]); // empty dict, drop
    run_to_end(&mut vm).unwrap();
    assert!(vm.current_call.stack.is_empty());
}

#[test]
fn nonempty_dict_is_not_droppable() {
    let mut vm = vm_with_script(vec![
        0x10, 50, 0x05, 0x01, 0x60, // {5: 50}
        0x1c,                       // drop
    ]);
    assert!(matches!(
        run_to_end(&mut vm).unwrap_err(),
        VMError::TypeNotDroppable
    ));
}

