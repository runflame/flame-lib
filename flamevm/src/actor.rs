//! Actor data model: identity, state, lifecycle counters, registry.

use merlin::Transcript;
use readerwriter::{Decodable, Encodable, ReadError, Reader, WriteError, Writer};

use crate::dict::Dict;
use crate::encoding::write_dict;
use crate::errors::VMError;
use crate::int253::Int253;
use crate::string::String;
use crate::value::Value;

/// Reserved method key in the actor-state `public` sub-Dict.
/// Dispatched for every inbound message send (`recv`); other keys
/// are reachable only via `op_call` between actors.
pub const RECV_METHOD: Int253 = Int253::ZERO;

/// Reserved top-level Dict keys for an actor's state. `public#0x00`
/// holds the method table; `private#0x01` holds the actor's data.
pub const ACTOR_STATE_PUBLIC_KEY_RAW: u64 = 0x00;
pub const ACTOR_STATE_PRIVATE_KEY_RAW: u64 = 0x01;

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

    /// Returns this id's canonical 32-byte hash. Both variants resolve
    /// to the **same** value (the equivalence invariant from the type docs).
    pub fn to_hash(&self) -> [u8; 32] {
        match self {
            ActorID::Hash(h) => *h,
            ActorID::Constructor(bytes) => {
                let mut t = Transcript::new(b"flamevm.actorid");
                t.append_message(b"constructor", bytes);
                let mut h = [0u8; 32];
                t.challenge_bytes(b"id", &mut h);
                h
            }
        }
    }

    /// Returns this id in its compact `Hash` form. Use this to compare
    /// ids by canonical value without keeping the constructor bytes around.
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
}

/// Canonical wire form (tag byte + payload). `Hash` writes a 32-byte
/// payload; `Constructor` writes an 8-byte little-endian length then
/// the script bytes.
impl Encodable for ActorID {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
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
}

impl Decodable for ActorID {
    fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
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
}

// ── Actor state helpers (state IS a Dict) ────────────────────────
//
// The actor's state is a plain `Dict` with the conventional shape
// `{ 0x00 → public_dict, 0x01 → private_dict }` (see spec.md §Actors).
// Scripts construct it on the stack via `dict` / `put`, push it back
// via `op_save`, and observe it via `op_load`. No wrapper struct —
// the convention is encoded directly in the Dict's contents.
//
// The two helpers below navigate the convention from registry-side
// code (`resolve_method` for method dispatch) and provide a canonical
// commitment hash (`state_root` for `TxEntry::ActorSave`'s merkle leaf).

/// Constructs an empty actor state: a Dict with `{0x00 → empty_dict,
/// 0x01 → empty_dict}`. Used by tests and the bootstrap deploy path.
pub fn empty_state() -> Dict {
    let mut s = Dict::new();
    s.insert(
        Int253::from(ACTOR_STATE_PUBLIC_KEY_RAW),
        Value::Dict(Dict::new()),
    );
    s.insert(
        Int253::from(ACTOR_STATE_PRIVATE_KEY_RAW),
        Value::Dict(Dict::new()),
    );
    s
}

/// Constructs an actor state with a pre-populated public Dict and an
/// empty private Dict. Convenience for tests that only care about
/// the method table.
pub fn state_with_public(public: Dict) -> Dict {
    let mut s = Dict::new();
    s.insert(Int253::from(ACTOR_STATE_PUBLIC_KEY_RAW), Value::Dict(public));
    s.insert(
        Int253::from(ACTOR_STATE_PRIVATE_KEY_RAW),
        Value::Dict(Dict::new()),
    );
    s
}

/// Looks up a method script in the canonical state shape:
/// `state[0x00 = public][method]`. Returns `None` if the state isn't
/// in canonical shape, the method key is absent, or the value at the
/// method key isn't a `String`.
pub fn resolve_method<'a>(state: &'a Dict, method: &Int253) -> Option<&'a String> {
    match state.get(&Int253::from(ACTOR_STATE_PUBLIC_KEY_RAW))? {
        Value::Dict(public) => match public.get(method)? {
            Value::String(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

/// Canonical 32-byte commitment to an actor's state Dict — the
/// `TxEntry::ActorSave` merkle leaf.
///
/// Infallible in valid registry context (op_save gates stored states on
/// portability). Panics on a portable-but-unencodable value — see the
/// architect's queue item on the `ClearToken`-vs-encoder gap.
pub fn state_root(state: &Dict) -> [u8; 32] {
    let mut buf = Vec::new();
    write_dict(&mut buf, state)
        .expect("actor state in valid registry context is wire-encodable");
    let mut t = Transcript::new(b"flamevm.actor.state.root");
    t.append_message(b"state", &buf);
    let mut h = [0u8; 32];
    t.challenge_bytes(b"root", &mut h);
    h
}

// ── Actor (full record, including lifecycle counters) ────────────

/// Full per-actor record stored in the registry. Combines the
/// mutable script-visible state Dict with the protocol-managed
/// lifecycle counters (vbyte balance, activation tracking, freeze
/// state). Per `flamevm/spec.md` §Storage and ADR 0005.
#[derive(Clone, Debug)]
pub struct Actor {
    /// Mutable script-visible state, or `None` while **checked out**
    /// — `op_load` moves the state onto a frame's stack (leaving
    /// `None`), `op_save` moves it back. A checked-out actor has no
    /// code or data, so calls/loads against it fail `ActorEmpty`: the
    /// state's presence *is* the re-entrancy lock (ADR 0017). Canonical
    /// shape when present is `{0x00 → public_dict, 0x01 → private_dict}`.
    pub state: Option<Dict>,

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
    pub fn new_active(state: Dict, vbytes: u64, height: u64) -> Self {
        Self {
            state: Some(state),
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

    /// True iff the state is checked out (a frame `load`ed it and
    /// hasn't `save`d it back). Such an actor — left empty at tx end
    /// because the frame dismantled its state instead of saving — is
    /// reaped (self-destruct, Q6).
    pub fn is_checked_out(&self) -> bool {
        self.state.is_none()
    }
}

// ── Vbyte sizing ──────────────────────────────────────────────────

/// Canonical vbyte size of an actor's state Dict (Q2): `wire_len(state) +
/// STORAGE_OVERHEAD`, the overhead covering the protocol-managed lifecycle
/// counters every actor carries.
///
/// `Err(MalformedActorState)` if the state can't be encoded. Note:
/// encodability ≠ portability — op_save's `is_portable` check is the
/// authoritative storage gate; this reports the rare case of a portable
/// value that lacks an encoder.
pub fn vbyte_size(state: &Dict) -> Result<u64, VMError> {
    const STORAGE_OVERHEAD: u64 = 32;
    let mut buf = Vec::new();
    write_dict(&mut buf, state).map_err(|_| VMError::MalformedActorState)?;
    Ok(buf.len() as u64 + STORAGE_OVERHEAD)
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

/// Cap on the grace window in blocks (≈ six months)
pub const GRACE_BLOCKS_CAP: u64 = 144*30*6;

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
#[derive(Clone, Debug, Default)]
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

    /// **Checks out** the actor's state — moves it out of the registry
    /// (leaving the actor `None`/empty) and returns it for `op_load` to
    /// push onto the stack. Errors `ActorEmpty` if it's already checked
    /// out (the re-entrancy lock), `ActorFrozen` if frozen, or
    /// `ActorNotFound`. The matching `save_state` moves it back.
    fn load_state(&mut self, id: &ActorID) -> Result<Dict, VMError>;

    /// Moves `state` back into a **checked-out** actor (`op_save`).
    /// Errors `SaveWithoutLoad` if the actor isn't checked out (saving
    /// would clobber live, possibly token-bearing, state). The caller
    /// ensures `state` is portable (op_save checks first).
    fn save_state(&mut self, id: &ActorID, state: Dict) -> Result<(), VMError>;

    /// Resolves a method's script bytes. Equivalent to
    /// `load_state(id)?.resolve_method(method)?` but exists as a
    /// distinct call so dispatch can skip the per-call state clone
    /// for the common case where the callee only runs its method
    /// (no `load`/`save`).
    fn resolve_method(
        &self,
        actor: &ActorID,
        method: Int253,
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

    // ── checkpoint / rollback (call-frame atomicity) ───────────

    /// Open an undo-log frame onto an internal stack. Called by the
    /// VM at every call-frame entry (and once per tx) so a failed
    /// sub-call can roll back any registry mutations the failed callee
    /// made — `op_save`/`op_load` moves (a checked-out state is just a
    /// `None` the undo restores to `Some`) and deploys. Pairs LIFO with
    /// [`Self::pop_checkpoint_commit`] / [`Self::pop_checkpoint_rollback`].
    fn push_checkpoint(&mut self);

    /// Pop the topmost undo frame and discard it (keep current state),
    /// merging its entries into the parent. Clean call-frame / tx exit.
    fn pop_checkpoint_commit(&mut self);

    /// Pop the topmost undo frame and replay it, restoring registry
    /// state. Called when a call frame fails (`VM::fail_current_call`)
    /// or the outermost tx fails — so a failed `load` restores the
    /// checked-out state (the actor isn't spuriously self-destructed).
    /// The vbyte pool isn't tracked (only tx-end / per-block hooks
    /// touch it).
    fn pop_checkpoint_rollback(&mut self);

    // ── self-destruct (Q6 / ADR 0017) ──────────────────────────

    /// End-of-tx hook called by the VM driver after a successful run.
    /// Reaps any actor left **checked out** (`state == None` — a `load`
    /// whose state the frame dismantled instead of saving), recycling
    /// its vbytes to the pool with [`VBYTE_MATURITY_BLOCKS`] delay.
    /// Returns the count cleared. `current_height` is the containing
    /// block's height.
    fn commit_tx_destructions(&mut self, current_height: u64) -> usize;

    // ── deployment (Q4 — transparent on first delivery) ────────

    /// Installs a freshly-deployed actor under `id`, funded with
    /// `vbytes`, activated at `height`. Errors `ActorAlreadyExists`
    /// if the id is already taken.
    fn deploy(
        &mut self,
        id: ActorID,
        state: Dict,
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
#[derive(Clone)]
pub struct MemRegistry {
    actors: std::collections::BTreeMap<[u8; 32], Actor>,
    pool: VbytePool,
    /// LIFO undo-log stack for call-frame / tx-level rollback.
    /// Pushed by `push_checkpoint`; consumed by `pop_checkpoint_commit`
    /// (merge into parent) or `pop_checkpoint_rollback` (replay).
    checkpoints: Vec<CheckpointFrame>,
}

/// One checkpoint frame's undo log. Rather than cloning the whole
/// registry on every call/open/load/save, each frame records the prior
/// value of every actor it mutates, on first touch — so a checkpoint
/// costs O(touched), not O(all actors). A checked-out state is just an
/// `Actor` whose `state` is `None`, so the single `actor_undo` map
/// covers load/save moves, deploys, and re-entrancy lock state
/// uniformly. The vbyte pool isn't tracked (only tx-end / per-block
/// hooks touch it, never mid-call).
#[derive(Clone)]
struct CheckpointFrame {
    /// actor id → prior record (`None` = absent before first touch).
    actor_undo: std::collections::BTreeMap<[u8; 32], Option<Actor>>,
}

impl MemRegistry {
    /// Constructs an empty registry with an empty pool.
    pub fn new() -> Self {
        Self {
            actors: std::collections::BTreeMap::new(),
            pool: VbytePool::new(),
            checkpoints: Vec::new(),
        }
    }

    /// Records the prior value of actor `h` into the open checkpoint
    /// (once per frame). No-op when no checkpoint is open. Split-borrow
    /// so the closure can read `actors` while `checkpoints` is held.
    fn record_actor(&mut self, h: [u8; 32]) {
        let Self { checkpoints, actors, .. } = self;
        if let Some(frame) = checkpoints.last_mut() {
            frame.actor_undo.entry(h).or_insert_with(|| actors.get(&h).cloned());
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
    fn load_state(&mut self, id: &ActorID) -> Result<Dict, VMError> {
        let h = id.to_hash();
        // Pre-checks on an immutable borrow before recording undo +
        // moving the state out.
        {
            let actor = self.actors.get(&h).ok_or(VMError::ActorNotFound)?;
            if actor.is_frozen() {
                return Err(VMError::ActorFrozen);
            }
            if actor.state.is_none() {
                // Already loaded by some live frame (the lock), or
                // destroyed — either way nothing to check out.
                return Err(VMError::ActorEmpty);
            }
        }
        self.record_actor(h);
        // Move the state out (the actor goes empty) — no clone; the
        // matching `save_state` moves it back.
        Ok(self.actors.get_mut(&h).unwrap().state.take().unwrap())
    }

    fn save_state(
        &mut self,
        id: &ActorID,
        state: Dict,
    ) -> Result<(), VMError> {
        let h = id.to_hash();
        // Only a checked-out actor can be saved to — otherwise we'd
        // clobber (and silently drop the tokens of) live state.
        match self.actors.get(&h) {
            None => return Err(VMError::ActorNotFound),
            Some(a) if !a.is_checked_out() => return Err(VMError::SaveWithoutLoad),
            Some(_) => {}
        }
        self.record_actor(h);
        self.actors.get_mut(&h).unwrap().state = Some(state);
        Ok(())
    }

    fn resolve_method(
        &self,
        actor: &ActorID,
        method: Int253,
    ) -> Result<Vec<u8>, VMError> {
        let a = self
            .actors
            .get(&actor.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        if a.is_frozen() {
            return Err(VMError::ActorFrozen);
        }
        // Checked-out actor has no code/data → can't dispatch. This is
        // the re-entrancy block for *calls* (ADR 0017).
        let state = a.state.as_ref().ok_or(VMError::ActorEmpty)?;
        let script = resolve_method(state, &method)
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

    fn push_checkpoint(&mut self) {
        self.checkpoints.push(CheckpointFrame {
            actor_undo: std::collections::BTreeMap::new(),
        });
    }

    fn pop_checkpoint_commit(&mut self) {
        // Merge this frame's undo entries into the parent so an outer
        // rollback can still undo what this frame changed; `or_insert`
        // keeps the parent's older prior where both touched the same
        // key. Drop them outright if this was the outermost frame.
        let Some(frame) = self.checkpoints.pop() else { return };
        if let Some(parent) = self.checkpoints.last_mut() {
            for (h, prior) in frame.actor_undo {
                parent.actor_undo.entry(h).or_insert(prior);
            }
        }
    }

    fn pop_checkpoint_rollback(&mut self) {
        let Some(frame) = self.checkpoints.pop() else { return };
        for (h, prior) in frame.actor_undo {
            match prior {
                Some(actor) => { self.actors.insert(h, actor); }
                None => { self.actors.remove(&h); }
            }
        }
    }

    fn commit_tx_destructions(&mut self, current_height: u64) -> usize {
        // Reap actors left checked out at tx end — a `load` whose
        // state the frame dismantled (recursively read out, tokens
        // retired, droppable residue dropped) instead of saving. A
        // failed `load` rolled its `None` back to `Some` already, so
        // only an intentional full dismantle survives to here.
        let to_clear: Vec<[u8; 32]> = self
            .actors
            .iter()
            .filter(|(_, a)| a.is_checked_out())
            .map(|(k, _)| *k)
            .collect();
        let mut count = 0usize;
        for id in to_clear {
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
        state: Dict,
        vbytes: u64,
        height: u64,
    ) -> Result<(), VMError> {
        let key = id.to_hash();
        if self.actors.contains_key(&key) {
            return Err(VMError::ActorAlreadyExists);
        }
        // Defensive: reject non-portable initial state — the same
        // gate op_save uses. Without this check, a buggy deploy
        // path could plant non-portable values that subsequent
        // load/snapshot paths can't round-trip.
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        self.record_actor(key);
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
                    let occupied = match actor.state.as_ref() {
                        // Checked out at a block boundary is an
                        // invariant break (txs end with state restored
                        // or the actor reaped); skip the bleed.
                        None => continue,
                        Some(state) => match vbyte_size(state) {
                            Ok(n) => n,
                            Err(_) => {
                                // Defensive: malformed state cleared on
                                // tick — the registry only accepts
                                // well-formed states at deploy/save, so
                                // hitting this is an invariant break.
                                cleared.push(ActorID::Hash(id));
                                continue;
                            }
                        },
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
