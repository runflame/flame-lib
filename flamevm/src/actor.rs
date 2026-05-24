//! Actor data model — identity, state, lifecycle counters.
//!
//! Module boundary: anything actor-shaped lives here; `vm.rs` only
//! holds the opcode handlers that read/mutate actors through the
//! registry interface. Cross-references flow one way:
//! `vm.rs → actor.rs`, never the reverse.
//!
//! Unit 1 of the actor build (per the plan in
//! `flamevm/plan.md`) introduces the data layer: `ActorID`,
//! `MethodKey`, `ActorState`, `Actor`, and the canonical
//! `vbyte_size` measure. The `Address` enum, the registry trait,
//! the vbyte pool, and the per-block lifecycle ticker arrive in
//! subsequent units.

use merlin::Transcript;
use readerwriter::{ReadError, Reader, WriteError, Writer};

use crate::dict::Dict;
use crate::encoding::{read_value, write_dict};
use crate::errors::VMError;
use crate::int253::Int253;
use crate::string::String;
use crate::value::Value;

// ── Domain constants ─────────────────────────────────────────────

/// Transcript label for canonical `ActorID::Hash` derivation from
/// initial `ActorState`. Per Q1 (forthcoming ADR
/// `0010-actor-data-model`): `flamevm.actorid`.
pub const ACTOR_ID_DOMAIN: &[u8] = b"flamevm.actorid";

/// Fixed vbyte overhead added on top of `encode(state).len()` for
/// the per-actor lifecycle counters (`vbytes`, `active_blocks`,
/// `last_activation_height`, `frozen_since`). Sized at 32 bytes —
/// four `u64`s worth, the worst case when all four fields are
/// present. Per Q2: `vbyte = wire_len(state) + this constant`.
pub const ACTOR_LIFECYCLE_OVERHEAD_VBYTES: u64 = 32;

/// Reserved method key in `ActorState.public`. Dispatched for every
/// inbound message send (`recv`); other keys are reachable only via
/// `op_call` between actors.
pub const RECV_METHOD_KEY: MethodKey = MethodKey(Int253::ZERO);

/// Reserved dict keys inside the `ActorState` wrapper Dict.
pub const ACTOR_STATE_PUBLIC_KEY_RAW: u64 = 0x00;
pub const ACTOR_STATE_PRIVATE_KEY_RAW: u64 = 0x01;

// ── MethodKey ────────────────────────────────────────────────────

/// Method index within an actor's `public` Dict. Typed as `Int253`
/// to match the underlying Dict key type — `public` is
/// `Dict<Int253 → String>`, so two layers of integer types would be
/// gratuitous. Convenient constructors via `From<u64>` / `From<i64>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MethodKey(pub Int253);

impl MethodKey {
    /// Returns the underlying scalar key. Convenient for
    /// passing into `Dict::get` without an explicit deconstruction.
    pub fn as_int(&self) -> &Int253 {
        &self.0
    }
}

impl From<u64> for MethodKey {
    fn from(n: u64) -> Self {
        MethodKey(Int253::from(n))
    }
}

impl From<i64> for MethodKey {
    fn from(n: i64) -> Self {
        MethodKey(Int253::from(n))
    }
}

impl From<Int253> for MethodKey {
    fn from(i: Int253) -> Self {
        MethodKey(i)
    }
}

// ── ActorID ──────────────────────────────────────────────────────

/// Canonical actor identifier.
///
/// Two forms per `design.md`:
///
/// - [`ActorID::Hash`] — the canonical 32-byte fingerprint of the
///   actor's initial [`ActorState`]. Domain-separated by
///   [`ACTOR_ID_DOMAIN`]. The form every persisted actor uses.
///
/// - [`ActorID::Constructor`] — an actor that doesn't yet exist on
///   chain. Per Q4 the constructor instantiates the state on the
///   fly within the same transaction: the first delivery to this
///   id runs the constructor, computes the canonical hash from the
///   resulting `ActorState`, and the registry key flips from
///   `Constructor(bytes) → Hash(canonical_id)`.
///
/// Wire form: a tag byte (`0x00` Hash, `0x01` Constructor) followed
/// by the payload. The Hash variant's payload is a bare 32 bytes;
/// the Constructor variant's payload is an 8-byte little-endian
/// length followed by the script bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActorID {
    /// Canonical 32-byte hash of the initial actor state.
    Hash([u8; 32]),

    /// Constructor script for transparent deployment. Resolved to
    /// [`ActorID::Hash`] on first delivery (Q4).
    Constructor(Vec<u8>),
}

impl ActorID {
    /// Tag value for [`ActorID::Hash`] on the wire.
    pub const TAG_HASH: u8 = 0x00;
    /// Tag value for [`ActorID::Constructor`] on the wire.
    pub const TAG_CONSTRUCTOR: u8 = 0x01;

    /// Returns a 32-byte representative for this id.
    ///
    /// - [`ActorID::Hash`] returns the hash directly.
    /// - [`ActorID::Constructor`] hashes the constructor bytes
    ///   under [`ACTOR_ID_DOMAIN`] — useful as a pre-deployment
    ///   routing seed.
    ///
    /// Note: `Constructor(bytes).to_hash()` does **not** equal the
    /// post-deployment `Hash(...)` value — the latter hashes the
    /// resulting state, not the constructor input. Use
    /// [`ActorID::canonical_from_initial_state`] to compute the
    /// post-deployment id.
    pub fn to_hash(&self) -> [u8; 32] {
        match self {
            ActorID::Hash(h) => *h,
            ActorID::Constructor(bytes) => {
                let mut t = Transcript::new(ACTOR_ID_DOMAIN);
                t.append_message(b"constructor", bytes);
                let mut h = [0u8; 32];
                t.challenge_bytes(b"id", &mut h);
                h
            }
        }
    }

    /// Computes the canonical [`ActorID::Hash`] for a freshly
    /// deployed actor from its initial [`ActorState`]. The hash
    /// binds to the state's canonical wire encoding, so two
    /// structurally equivalent states deterministically produce the
    /// same id.
    pub fn canonical_from_initial_state(state: &ActorState) -> Self {
        let mut buf = Vec::new();
        state
            .encode(&mut buf)
            .expect("ActorState always encodable (portable Dict)");
        let mut t = Transcript::new(ACTOR_ID_DOMAIN);
        t.append_message(b"state", &buf);
        let mut h = [0u8; 32];
        t.challenge_bytes(b"id", &mut h);
        ActorID::Hash(h)
    }

    /// True iff this id is already in canonical hash form.
    pub fn is_resolved(&self) -> bool {
        matches!(self, ActorID::Hash(_))
    }

    /// Writes the canonical wire form (tag byte + payload).
    pub fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        match self {
            ActorID::Hash(h) => {
                w.write_u8(b"actorid.tag", Self::TAG_HASH)?;
                w.write(b"actorid.hash", h)
            }
            ActorID::Constructor(bytes) => {
                w.write_u8(b"actorid.tag", Self::TAG_CONSTRUCTOR)?;
                let len = bytes.len() as u64;
                w.write(b"actorid.ctor.len", &len.to_le_bytes())?;
                w.write(b"actorid.ctor.bytes", bytes)
            }
        }
    }

    /// Reads the canonical wire form.
    pub fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        let tag = r.read_u8()?;
        match tag {
            Self::TAG_HASH => {
                let h = r.read_u8x32()?;
                Ok(ActorID::Hash(h))
            }
            Self::TAG_CONSTRUCTOR => {
                let len = r.read_u64()? as usize;
                let mut bytes = vec![0u8; len];
                r.read(&mut bytes)?;
                Ok(ActorID::Constructor(bytes))
            }
            _ => Err(ReadError::InvalidFormat),
        }
    }

    /// Convenience: encode to a fresh `Vec<u8>`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out).expect("Vec<u8> writer never fails");
        out
    }
}

// ── ActorState ───────────────────────────────────────────────────

/// The full mutable state of an actor — a Dict with two reserved
/// keys: `public` (methods callable by other actors / external
/// senders) and `private` (internal state visible only to the
/// actor's own scripts). Per `flamevm/spec.md` §Actors.
///
/// The two halves are kept as distinct fields rather than a flat
/// wrapper Dict so callers don't have to round-trip through
/// `get`/`put` to mutate one side. Wire form is the wrapper Dict
/// via [`ActorState::encode`] / [`ActorState::decode`].
pub struct ActorState {
    /// Methods callable by message sends (`recv` at key 0) and by
    /// other actors' `op_call`. Keys: `Int253`; values: `String` of
    /// method script bytes.
    pub public: Dict,

    /// Internal state and helpers. Keys: `Int253`; values:
    /// arbitrary portable [`Value`]s.
    pub private: Dict,
}

impl ActorState {
    /// Constructs an empty state.
    pub fn new() -> Self {
        Self {
            public: Dict::new(),
            private: Dict::new(),
        }
    }

    /// Constructs a state with a pre-populated `public` Dict. Used
    /// when deploying an actor with a fixed method table.
    pub fn with_public(public: Dict) -> Self {
        Self {
            public,
            private: Dict::new(),
        }
    }

    /// Looks up a method script by key. Returns `None` if the key
    /// is absent or the value at that key isn't a `String`.
    pub fn resolve_method(&self, key: &MethodKey) -> Option<&String> {
        match self.public.get(key.as_int())? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// True iff `public` contains a method at `key`.
    pub fn has_method(&self, key: &MethodKey) -> bool {
        self.public.get(key.as_int()).is_some()
    }

    /// Writes the canonical wire form — the wrapper Dict
    /// (`0x00 → public`, `0x01 → private`). Errors only if a
    /// `private` payload value lacks an encoder; `public` is
    /// guaranteed encodable (entries are all `String`).
    pub fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        let wrapper = self.to_wrapper_dict();
        write_dict(w, &wrapper)
    }

    /// Reads the canonical wire form. Strict: rejects any deviation
    /// from the 2-entry shape or non-Dict values at either slot.
    ///
    /// The wrapper is itself a Dict — the encoder uniformly hands
    /// either the list-style form (sequential 0/1 keys, shortest
    /// encoding) or dict-style. We use the top-level `read_value`
    /// and downcast: that way both shapes decode without us having
    /// to peek at the tag byte.
    pub fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        match read_value(r)? {
            Some(Value::Dict(d)) => {
                Self::from_wrapper_dict(d).map_err(|_| ReadError::InvalidFormat)
            }
            _ => Err(ReadError::InvalidFormat),
        }
    }

    /// Returns the wrapper Dict that mirrors the wire form. Useful
    /// when callers want to push the actor state onto the VM stack
    /// (`op_load` will consume this).
    pub fn to_wrapper_dict(&self) -> Dict {
        let mut d = Dict::new();
        d.insert(
            Int253::from(ACTOR_STATE_PUBLIC_KEY_RAW),
            Value::Dict(
                self.public
                    .try_clone()
                    .expect("public dict (Strings only) is always copyable"),
            ),
        );
        d.insert(
            Int253::from(ACTOR_STATE_PRIVATE_KEY_RAW),
            Value::Dict(
                self.private
                    .try_clone()
                    .expect("private dict must hold copyable portable values"),
            ),
        );
        d
    }

    /// Parses a wrapper Dict back into an `ActorState`. Inverse of
    /// [`ActorState::to_wrapper_dict`].
    pub fn from_wrapper_dict(mut d: Dict) -> Result<Self, VMError> {
        let public_key = Int253::from(ACTOR_STATE_PUBLIC_KEY_RAW);
        let private_key = Int253::from(ACTOR_STATE_PRIVATE_KEY_RAW);
        if d.len() != 2 {
            return Err(VMError::MalformedActorState);
        }
        let public = match d.remove(&public_key) {
            Some(Value::Dict(d)) => d,
            _ => return Err(VMError::MalformedActorState),
        };
        let private = match d.remove(&private_key) {
            Some(Value::Dict(d)) => d,
            _ => return Err(VMError::MalformedActorState),
        };
        Ok(Self { public, private })
    }
}

impl Default for ActorState {
    fn default() -> Self {
        Self::new()
    }
}

/// Manual `Debug` impl — `Dict` lacks `#[derive(Debug)]` (because
/// its `Value` payloads include linear types that can't derive
/// `Debug`), so the standard derive doesn't apply. Print just the
/// public/private slot counts — useful for `Result::expect_err`
/// callers without leaking internal payload structure.
impl core::fmt::Debug for ActorState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActorState")
            .field("public.len", &self.public.len())
            .field("private.len", &self.private.len())
            .finish()
    }
}

// ── Actor (full record, including lifecycle counters) ────────────

/// Full per-actor record stored in the registry. Combines the
/// mutable [`ActorState`] (visible to scripts) with the
/// protocol-managed lifecycle counters (vbyte balance, activation
/// tracking, freeze state). Per `flamevm/spec.md` §Storage and ADR
/// 0005.
pub struct Actor {
    /// Mutable script-visible state.
    pub state: ActorState,

    /// Persistent vbyte balance. Bled per block during `tick_block`
    /// (Unit 3). `0` puts the actor in the frozen state
    /// ([`Self::frozen_since`]).
    pub vbytes: u64,

    /// Number of blocks the actor has been continuously active.
    /// Drives the grace-window formula
    /// `min(active_blocks/4, blocks_per_6_months)`. Reset by a
    /// top-up when frozen (per ADR 0005).
    pub active_blocks: u64,

    /// Height at which the actor's last activation (deployment or
    /// post-freeze top-up) occurred. Combined with `active_blocks`
    /// the grace window is precisely bounded.
    pub last_activation_height: u64,

    /// `Some(height)` iff currently frozen; the height is when the
    /// vbyte balance first hit zero. `None` iff active.
    pub frozen_since: Option<u64>,
}

impl Actor {
    /// Constructs a fresh actor with the given initial state and
    /// vbyte funding, activated at `height`.
    pub fn new_active(state: ActorState, vbytes: u64, height: u64) -> Self {
        Self {
            state,
            vbytes,
            active_blocks: 0,
            last_activation_height: height,
            frozen_since: None,
        }
    }

    /// True iff currently in the frozen state.
    pub fn is_frozen(&self) -> bool {
        self.frozen_since.is_some()
    }
}

/// Manual `Debug` impl — `ActorState` has its own manual impl (see
/// above), so derive doesn't compose for `Actor`. Print the
/// lifecycle counters; defer state-shape printing to the
/// `ActorState` impl.
impl core::fmt::Debug for Actor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Actor")
            .field("state", &self.state)
            .field("vbytes", &self.vbytes)
            .field("active_blocks", &self.active_blocks)
            .field("last_activation_height", &self.last_activation_height)
            .field("frozen_since", &self.frozen_since)
            .finish()
    }
}

// ── Vbyte sizing ──────────────────────────────────────────────────

/// Computes the canonical vbyte size of an actor's state, per Q2:
/// `wire_len(ActorState::encode()) + ACTOR_LIFECYCLE_OVERHEAD_VBYTES`.
/// The lifecycle overhead covers the protocol-managed counters
/// every actor carries regardless of state shape.
///
/// Returns `Err(VMError::MalformedActorState)` if the state can't
/// be encoded (a `private` payload containing non-portable values).
pub fn vbyte_size(state: &ActorState) -> Result<u64, VMError> {
    let mut buf = Vec::new();
    state
        .encode(&mut buf)
        .map_err(|_| VMError::MalformedActorState)?;
    Ok(buf.len() as u64 + ACTOR_LIFECYCLE_OVERHEAD_VBYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ActorID ──────────────────────────────────────────────────

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
    fn actorid_canonical_from_initial_state_is_deterministic() {
        let s1 = ActorState::new();
        let s2 = ActorState::new();
        let id1 = ActorID::canonical_from_initial_state(&s1);
        let id2 = ActorID::canonical_from_initial_state(&s2);
        assert_eq!(id1, id2, "same state → same id");
        assert!(matches!(id1, ActorID::Hash(_)));
        assert!(id1.is_resolved());
    }

    #[test]
    fn actorid_canonical_diverges_on_different_state() {
        let mut s1 = ActorState::new();
        s1.public
            .insert(Int253::from(0u64), Value::String(String::from(b"a".to_vec())));
        let mut s2 = ActorState::new();
        s2.public
            .insert(Int253::from(0u64), Value::String(String::from(b"b".to_vec())));
        assert_ne!(
            ActorID::canonical_from_initial_state(&s1),
            ActorID::canonical_from_initial_state(&s2),
            "different state → different id"
        );
    }

    #[test]
    fn actorid_constructor_to_hash_differs_from_canonical_post_deploy() {
        // The two routes deliberately produce different hashes —
        // constructor hashes the script bytes; canonical hashes
        // the resulting state. Documented in the `to_hash` docstring.
        let ctor = ActorID::Constructor(vec![0x01, 0x02, 0x03]);
        let seed = ActorID::Hash(ctor.to_hash());
        let post_state = ActorState::new();
        let canonical = ActorID::canonical_from_initial_state(&post_state);
        assert_ne!(seed, canonical);
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

    // ── MethodKey ────────────────────────────────────────────────

    #[test]
    fn methodkey_constructors_match() {
        let a: MethodKey = 5u64.into();
        let b: MethodKey = 5i64.into();
        let c: MethodKey = Int253::from(5u64).into();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(*a.as_int(), Int253::from(5u64));
    }

    #[test]
    fn recv_method_key_is_zero() {
        assert_eq!(RECV_METHOD_KEY, MethodKey::from(0u64));
        assert_eq!(*RECV_METHOD_KEY.as_int(), Int253::zero());
    }

    // ── ActorState ───────────────────────────────────────────────

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
            Value::String(String::from(b"\x1d".to_vec())), // `nop`
        );
        let m = MethodKey::from(7u64);
        assert!(s.has_method(&m));
        let script = s.resolve_method(&m).expect("present");
        assert_eq!(script.bytes_view().as_ref(), b"\x1d");
    }

    #[test]
    fn actorstate_resolve_method_missing_returns_none() {
        let s = ActorState::new();
        assert!(s.resolve_method(&MethodKey::from(42u64)).is_none());
    }

    #[test]
    fn actorstate_resolve_method_wrong_type_returns_none() {
        let mut s = ActorState::new();
        // A non-String at the public-method slot → not a callable.
        s.public
            .insert(Int253::from(0u64), Value::Int253(Int253::from(99u64)));
        assert!(s.resolve_method(&RECV_METHOD_KEY).is_none());
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
        assert!(back.resolve_method(&RECV_METHOD_KEY).is_some());
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
        // Canonicality: re-encoding the decoded form yields the same bytes.
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
        // Canonicality check.
        let mut buf2 = Vec::new();
        back.encode(&mut buf2).expect("re-encode");
        assert_eq!(buf2, buf);
    }

    #[test]
    fn actorstate_from_wrapper_rejects_wrong_shape() {
        let mut d = Dict::new();
        // Only one entry — not the 2-entry shape.
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

    // ── Actor + vbyte sizing ─────────────────────────────────────

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
}
