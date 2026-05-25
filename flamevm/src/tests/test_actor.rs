//! Tests for the actor data model + MemRegistry.

use readerwriter::ReadError;

use crate::{
    grace_window, vbyte_size, Actor, ActorID, ActorRegistry, ActorState, Dict, Int253,
    MemRegistry, String, VbytePool, Value, VMError,
    ACTOR_LIFECYCLE_OVERHEAD_VBYTES, GRACE_BLOCKS_CAP, RECV_METHOD,
    VBYTES_PER_BLOCK,
};

#[test]
fn actorid_hash_encode_decode_roundtrip() {
    let id = ActorID::Hash([0x42; 32]);
    let bytes = id.to_bytes();
    assert_eq!(bytes.len(), 1 + 32, "tag (1) + hash (32)");
    assert_eq!(bytes[0], ActorID::TAG_HASH);
    let mut r = bytes.as_slice();
    let decoded = ActorID::decode(&mut r).expect("decode");
    assert_eq!(decoded, id);
    assert!(r.is_empty(), "no trailing bytes");
}

#[test]
fn actorid_constructor_encode_decode_roundtrip() {
    let id = ActorID::Constructor(vec![0xde, 0xad, 0xbe, 0xef]);
    let bytes = id.to_bytes();
    assert_eq!(bytes.len(), 1 + 8 + 4, "tag (1) + u64 len (8) + script (4)");
    assert_eq!(bytes[0], ActorID::TAG_CONSTRUCTOR);
    let mut r = bytes.as_slice();
    let decoded = ActorID::decode(&mut r).expect("decode");
    assert_eq!(decoded, id);
    assert!(r.is_empty(), "no trailing bytes");
}

#[test]
fn actorid_constructor_empty_script_roundtrips() {
    let id = ActorID::Constructor(Vec::new());
    let bytes = id.to_bytes();
    let mut r = bytes.as_slice();
    let decoded = ActorID::decode(&mut r).expect("decode");
    assert_eq!(decoded, id);
}

#[test]
fn actorid_constructor_hash_is_deterministic() {
    let a = ActorID::Constructor(vec![0xde, 0xad, 0xbe, 0xef]);
    let b = ActorID::Constructor(vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(a.to_hash(), b.to_hash(), "same bytes → same id");
}

#[test]
fn actorid_constructor_hash_diverges_on_different_bytes() {
    let a = ActorID::Constructor(vec![0x01]);
    let b = ActorID::Constructor(vec![0x02]);
    assert_ne!(a.to_hash(), b.to_hash(), "different bytes → different id");
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
    let ctor_bytes = vec![0x11, 0x22, 0x33];
    let ctor = ActorID::Constructor(ctor_bytes.clone());
    let hash_form = ActorID::Hash(ctor.to_hash());

    r.deploy(ctor.clone(), ActorState::new(), 1_000, 0)
        .expect("deploy via Constructor");
    assert!(r.exists(&ctor));
    assert!(r.exists(&hash_form));
    let err = r
        .deploy(hash_form, ActorState::new(), 1_000, 0)
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

#[test]
fn actorid_unresolved_for_constructor_form() {
    assert!(!ActorID::Constructor(vec![0u8; 4]).is_resolved());
    assert!(ActorID::Hash([0u8; 32]).is_resolved());
}

#[test]
fn recv_method_is_zero() {
    assert_eq!(RECV_METHOD, Int253::zero());
}

#[test]
fn actorstate_new_is_empty() {
    let s = ActorState::new();
    assert!(s.public.is_empty());
    assert!(s.private.is_empty());
}

#[test]
fn actorstate_resolve_method_returns_script() {
    let mut s = ActorState::new();
    s.public.insert(
        Int253::from(7u64),
        Value::String(String::from(b"\x1d".to_vec())),
    );
    let m = Int253::from(7u64);
    assert!(s.has_method(&m));
    let script = s.resolve_method(&m).expect("present");
    assert_eq!(script.bytes_view().as_ref(), b"\x1d");
}

#[test]
fn actorstate_resolve_method_missing_returns_none() {
    let s = ActorState::new();
    assert!(s.resolve_method(&Int253::from(42u64)).is_none());
}

#[test]
fn actorstate_resolve_method_wrong_type_returns_none() {
    let mut s = ActorState::new();
    s.public
        .insert(Int253::from(0u64), Value::Int253(Int253::from(99u64)));
    assert!(s.resolve_method(&RECV_METHOD).is_none());
}

#[test]
fn actorstate_wrapper_dict_roundtrip() {
    let mut s = ActorState::new();
    s.public
        .insert(Int253::from(0u64), Value::String(String::from(b"\x1d".to_vec())));
    s.private
        .insert(Int253::from(1u64), Value::Int253(Int253::from(123u64)));
    let d = s.to_wrapper_dict();
    let back = ActorState::from_wrapper_dict(d).expect("from_wrapper_dict");
    assert_eq!(back.public.len(), 1);
    assert_eq!(back.private.len(), 1);
    assert!(back.resolve_method(&RECV_METHOD).is_some());
}

#[test]
fn actorstate_encode_decode_roundtrip() {
    let mut s = ActorState::new();
    s.public.insert(
        Int253::from(0u64),
        Value::String(String::from(b"\x1d\x1d".to_vec())),
    );
    s.private
        .insert(Int253::from(0u64), Value::Int253(Int253::from(7u64)));
    let mut buf = Vec::new();
    s.encode(&mut buf).expect("encode");
    let mut r = buf.as_slice();
    let back = ActorState::decode(&mut r).expect("decode");
    assert_eq!(back.public.len(), 1);
    assert_eq!(back.private.len(), 1);
    let mut buf2 = Vec::new();
    back.encode(&mut buf2).expect("re-encode");
    assert_eq!(buf2, buf, "canonical: re-encode == encode");
}

#[test]
fn actorstate_empty_encode_decode_roundtrip() {
    let s = ActorState::new();
    let mut buf = Vec::new();
    s.encode(&mut buf).expect("encode");
    let mut r = buf.as_slice();
    let back = ActorState::decode(&mut r).expect("decode");
    assert!(back.public.is_empty());
    assert!(back.private.is_empty());
    let mut buf2 = Vec::new();
    back.encode(&mut buf2).expect("re-encode");
    assert_eq!(buf2, buf);
}

#[test]
fn actorstate_from_wrapper_rejects_wrong_shape() {
    let mut d = Dict::new();
    d.insert(Int253::from(0u64), Value::Dict(Dict::new()));
    let err = ActorState::from_wrapper_dict(d).expect_err("must error");
    assert!(matches!(err, VMError::MalformedActorState));
}

#[test]
fn actorstate_from_wrapper_rejects_non_dict_slots() {
    let mut d = Dict::new();
    d.insert(Int253::from(0u64), Value::Int253(Int253::zero()));
    d.insert(Int253::from(1u64), Value::Dict(Dict::new()));
    let err = ActorState::from_wrapper_dict(d).expect_err("must error");
    assert!(matches!(err, VMError::MalformedActorState));
}

#[test]
fn vbyte_size_empty_state_is_at_least_overhead() {
    let s = ActorState::new();
    let n = vbyte_size(&s).expect("vbyte_size");
    assert!(n >= ACTOR_LIFECYCLE_OVERHEAD_VBYTES);
}

#[test]
fn vbyte_size_grows_with_state() {
    let small = ActorState::new();
    let mut big = ActorState::new();
    big.private.insert(
        Int253::from(0u64),
        Value::String(String::from(vec![0u8; 100])),
    );
    let n_small = vbyte_size(&small).expect("vbyte_size");
    let n_big = vbyte_size(&big).expect("vbyte_size");
    assert!(n_big > n_small, "bigger state → bigger vbytes");
    assert!(
        n_big - n_small >= 100,
        "payload at least accounts for the 100-byte string"
    );
}

#[test]
fn actor_new_active_starts_unfrozen() {
    let a = Actor::new_active(ActorState::new(), 500, 42);
    assert_eq!(a.vbytes, 500);
    assert_eq!(a.last_activation_height, 42);
    assert_eq!(a.active_blocks, 0);
    assert!(!a.is_frozen());
    assert_eq!(a.frozen_since, None);
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
    let released = p.release_matured(149);
    assert_eq!(released, 0);
    assert_eq!(p.available, 0);
    let released = p.release_matured(150);
    assert_eq!(released, 1000);
    assert_eq!(p.available, 1000);
}

#[test]
fn vbyte_pool_queue_zero_is_noop() {
    let mut p = VbytePool::new();
    p.queue_recycle(0, 10);
    assert!(p.is_empty());
}

#[test]
fn vbyte_pool_multiple_recycles_accumulate_per_bucket() {
    let mut p = VbytePool::new();
    p.queue_recycle(100, 10);
    p.queue_recycle(50, 10);
    p.queue_recycle(200, 20);
    let released = p.release_matured(115);
    assert_eq!(released, 150);
    let released = p.release_matured(120);
    assert_eq!(released, 200);
    assert_eq!(p.available, 350);
}

#[test]
fn grace_window_quarters_active_blocks() {
    assert_eq!(grace_window(0), 0);
    assert_eq!(grace_window(4), 1);
    assert_eq!(grace_window(100), 25);
}

#[test]
fn grace_window_capped_at_six_months() {
    assert_eq!(grace_window(u64::MAX), GRACE_BLOCKS_CAP);
}

fn fixture_state() -> ActorState {
    let mut s = ActorState::new();
    s.public.insert(
        RECV_METHOD,
        Value::String(String::from(b"\x1d".to_vec())),
    );
    s
}

/// Test helper: arbitrary canonical id (real deployments derive it
/// from the constructor; tests hand-pick the bytes).
fn fixture_id(seed: u8) -> ActorID {
    ActorID::Hash([seed; 32])
}

#[test]
fn memregistry_deploy_load_roundtrip() {
    let mut r = MemRegistry::new();
    let s = fixture_state();
    let id = fixture_id(0xab);
    r.deploy(id.clone(), s, 1000, 5).expect("deploy");
    let loaded = r.load_state(&id).expect("load");
    assert!(loaded.has_method(&RECV_METHOD));
    assert!(r.exists(&id));
    assert_eq!(r.actor_vbytes(&id).expect("vbytes"), 1000);
}

#[test]
fn memregistry_deploy_collision_errors() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([0u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("first");
    let err = r
        .deploy(id, fixture_state(), 100, 0)
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
    r.deploy(id.clone(), fixture_state(), 1000, 0).expect("deploy");
    let mut updated = ActorState::new();
    updated.private.insert(
        Int253::from(99u64),
        Value::Int253(Int253::from(7u64)),
    );
    r.save_state(&id, updated).expect("save");
    let loaded = r.load_state(&id).expect("load");
    assert_eq!(loaded.private.len(), 1);
    assert!(!loaded.has_method(&RECV_METHOD));
}

#[test]
fn memregistry_resolve_method_returns_script() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([2u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
    let script = r.resolve_method(&id, RECV_METHOD).expect("resolve");
    assert_eq!(script, vec![0x1d]);
}

#[test]
fn memregistry_resolve_method_missing_errors() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([3u8; 32]);
    r.deploy(id.clone(), ActorState::new(), 100, 0).expect("deploy");
    let err = r
        .resolve_method(&id, Int253::from(42u64))
        .expect_err("must error");
    assert!(matches!(err, VMError::MethodNotFound));
}

#[test]
fn memregistry_mark_unmark_round_trip() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([4u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
    assert!(!r.is_marked_for_destruction(&id));
    r.mark_for_destruction(&id);
    assert!(r.is_marked_for_destruction(&id));
    r.unmark_for_destruction(&id);
    assert!(!r.is_marked_for_destruction(&id));
}

#[test]
fn memregistry_commit_tx_clears_marked_actors_and_queues_vbytes() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([5u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
    r.mark_for_destruction(&id);
    let cleared = r.commit_tx_destructions(10);
    assert_eq!(cleared, 1);
    assert!(!r.exists(&id));
    assert_eq!(r.vbyte_pool().maturing.get(&110).copied(), Some(100));
}

#[test]
fn memregistry_commit_tx_leaves_unmarked_actors_alone() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([6u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
    let cleared = r.commit_tx_destructions(10);
    assert_eq!(cleared, 0);
    assert!(r.exists(&id));
}

#[test]
fn memregistry_credit_vbytes_to_active_actor_adds_balance() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([7u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
    r.credit_vbytes(&id, 50, 10).expect("credit");
    let a = r.actor(&id).expect("present");
    assert_eq!(a.vbytes, 150);
    assert!(!a.is_frozen());
}

#[test]
fn memregistry_credit_unfreezes_and_resets_counters() {
    let mut r = MemRegistry::new();
    let id = ActorID::Hash([8u8; 32]);
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
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
    r.deploy(id.clone(), fixture_state(), 10_000, 0).expect("deploy");
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
    r.deploy(id.clone(), fixture_state(), 10, 0).expect("deploy");
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
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
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
    r.deploy(id.clone(), fixture_state(), 100, 0).expect("deploy");
    {
        let a = r.actor_mut(&id).expect("present");
        a.vbytes = 0;
        a.active_blocks = 1000;
        a.frozen_since = Some(10);
    }
    let cleared = r.tick_block(50);
    assert!(cleared.is_empty());
    assert!(r.exists(&id));
    let a = r.actor(&id).expect("present");
    assert!(a.is_frozen());
}

#[test]
fn tick_block_releases_matured_pool_entries() {
    let mut r = MemRegistry::new();
    r.pool_mut().queue_recycle(2_000, 10);
    let _ = r.tick_block(110);
    assert!(r.vbyte_pool().available >= VBYTES_PER_BLOCK + 2_000);
}
