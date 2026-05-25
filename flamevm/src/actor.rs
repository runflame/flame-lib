//! Actor data model: identity, state, lifecycle counters, registry.

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

/// Actor identifier. **Every actor has exactly one identity**;
/// this enum is just two views of the *same* 32-byte hash:
///
/// - [`ActorID::Hash`] — the bare canonical hash. The form scripts
///   use to address already-deployed actors (32 bytes, low overhead).
///
/// - [`ActorID::Constructor`] — the constructor script bytes
///   inlined. Carries the actor's full code on the wire, useful
///   for the first send to a not-yet-deployed actor: consensus
///   sees the script, runs it to instantiate state, and registers
///   the actor under the same canonical id.
///
/// **Equivalence invariant**: `Constructor(bytes).to_hash()` ==
/// `Hash(h).to_hash()` whenever `h == H_{flamevm.actorid}(bytes)`.
/// Both addresses route to the same actor; the registry
/// canonicalizes on the hash so callers can use whichever form
/// they have on hand.
///
/// Wire form: a tag byte (`0x00` Hash, `0x01` Constructor) followed
/// by the payload. The Hash variant's payload is a bare 32 bytes;
/// the Constructor variant's payload is an 8-byte little-endian
/// length followed by the script bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActorID {
    /// Canonical 32-byte hash of the constructor script.
    Hash([u8; 32]),

    /// Constructor script bytes. Hashes to the same canonical
    /// id as `Hash(H_{flamevm.actorid}(bytes))`.
    Constructor(Vec<u8>),
}

impl ActorID {
    /// Tag value for [`ActorID::Hash`] on the wire.
    pub const TAG_HASH: u8 = 0x00;
    /// Tag value for [`ActorID::Constructor`] on the wire.
    pub const TAG_CONSTRUCTOR: u8 = 0x01;

    /// Returns this id's canonical 32-byte hash. Both enum variants
    /// resolve to the **same** value when they refer to the same
    /// actor — that's the equivalence invariant from the type docs.
    ///
    /// - [`ActorID::Hash`] returns the hash directly (free).
    /// - [`ActorID::Constructor`] hashes the bytes under
    ///   [`ACTOR_ID_DOMAIN`].
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

    /// Returns this id in its compact `Hash` form. For
    /// [`ActorID::Hash`] this is a cheap clone; for
    /// [`ActorID::Constructor`] it hashes the bytes and wraps.
    /// Use this when you want to compare ids by canonical value
    /// without keeping the constructor bytes around.
    pub fn to_canonical(&self) -> ActorID {
        match self {
            ActorID::Hash(_) => self.clone(),
            ActorID::Constructor(_) => ActorID::Hash(self.to_hash()),
        }
    }

    /// True iff this id is in the compact (`Hash`) form. Both forms
    /// resolve to the same canonical hash, so this is a wire-shape
    /// query, not an identity query.
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

// ── Lifecycle constants ───────────────────────────────────────────

/// Per-block introduction of fresh vbytes into the pool. Per
/// design.md §Resources / Storage and ADR 0004: 5000/block,
/// adjustable up to 2× per cycle by supermajority. The constant
/// here is the genesis value.
pub const VBYTES_PER_BLOCK: u64 = 5000;

/// Maturity delay for vbytes returning to the pool from a cleared
/// actor. 100 blocks per design.md §Resources / Storage and ADR
/// 0005.
pub const VBYTE_MATURITY_BLOCKS: u64 = 100;

/// Cap on the grace window in blocks (≈ six months at one
/// block per ~6s). Per ADR 0005:
/// `grace = min(active_blocks / 4, blocks_per_6_months)`. The
/// "six months" interpretation is consensus-fixed; the constant
/// here picks 2,628,000 blocks (= 6 × 30 × 24 × 60 × 60 / 6),
/// which is the working assumption. Validators will reconcile
/// against the actual block cadence at protocol-launch time.
pub const GRACE_BLOCKS_CAP: u64 = 2_628_000;

/// Grace-window formula. Returns the number of blocks an actor
/// stays frozen before being cleared. Per ADR 0005:
/// `min(active_blocks / 4, GRACE_BLOCKS_CAP)`.
pub fn grace_window(active_blocks: u64) -> u64 {
    let earned = active_blocks / 4;
    earned.min(GRACE_BLOCKS_CAP)
}

// ── VbytePool ────────────────────────────────────────────────────

/// Protocol-level vbyte supply. New vbytes flow in at
/// [`VBYTES_PER_BLOCK`] per block; cleared actors' vbytes flow
/// back via the maturity queue with a [`VBYTE_MATURITY_BLOCKS`]
/// delay. Available vbytes are consumed by external transactions
/// purchasing storage via fees (the consensus layer brokers that
/// transfer; this module just tracks the pool).
#[derive(Debug, Default)]
pub struct VbytePool {
    /// Vbytes currently available for purchase.
    pub available: u64,

    /// Maturity queue keyed by the block height at which the
    /// entry becomes spendable (= clear_height + VBYTE_MATURITY_BLOCKS).
    /// Inserted by [`VbytePool::queue_recycle`]; drained by
    /// [`VbytePool::release_matured`].
    pub maturing: std::collections::BTreeMap<u64, u64>,
}

impl VbytePool {
    /// Constructs an empty pool. Real chains seed with the genesis
    /// vbyte introduction; tests build piecewise.
    pub fn new() -> Self {
        Self::default()
    }

    /// Per-block introduction of fresh vbytes. Adds
    /// [`VBYTES_PER_BLOCK`] to `available`. Called once per block
    /// by [`MemRegistry::tick_block`] (real consensus impls do the
    /// same).
    pub fn introduce_block_vbytes(&mut self) {
        self.available = self.available.saturating_add(VBYTES_PER_BLOCK);
    }

    /// Queues `amount` vbytes recycled from a cleared actor for
    /// release at `cleared_at_height + VBYTE_MATURITY_BLOCKS`.
    pub fn queue_recycle(&mut self, amount: u64, cleared_at_height: u64) {
        if amount == 0 {
            return;
        }
        let release_height =
            cleared_at_height.saturating_add(VBYTE_MATURITY_BLOCKS);
        *self.maturing.entry(release_height).or_insert(0) =
            self.maturing
                .get(&release_height)
                .copied()
                .unwrap_or(0)
                .saturating_add(amount);
    }

    /// Releases any maturing entries whose release height has
    /// arrived (`<= current_height`). Returns the amount released.
    pub fn release_matured(&mut self, current_height: u64) -> u64 {
        // Drain entries with key ≤ current_height. BTreeMap doesn't
        // have a `drain_filter` on stable, so we split off the
        // strictly-larger half and accumulate the remainder.
        let upper = self.maturing.split_off(&(current_height + 1));
        let mut total = 0u64;
        for (_h, v) in self.maturing.iter() {
            total = total.saturating_add(*v);
        }
        self.maturing = upper;
        self.available = self.available.saturating_add(total);
        total
    }

    /// True iff no vbytes are available or maturing.
    pub fn is_empty(&self) -> bool {
        self.available == 0 && self.maturing.is_empty()
    }
}

// ── ActorRegistry trait ──────────────────────────────────────────

/// Mutable handle into the live actor registry. The VM consults
/// this from `op_load` / `op_save` / `op_call` / `op_send` (Units
/// 5–8). The trait stays thin so a real consensus-backed
/// implementation only needs to supply storage + the per-block
/// tick.
///
/// **Re-entrancy lock semantics.** `mark_for_destruction` is the
/// runtime enforcement of the load/save lock from
/// `flamevm/CLAUDE.md`. `op_load` marks; `op_save` unmarks; a
/// second `op_load` against a marked actor errors
/// `LoadAlreadyMarked`. If a transaction commits with an actor
/// still marked, the registry deletes the actor and recycles its
/// vbytes via the pool — that's the "load without save = destroy"
/// path (Q6).
///
/// **Lifecycle ownership.** `tick_block` is the sole transition
/// site for ACTIVE↔FROZEN↔CLEARED + maturity. Consensus calls it
/// once per block after applying that block's transactions.
pub trait ActorRegistry {
    // ── lookup ─────────────────────────────────────────────────

    /// Returns a cloned snapshot of the actor's state. `op_load`
    /// pushes this onto the stack; `op_call` uses it to resolve
    /// the callee's method bytes without retaining a borrow.
    fn load_state(&mut self, id: &ActorID) -> Result<ActorState, VMError>;

    /// Persists `state` against `id`. Re-sizes the actor's vbyte
    /// occupancy under the new state (the lifecycle ticker will
    /// reconcile balance on the next block).
    fn save_state(&mut self, id: &ActorID, state: ActorState) -> Result<(), VMError>;

    /// Resolves a method's script bytes. Equivalent to
    /// `load_state(id)?.resolve_method(method)?` but exists as a
    /// distinct call so dispatch can skip the per-call state clone
    /// for the common case where the callee only runs its method
    /// (no `load`/`save`).
    fn resolve_method(
        &self,
        actor: &ActorID,
        method: MethodKey,
    ) -> Result<Vec<u8>, VMError>;

    /// Returns the actor's persistent vbyte balance. Used by the
    /// VM driver to size the transient-memory cap (`4× persistent`
    /// per ADR 0002).
    fn actor_vbytes(&self, actor: &ActorID) -> Result<u64, VMError>;

    /// True iff a row exists in the registry for `actor`. Used by
    /// `op_call` / `op_send` to distinguish "actor doesn't exist"
    /// (hard fail for direct calls; deploy path for sends via
    /// `Constructor`) from frozen/loaded states.
    fn exists(&self, actor: &ActorID) -> bool;

    // ── re-entrancy lock + self-destruct (Q6) ──────────────────

    /// Marks the actor as currently loaded. Subsequent loads error
    /// `LoadAlreadyMarked`. Called by `op_load`.
    fn mark_for_destruction(&mut self, id: &ActorID);

    /// Clears the mark set by [`Self::mark_for_destruction`].
    /// Called by `op_save` after a successful save.
    fn unmark_for_destruction(&mut self, id: &ActorID);

    /// True iff the actor is currently marked. Used by the
    /// re-entrancy check and the tx-end self-destruct sweep.
    fn is_marked_for_destruction(&self, id: &ActorID) -> bool;

    /// End-of-tx hook called by the VM driver after a successful
    /// run. Walks marks; any still-marked actor is removed and its
    /// vbytes recycled to the pool with [`VBYTE_MATURITY_BLOCKS`]
    /// delay. Returns the number of actors cleared (useful for
    /// telemetry/tests). `current_height` is the height of the
    /// containing block.
    fn commit_tx_destructions(&mut self, current_height: u64) -> usize;

    // ── deployment (Q4 — transparent on first delivery) ────────

    /// Installs a freshly-deployed actor under `id`, funded with
    /// `vbytes`, activated at `height`. Errors `ActorAlreadyExists`
    /// if the id is already taken.
    fn deploy(
        &mut self,
        id: ActorID,
        state: ActorState,
        vbytes: u64,
        height: u64,
    ) -> Result<(), VMError>;

    /// Credits `amount` vbytes to `id`. If the actor was frozen,
    /// this restores it to ACTIVE, clears `frozen_since`, and resets
    /// `active_blocks` per ADR 0005's "top-up resets the counter".
    /// `last_activation_height` is updated to `current_height`.
    /// Errors `ActorNotFound` if no such actor.
    ///
    /// Called by the consensus side of `op_send` when delivering
    /// a vbyte-bearing message. An empty send (no method, no args,
    /// vbytes-only) is the dedicated transfer form per spec.md.
    fn credit_vbytes(
        &mut self,
        id: &ActorID,
        amount: u64,
        current_height: u64,
    ) -> Result<(), VMError>;

    // ── lifecycle ──────────────────────────────────────────────

    /// Per-block tick: bleed every actor's vbytes by their current
    /// `vbyte_size`, transition ACTIVE↔FROZEN, expire FROZEN past
    /// grace, and release matured pool entries. Called once per
    /// block by consensus after applying transactions.
    ///
    /// Returns the set of `ActorID`s cleared (expired past grace)
    /// during this tick, in deterministic order — useful for log
    /// emission. Per design.md §Internal-transaction grace.
    fn tick_block(&mut self, height: u64) -> Vec<ActorID>;

    // ── pool ───────────────────────────────────────────────────

    /// Read-only view of the protocol's vbyte pool.
    fn vbyte_pool(&self) -> &VbytePool;
}

// ── MemRegistry (in-memory ActorRegistry, for tests) ─────────────

/// `BTreeMap`-backed registry implementing [`ActorRegistry`].
/// Suitable for tests, fixtures, and the consensus crate's
/// reference implementation before persistent storage lands.
///
/// **Canonical-key invariant**: storage is keyed by the canonical
/// `[u8;32]` hash, not by the `ActorID` enum directly. Every
/// trait method canonicalizes the inbound id via
/// [`ActorID::to_hash`] before looking up, so callers can pass
/// either the `Hash` or the `Constructor` variant for the same
/// actor and reach the same entry.
pub struct MemRegistry {
    actors: std::collections::BTreeMap<[u8; 32], Actor>,
    marks: std::collections::BTreeSet<[u8; 32]>,
    pool: VbytePool,
}

impl MemRegistry {
    /// Constructs an empty registry with an empty pool.
    pub fn new() -> Self {
        Self {
            actors: std::collections::BTreeMap::new(),
            marks: std::collections::BTreeSet::new(),
            pool: VbytePool::new(),
        }
    }

    /// Mutable accessor for the underlying pool — useful for tests
    /// that want to seed `available` directly.
    pub fn pool_mut(&mut self) -> &mut VbytePool {
        &mut self.pool
    }

    /// Mutable accessor for the actor map — used by tests to set
    /// up scenarios. Not part of the trait surface.
    pub fn actor_mut(&mut self, id: &ActorID) -> Option<&mut Actor> {
        self.actors.get_mut(&id.to_hash())
    }

    /// Immutable accessor for an actor record. Convenience for tests.
    pub fn actor(&self, id: &ActorID) -> Option<&Actor> {
        self.actors.get(&id.to_hash())
    }
}

impl Default for MemRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActorRegistry for MemRegistry {
    fn load_state(&mut self, id: &ActorID) -> Result<ActorState, VMError> {
        let actor = self
            .actors
            .get(&id.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        if actor.is_frozen() {
            return Err(VMError::ActorFrozen);
        }
        // Clone the state (Dicts implement try_clone for portable
        // payloads; the load path requires payloads to be portable
        // since they're being moved across the wire / VM boundary).
        let public = actor
            .state
            .public
            .try_clone()
            .map_err(|_| VMError::MalformedActorState)?;
        let private = actor
            .state
            .private
            .try_clone()
            .map_err(|_| VMError::MalformedActorState)?;
        Ok(ActorState { public, private })
    }

    fn save_state(
        &mut self,
        id: &ActorID,
        state: ActorState,
    ) -> Result<(), VMError> {
        let actor = self
            .actors
            .get_mut(&id.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        actor.state = state;
        Ok(())
    }

    fn resolve_method(
        &self,
        actor: &ActorID,
        method: MethodKey,
    ) -> Result<Vec<u8>, VMError> {
        let a = self
            .actors
            .get(&actor.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        if a.is_frozen() {
            return Err(VMError::ActorFrozen);
        }
        let script = a
            .state
            .resolve_method(&method)
            .ok_or(VMError::MethodNotFound)?;
        Ok(script.to_bytes_vec())
    }

    fn actor_vbytes(&self, actor: &ActorID) -> Result<u64, VMError> {
        let a = self
            .actors
            .get(&actor.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        Ok(a.vbytes)
    }

    fn exists(&self, actor: &ActorID) -> bool {
        self.actors.contains_key(&actor.to_hash())
    }

    fn mark_for_destruction(&mut self, id: &ActorID) {
        self.marks.insert(id.to_hash());
    }

    fn unmark_for_destruction(&mut self, id: &ActorID) {
        self.marks.remove(&id.to_hash());
    }

    fn is_marked_for_destruction(&self, id: &ActorID) -> bool {
        self.marks.contains(&id.to_hash())
    }

    fn commit_tx_destructions(&mut self, current_height: u64) -> usize {
        let to_clear: Vec<[u8; 32]> =
            self.marks.iter().copied().collect();
        let mut count = 0usize;
        for id in to_clear {
            // Drain the mark regardless of whether the actor still
            // exists (defensive: a re-entrancy mark on a vanished
            // actor shouldn't linger).
            self.marks.remove(&id);
            if let Some(actor) = self.actors.remove(&id) {
                self.pool.queue_recycle(actor.vbytes, current_height);
                count += 1;
            }
        }
        count
    }

    fn deploy(
        &mut self,
        id: ActorID,
        state: ActorState,
        vbytes: u64,
        height: u64,
    ) -> Result<(), VMError> {
        let key = id.to_hash();
        if self.actors.contains_key(&key) {
            return Err(VMError::ActorAlreadyExists);
        }
        self.actors
            .insert(key, Actor::new_active(state, vbytes, height));
        Ok(())
    }

    fn credit_vbytes(
        &mut self,
        id: &ActorID,
        amount: u64,
        current_height: u64,
    ) -> Result<(), VMError> {
        let actor = self
            .actors
            .get_mut(&id.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        actor.vbytes = actor.vbytes.saturating_add(amount);
        if actor.is_frozen() {
            actor.frozen_since = None;
            actor.active_blocks = 0;
            actor.last_activation_height = current_height;
        }
        Ok(())
    }

    fn tick_block(&mut self, height: u64) -> Vec<ActorID> {
        // 1) introduce per-block vbytes + release matured.
        self.pool.introduce_block_vbytes();
        self.pool.release_matured(height);

        // 2) walk actors: bleed, transition, expire.
        let mut cleared: Vec<ActorID> = Vec::new();

        // Collect keys first to avoid an aliased mutable iter.
        let ids: Vec<[u8; 32]> = self.actors.keys().copied().collect();
        for id in ids {
            let actor = match self.actors.get_mut(&id) {
                Some(a) => a,
                None => continue,
            };
            match actor.frozen_since {
                None => {
                    // ACTIVE: bleed by current vbyte_size.
                    let occupied = match vbyte_size(&actor.state) {
                        Ok(n) => n,
                        Err(_) => {
                            // Defensive: malformed state cleared on
                            // tick — the registry only accepts
                            // well-formed states at deploy/save, so
                            // hitting this is an invariant break.
                            cleared.push(ActorID::Hash(id));
                            continue;
                        }
                    };
                    if actor.vbytes >= occupied {
                        actor.vbytes -= occupied;
                        actor.active_blocks =
                            actor.active_blocks.saturating_add(1);
                    } else {
                        // Bleeding to zero — exhausted within this tick.
                        actor.vbytes = 0;
                    }
                    if actor.vbytes == 0 {
                        actor.frozen_since = Some(height);
                    }
                }
                Some(frozen_at) => {
                    // FROZEN: count elapsed; expire past grace.
                    let elapsed = height.saturating_sub(frozen_at);
                    if elapsed >= grace_window(actor.active_blocks) {
                        // Clear the actor and recycle vbytes (in
                        // practice vbytes == 0 here, but be defensive
                        // for callers that mutated the field).
                        let recycled = actor.vbytes;
                        cleared.push(ActorID::Hash(id));
                        self.actors.remove(&id);
                        if recycled > 0 {
                            self.pool.queue_recycle(recycled, height);
                        }
                    }
                }
            }
        }

        cleared
    }

    fn vbyte_pool(&self) -> &VbytePool {
        &self.pool
    }
}
