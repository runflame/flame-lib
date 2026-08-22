//! Tests for actor identity, charged usage, and registry atomicity.

use readerwriter::{Decodable, ReadError};

use super::mem_registry::MemRegistry;
use crate::{
    code_state_bytes, empty_state, state_root, ActorID, ActorRegistry, Dict, Int253, String,
    VMError, Value,
};

fn fixture_code() -> Vec<u8> {
    vec![0x1d]
}

fn fixture_id(seed: u8) -> ActorID {
    ActorID::Hash([seed; 32])
}

fn state_with(entries: &[(u64, u64)]) -> Value {
    let mut state = Dict::new();
    for (key, value) in entries {
        state.insert(Int253::from(*key), Value::Int253(Int253::from(*value)));
    }
    Value::Dict(state)
}

#[test]
fn checkpoint_inner_commit_then_outer_rollback_undoes_save() {
    let mut reg = MemRegistry::new();
    let id = fixture_id(0x33);
    reg.deploy(id.clone(), fixture_code(), empty_state(), 1_000)
        .unwrap();
    let state = reg.load_state(&id).unwrap();
    let root_before = state_root(&state);
    reg.save_state(&id, state).unwrap();

    reg.push_checkpoint();
    reg.push_checkpoint();
    drop(reg.load_state(&id).unwrap());
    reg.save_state(&id, state_with(&[(9, 1)])).unwrap();
    reg.pop_checkpoint_commit();
    reg.pop_checkpoint_rollback();

    assert_eq!(state_root(&reg.load_state(&id).unwrap()), root_before);
}

#[test]
fn checkpoint_rollback_removes_deployed_actor() {
    let mut reg = MemRegistry::new();
    let id = fixture_id(0x44);
    reg.push_checkpoint();
    reg.deploy(id.clone(), fixture_code(), empty_state(), 1_000)
        .unwrap();
    reg.pop_checkpoint_rollback();
    assert!(!reg.exists(&id));
}

#[test]
fn actorid_forms_share_one_registry_key() {
    let constructor = ActorID::Constructor(vec![1, 2, 3]);
    let hash = constructor.to_canonical();
    assert_eq!(constructor.to_hash(), hash.to_hash());

    let mut reg = MemRegistry::new();
    reg.deploy(constructor.clone(), fixture_code(), empty_state(), 1_000)
        .unwrap();
    assert!(reg.exists(&hash));
    assert!(matches!(
        reg.deploy(hash, fixture_code(), empty_state(), 1_000),
        Err(VMError::ActorAlreadyExists)
    ));
}

#[test]
fn actorid_decode_rejects_unknown_tag() {
    let mut bytes = [0x42u8].as_slice();
    assert!(matches!(
        ActorID::decode(&mut bytes),
        Err(ReadError::InvalidFormat)
    ));
}

#[test]
fn registry_code_roundtrip_and_reentry_lock() {
    let mut reg = MemRegistry::new();
    let id = fixture_id(0x2a);
    reg.deploy(id.clone(), vec![0xde, 0xad], empty_state(), 100)
        .unwrap();
    assert_eq!(reg.load_code(&id).unwrap(), vec![0xde, 0xad]);
    reg.set_code(&id, vec![0x02, 0x03]).unwrap();
    assert_eq!(reg.load_code(&id).unwrap(), vec![0x02, 0x03]);
    drop(reg.load_state(&id).unwrap());
    assert!(matches!(reg.load_code(&id), Err(VMError::ActorEmpty)));
}

#[test]
fn charged_usage_counts_state_and_code() {
    let code = fixture_code();
    let small = empty_state();
    let mut large = Dict::new();
    large.insert(Int253::ZERO, Value::String(String::from(vec![0u8; 100])));
    let large = Value::Dict(large);
    let small_usage = code_state_bytes(&code, &small).unwrap();
    let large_usage = code_state_bytes(&code, &large).unwrap();
    assert!(large_usage - small_usage >= 100);
    assert!(code_state_bytes(&[0x1d; 100], &small).unwrap() - small_usage >= 99);
}

#[test]
fn registry_load_save_roundtrip_preserves_capacity() {
    let mut reg = MemRegistry::new();
    let id = fixture_id(0xab);
    reg.deploy(id.clone(), fixture_code(), state_with(&[(1, 7)]), 1_000)
        .unwrap();
    let state = reg.load_state(&id).unwrap();
    reg.save_state(&id, state).unwrap();
    assert_eq!(reg.actor_capacity(&id, 5).unwrap(), 1_000);
}

#[test]
fn registry_rejects_unknown_load_and_save_without_checkout() {
    let mut reg = MemRegistry::new();
    assert!(matches!(
        reg.load_state(&fixture_id(0)),
        Err(VMError::ActorNotFound)
    ));

    let id = fixture_id(1);
    reg.deploy(id.clone(), fixture_code(), empty_state(), 100)
        .unwrap();
    assert!(matches!(
        reg.save_state(&id, empty_state()),
        Err(VMError::SaveWithoutLoad)
    ));
}

#[test]
fn transaction_destruction_reaps_only_checked_out_actor() {
    let mut reg = MemRegistry::new();
    let removed = fixture_id(5);
    let retained = fixture_id(6);
    reg.deploy(removed.clone(), fixture_code(), empty_state(), 100)
        .unwrap();
    reg.deploy(retained.clone(), fixture_code(), empty_state(), 100)
        .unwrap();
    drop(reg.load_state(&removed).unwrap());

    assert_eq!(
        ActorRegistry::commit_tx_destructions(&mut reg),
        vec![removed.clone()]
    );
    assert!(!reg.exists(&removed));
    assert!(reg.exists(&retained));
}
