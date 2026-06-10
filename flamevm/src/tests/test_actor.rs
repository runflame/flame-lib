//! Tests for the actor data model + MemRegistry.

use readerwriter::{Decodable, ReadError};

use crate::{
    empty_state, grace_window, state_root, vbyte_size,
    ActorID, ActorRegistry, Dict, Int253, MemRegistry, String, VbytePool,
    Value, VMError,
    GRACE_BLOCKS_CAP,
    VBYTES_PER_BLOCK,
};

/// An actor's code blob (a single bytecode string dispatched on `method`).
/// Tests use a trivial `nop` (0x1d) where the exact bytes don't matter.
fn fixture_code() -> Vec<u8> {
    vec![0x1d]
}

/// Test helper: arbitrary canonical id (real deployments derive it
/// from the constructor; tests hand-pick the bytes).
fn fixture_id(seed: u8) -> ActorID {
    ActorID::Hash([seed; 32])
}

/// A small state `Value` with `n` entries — state may be any portable Value.
fn state_with(entries: &[(u64, u64)]) -> Value {
    let mut d = Dict::new();
    for (k, v) in entries {
        d.insert(Int253::from(*k), Value::Int253(Int253::from(*v)));
    }
    Value::Dict(d)
}

/// Undo-log checkpoint: an inner frame that *commits* a save, then an
/// outer rollback, must still undo the inner-committed mutation.
#[test]
fn checkpoint_inner_commit_then_outer_rollback_undoes_save() {
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0x33; 32]);
    reg.deploy(id.clone(), fixture_code(), empty_state(), 1_000, 0).expect("deploy");
    let s0 = reg.load_state(&id).expect("checkout");
    let root_before = state_root(&s0);
    reg.save_state(&id, s0).expect("checkin");

    reg.push_checkpoint(); // outer
    reg.push_checkpoint(); // inner
    drop(reg.load_state(&id).expect("checkout"));
    reg.save_state(&id, state_with(&[(9, 1)])).expect("save");
    reg.pop_checkpoint_commit(); // inner commits → undo merges into outer
    reg.pop_checkpoint_rollback(); // outer rolls back → must restore original

    let s_after = reg.load_state(&id).expect("checkout");
    assert_eq!(state_root(&s_after), root_before, "outer rollback must undo inner-committed save");
}

/// An actor deployed inside a checkpoint is removed on rollback.
#[test]
fn checkpoint_rollback_removes_actor_deployed_in_frame() {
    let mut reg = MemRegistry::new();
    let id = ActorID::Hash([0x44; 32]);
    reg.push_checkpoint();
    reg.deploy(id.clone(), fixture_code(), empty_state(), 1_000, 0).expect("deploy");
    assert!(reg.exists(&id));
    reg.pop_checkpoint_rollback();
    assert!(!reg.exists(&id), "rollback must remove the in-frame deploy");
}

#[test]
fn actorid_hash_and_constructor_resolve_to_same_canonical_id() {
    let bytes = vec![0x01, 0x02, 0x03];
    let ctor = ActorID::Constructor(bytes.clone());
    let h = ctor.to_hash();
    let hash_form = ActorID::Hash(h);
    assert_eq!(ctor.to_hash(), hash_form.to_hash());
    assert_eq!(ctor.to_canonical(), hash_form);
    assert_eq!(hash_form.to_canonical(), hash_form);
}

#[test]
fn actorid_registry_treats_both_forms_as_same_actor() {
    let mut r = MemRegistry::new();
    let ctor = ActorID::Constructor(vec![0x11, 0x22, 0x33]);
    let hash_form = ActorID::Hash(ctor.to_hash());

    r.deploy(ctor.clone(), fixture_code(), empty_state(), 1_000, 0)
        .expect("deploy via Constructor");
    assert!(r.exists(&ctor));
    assert!(r.exists(&hash_form));
    let err = r
        .deploy(hash_form, fixture_code(), empty_state(), 1_000, 0)
        .expect_err("collide");
    assert!(matches!(err, VMError::ActorAlreadyExists));
}

#[test]
fn actorid_decode_rejects_unknown_tag() {
    let bytes = vec![0x42u8];
    let mut r = bytes.as_slice();
    let err = ActorID::decode(&mut r).expect_err("must error");
    assert!(matches!(err, ReadError::InvalidFormat));
}

// ── code blob: load_code / set_code ─────────────────────────

#[test]
fn memregistry_load_code_returns_code() {
    let mut r = MemRegistry::new();
    let id = fixture_id(0x2a);
    r.deploy(id.clone(), vec![0xde, 0xad], empty_state(), 100, 0).expect("deploy");
    assert_eq!(r.load_code(&id).expect("load_code"), vec![0xde, 0xad]);
}

#[test]
fn memregistry_set_code_replaces_code() {
    let mut r = MemRegistry::new();
    let id = fixture_id(0x2b);
    r.deploy(id.clone(), vec![0x01], empty_state(), 100, 0).expect("deploy");
    r.set_code(&id, vec![0x02, 0x03]).expect("set_code");
    assert_eq!(r.load_code(&id).expect("load_code"), vec![0x02, 0x03]);
}

#[test]
fn memregistry_load_code_on_checked_out_errors() {
    // State checked out → re-entrancy block: dispatch (load_code) fails.
    let mut r = MemRegistry::new();
    let id = fixture_id(0x2c);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    let _s = r.load_state(&id).expect("checkout");
    assert!(matches!(r.load_code(&id).unwrap_err(), VMError::ActorEmpty));
}

#[test]
fn memregistry_load_code_unknown_errors() {
    let r = MemRegistry::new();
    assert!(matches!(
        r.load_code(&fixture_id(0x2d)).unwrap_err(),
        VMError::ActorNotFound
    ));
}

// ── vbyte sizing ────────────────────────────────────────────

#[test]
fn vbyte_size_grows_with_state() {
    let code = fixture_code();
    let small = empty_state();
    let mut big = Dict::new();
    big.insert(Int253::from(0u64), Value::String(String::from(vec![0u8; 100])));
    let big = Value::Dict(big);
    let n_small = vbyte_size(&code, &small).expect("vbyte_size");
    let n_big = vbyte_size(&code, &big).expect("vbyte_size");
    assert!(n_big > n_small, "bigger state → bigger vbytes");
    assert!(n_big - n_small >= 100, "payload at least accounts for the 100-byte string");
}

#[test]
fn vbyte_size_grows_with_code() {
    let state = empty_state();
    let n_small = vbyte_size(&[0x1d], &state).expect("vbyte_size");
    let n_big = vbyte_size(&vec![0x1d; 100], &state).expect("vbyte_size");
    assert!(n_big - n_small >= 99, "code length counts toward vbytes");
}

#[test]
fn vbyte_pool_introduce_adds_per_block_amount() {
    let mut p = VbytePool::new();
    assert_eq!(p.available, 0);
    p.introduce_block_vbytes();
    assert_eq!(p.available, VBYTES_PER_BLOCK);
    p.introduce_block_vbytes();
    assert_eq!(p.available, 2 * VBYTES_PER_BLOCK);
}

#[test]
fn vbyte_pool_queue_and_release_at_maturity() {
    let mut p = VbytePool::new();
    p.queue_recycle(1000, 50);
    assert_eq!(p.release_matured(149), 0);
    assert_eq!(p.available, 0);
    assert_eq!(p.release_matured(150), 1000);
    assert_eq!(p.available, 1000);
}

#[test]
fn vbyte_pool_multiple_recycles_accumulate_per_bucket() {
    let mut p = VbytePool::new();
    p.queue_recycle(100, 10);
    p.queue_recycle(50, 10);
    p.queue_recycle(200, 20);
    assert_eq!(p.release_matured(115), 150);
    assert_eq!(p.release_matured(120), 200);
    assert_eq!(p.available, 350);
}

#[test]
fn grace_window_capped_at_six_months() {
    assert_eq!(grace_window(u64::MAX), GRACE_BLOCKS_CAP);
}

// ── deploy / load / save ────────────────────────────────────

#[test]
fn memregistry_deploy_load_roundtrip() {
    let mut r = MemRegistry::new();
    let id = fixture_id(0xab);
    r.deploy(id.clone(), fixture_code(), state_with(&[(1, 7)]), 1000, 5).expect("deploy");
    let loaded = r.load_state(&id).expect("load");
    match loaded {
        Value::Dict(d) => assert_eq!(d.len(), 1),
        _ => panic!("expected dict state"),
    }
    assert!(r.exists(&id));
    assert_eq!(r.actor_vbytes(&id).expect("vbytes"), 1000);
}

#[test]
fn memregistry_deploy_collision_errors() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([0u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("first");
    let err = r
        .deploy(id, fixture_code(), empty_state(), 100, 0)
        .expect_err("second must error");
    assert!(matches!(err, VMError::ActorAlreadyExists));
}

#[test]
fn memregistry_load_unknown_id_errors() {
    let mut r = MemRegistry::new();
    let err = r
        .load_state(&ActorID::Hash([0u8; 32]))
        .expect_err("must error");
    assert!(matches!(err, VMError::ActorNotFound));
}

#[test]
fn memregistry_save_persists_state() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([1u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 1000, 0).expect("deploy");
    // Check the state out first (save only accepts a checked-out actor).
    let _ = r.load_state(&id).expect("checkout");
    r.save_state(&id, state_with(&[(99, 7)])).expect("save");
    let loaded = r.load_state(&id).expect("load");
    match loaded {
        Value::Dict(d) => assert_eq!(d.len(), 1),
        _ => panic!("expected dict state"),
    }
}

#[test]
fn memregistry_checkout_checkin_round_trip() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([4u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    assert!(!r.actor(&id).unwrap().is_checked_out());
    let state = r.load_state(&id).expect("checkout");
    assert!(r.actor(&id).unwrap().is_checked_out());
    r.save_state(&id, state).expect("checkin");
    assert!(!r.actor(&id).unwrap().is_checked_out());
}

#[test]
fn memregistry_commit_tx_reaps_checked_out_actors_and_queues_vbytes() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([5u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    let _state = r.load_state(&id).expect("checkout"); // never saved back
    let cleared = r.commit_tx_destructions(10);
    assert_eq!(cleared, 1);
    assert!(!r.exists(&id));
    assert_eq!(r.vbyte_pool().maturing.get(&110).copied(), Some(100));
}

#[test]
fn memregistry_commit_tx_leaves_present_actors_alone() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([6u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    assert_eq!(r.commit_tx_destructions(10), 0);
    assert!(r.exists(&id));
}

#[test]
fn memregistry_credit_vbytes_to_active_actor_adds_balance() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([7u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    r.credit_vbytes(&id, 50, 10).expect("credit");
    let a = r.actor(&id).expect("present");
    assert_eq!(a.vbytes, 150);
    assert!(!a.is_frozen());
}

#[test]
fn memregistry_credit_unfreezes_and_resets_counters() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([8u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    {
        let a = r.actor_mut(&id).expect("present");
        a.vbytes = 0;
        a.active_blocks = 50;
        a.frozen_since = Some(20);
    }
    r.credit_vbytes(&id, 500, 100).expect("credit");
    let a = r.actor(&id).expect("present");
    assert_eq!(a.vbytes, 500);
    assert!(!a.is_frozen());
    assert_eq!(a.active_blocks, 0);
    assert_eq!(a.last_activation_height, 100);
}

#[test]
fn tick_block_bleeds_active_actors_and_advances_counter() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([9u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 10_000, 0).expect("deploy");
    let _ = r.tick_block(1);
    let a = r.actor(&id).expect("present");
    assert!(a.vbytes < 10_000, "vbytes bled");
    assert_eq!(a.active_blocks, 1);
    assert!(!a.is_frozen());
}

#[test]
fn tick_block_freezes_on_exhaustion() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([10u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 10, 0).expect("deploy");
    let _ = r.tick_block(1);
    let a = r.actor(&id).expect("present");
    assert_eq!(a.vbytes, 0);
    assert!(a.is_frozen());
    assert_eq!(a.frozen_since, Some(1));
}

#[test]
fn tick_block_expires_frozen_actor_past_grace() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([11u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    {
        let a = r.actor_mut(&id).expect("present");
        a.vbytes = 0;
        a.active_blocks = 4;
        a.frozen_since = Some(10);
    }
    let cleared = r.tick_block(11);
    assert_eq!(cleared, vec![id.clone()]);
    assert!(!r.exists(&id));
}

#[test]
fn tick_block_keeps_frozen_actor_within_grace() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([12u8; 32]);
    r.deploy(id.clone(), fixture_code(), empty_state(), 100, 0).expect("deploy");
    {
        let a = r.actor_mut(&id).expect("present");
        a.vbytes = 0;
        a.active_blocks = 1000;
        a.frozen_since = Some(10);
    }
    let cleared = r.tick_block(50);
    assert!(cleared.is_empty());
    assert!(r.exists(&id));
    assert!(r.actor(&id).expect("present").is_frozen());
}

#[test]
fn tick_block_releases_matured_pool_entries() {
    let mut r = MemRegistry::new();
    r.pool_mut().queue_recycle(2_000, 10);
    let _ = r.tick_block(110);
    assert!(r.vbyte_pool().available >= VBYTES_PER_BLOCK + 2_000);
}
