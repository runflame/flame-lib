//! Actor data model: identity, state, lifecycle counters, registry.

use cells::{
    BagOfCells, CellBuilder, CellDecode, CellEncode, CellError, CellRef, CellResolver, CellSlice,
};
use std::sync::Arc;

use crate::dict::Dict;
use crate::encoding::{blob_cell, read_blob};
use crate::errors::VMError;
use crate::value::Value;

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
/// `Hash(h).to_hash()` whenever `h == code_root(bytes)`.
/// Both addresses route to the same actor; the registry
/// canonicalizes on the hash so callers can use whichever form
/// they have on hand.
///
/// Wire form: a tag byte (`0x00` Hash, `0x01` Constructor) followed
/// by the payload. The Hash variant's payload is a bare 32 bytes;
/// the Constructor variant has one reference to snake-encoded code.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActorID {
    /// Canonical 32-byte hash of the constructor script.
    Hash([u8; 32]),

    /// Constructor script bytes. Hashes to the same canonical
    /// id as `Hash(code_root(bytes))`.
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
            ActorID::Constructor(bytes) => code_root(bytes),
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
}

/// Canonical wire form (tag byte + payload). `Hash` writes a 32-byte
/// payload; `Constructor` stores one reference to snake-encoded code.
impl CellEncode for ActorID {
    fn encode(&self, w: &mut CellBuilder) -> Result<(), CellError> {
        match self {
            ActorID::Hash(h) => {
                w.store_u8(Self::TAG_HASH)?.store_bytes(h)?;
            }
            ActorID::Constructor(bytes) => {
                w.store_u8(Self::TAG_CONSTRUCTOR)?;
                w.store_ref(CellRef::resident(blob_cell(bytes)?))?;
            }
        }
        Ok(())
    }
}

impl CellDecode for ActorID {
    fn decode<R: CellResolver + ?Sized>(
        r: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        let tag = r.load_u8()?;
        match tag {
            Self::TAG_HASH => {
                let h = <[u8; 32]>::decode(r, cells)?;
                Ok(ActorID::Hash(h))
            }
            Self::TAG_CONSTRUCTOR => {
                let bytes = read_blob(&r.load_ref()?, cells, u32::MAX as usize)?;
                Ok(ActorID::Constructor(bytes))
            }
            _ => Err(CellError::InvalidFormat),
        }
    }
}

// ── Actor state + code helpers ───────────────────────────────────
//
// An actor is `(code, state)`: a single bytecode blob (set by `setcode`,
// dispatched on the `method` opcode) plus an opaque state `Value` the
// author structures however they like. `op_load` / `op_save` move the
// state; `state_root` / `code_root` are the canonical commitments for
// `TxEntry::ActorSave` / `TxEntry::SetCode`. See ADR 0018.

/// A neutral empty state — an empty Dict. State may be **any** portable
/// `Value`; this is just a convenient default for deploy / tests.
pub fn empty_state() -> Value {
    Value::Dict(Dict::new())
}

/// Canonical state Cell ID, referenced by `TxEntry::ActorSave`.
/// Infallible in valid registry
/// context: op_save gates stored states on portability, and all
/// portable values are wire-encodable.
pub fn state_root(state: &Value) -> [u8; 32] {
    state
        .to_cell()
        .expect("admitted actor state is encodable")
        .id()
}

/// Canonical snake Cell ID of an actor's code, referenced by `TxEntry::SetCode`.
pub fn code_root(code: &[u8]) -> [u8; 32] {
    blob_cell(code)
        .expect("actor code fits the byte-string format")
        .id()
}

// ── Actor data sizing ─────────────────────────────────────────────

/// Canonical charged size of an actor's code and state. Lease-record overhead
/// belongs to the blockchain host because FlameVM does not own lease records.
///
/// Returns an error if the state can't be encoded. Note:
/// encodability ≠ portability — op_save's `is_portable` check is the
/// authoritative storage gate; this reports the rare case of a portable
/// value that lacks an encoder.
pub fn code_state_bytes(code: &[u8], state: &Value) -> Result<u64, VMError> {
    let mut graph = BagOfCells::collect(Arc::new(blob_cell(code)?))?;
    graph.extend(&BagOfCells::collect(Arc::new(state.to_cell()?))?)?;
    let size = graph.iter().try_fold(0u64, |size, (_, cell)| {
        size.checked_add(cell.encoded_size() as u64)
            .ok_or(VMError::StorageArithmeticOverflow)
    });
    size
}

/// Result of a successful persistent-storage purchase. The host owns
/// pricing and lease bookkeeping; the VM only turns this deterministic
/// result into a debt token and transaction-log effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoragePurchase {
    pub fee_sparks: crate::Scalar,
    pub expiry_height: u64,
}

// ── ActorRegistry trait ──────────────────────────────────────────

/// Mutable handle into actor state owned by the blockchain state machine.
/// FlameVM never owns the global lease pool or block lifecycle; it only asks
/// this host for deterministic actor reads and mutations while executing one
/// transaction.
///
pub trait ActorRegistry {
    /// Bodies committed as resident for this actor, never a node-global cache.
    fn actor_cells(&self, _id: &ActorID) -> Result<Arc<BagOfCells>, VMError> {
        Ok(Arc::new(BagOfCells::new()))
    }

    /// Execution-only recovery from the initiating transaction's immutable BoC.
    /// Implementations must not implicitly persist externally supplied bodies.
    fn load_code_with_cells(
        &self,
        id: &ActorID,
        _cells: &mut dyn CellResolver,
    ) -> Result<Vec<u8>, VMError> {
        self.load_code(id)
    }

    fn load_state_with_cells(
        &mut self,
        id: &ActorID,
        _cells: &mut dyn CellResolver,
    ) -> Result<Value, VMError> {
        self.load_state(id)
    }

    // ── lookup ─────────────────────────────────────────────────

    /// **Checks out** the actor's state — moves it out of the registry
    /// (leaving the actor `None`/empty) and returns it for `op_load` to
    /// push onto the stack. Errors `ActorEmpty` if it's already checked
    /// out (the re-entrancy lock), or `ActorNotFound`. The matching
    /// `save_state` moves it back.
    fn load_state(&mut self, id: &ActorID) -> Result<Value, VMError>;

    /// Moves `state` back into a **checked-out** actor (`op_save`).
    /// Errors `SaveWithoutLoad` if the actor isn't checked out (saving
    /// would clobber live, possibly token-bearing, state). The caller
    /// ensures `state` is portable (op_save checks first).
    fn save_state(&mut self, id: &ActorID, state: Value) -> Result<(), VMError>;

    /// Returns the actor's code blob, method-agnostic — the code itself
    /// dispatches on the `method` opcode. Errors `ActorNotFound` or
    /// `ActorEmpty` (checked out → re-entrancy block).
    fn load_code(&self, actor: &ActorID) -> Result<Vec<u8>, VMError>;

    /// Code bytes that [`Self::load_code`] would activate. Used to debit gas
    /// before the host clones or decodes the code.
    fn actor_code_bytes(&self, actor: &ActorID) -> Result<u64, VMError>;

    /// Canonical state bytes that [`Self::load_state`] would activate. Used to
    /// debit gas before checkout or deserialization.
    fn actor_state_bytes(&self, actor: &ActorID) -> Result<u64, VMError>;

    /// Replaces the actor's code blob (`setcode`). Errors `ActorNotFound`.
    fn set_code(&mut self, actor: &ActorID, code: Vec<u8>) -> Result<(), VMError>;

    /// Charged bytes currently occupied by code, committed state, and lease
    /// records. While state is checked out this remains the committed usage.
    fn actor_usage(&self, actor: &ActorID) -> Result<u64, VMError>;

    /// Leased bytes available at `height`. A past height and an actor pending
    /// block-boundary destruction are hard errors.
    fn actor_capacity(&self, actor: &ActorID, height: u64) -> Result<u64, VMError>;

    /// Quotes `bytes` without changing state. `None` is the storage opcodes'
    /// in-band unavailable result; arithmetic or invariant failures are hard
    /// errors.
    fn quote_storage(
        &self,
        actor: &ActorID,
        bytes: u64,
        current_height: u64,
    ) -> Result<Option<StoragePurchase>, VMError>;

    /// Atomically buys a one-year lease for `actor`. The implementation must
    /// use the same checks and quote as [`Self::quote_storage`].
    fn purchase_storage(
        &mut self,
        actor: &ActorID,
        bytes: u64,
        current_height: u64,
    ) -> Result<Option<StoragePurchase>, VMError>;

    /// Checks the post-mutation `usage <= capacity(current_height)` invariant.
    /// This is also the final gate for a provisionally deployed actor.
    fn validate_actor_storage(&self, actor: &ActorID, current_height: u64) -> Result<(), VMError>;

    /// True iff a row exists in the registry for `actor`. Used by
    /// `op_call` / message delivery to distinguish "actor doesn't exist"
    /// (hard fail for direct calls; deploy path for constructor messages)
    /// from a temporarily checked-out state.
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
    /// Registry-owned pool, lease, and expiry-index mutations are part of
    /// the same checkpoint and must be restored as well.
    fn pop_checkpoint_rollback(&mut self);

    // ── self-destruct (Q6 / ADR 0017) ──────────────────────────

    /// End-of-tx hook for explicit self-destruction: removes actors whose
    /// checked-out state was fully dismantled and returns their ids in
    /// canonical order. Unexpired leases remain owned by the global expiry
    /// index and are not refunded.
    fn commit_tx_destructions(&mut self) -> Vec<ActorID>;

    // ── deployment (Q4 — transparent on first delivery) ────────

    /// Installs a provisional actor with no lease. Its constructor may buy
    /// storage; the containing transaction commits only after
    /// [`Self::validate_actor_storage`] succeeds.
    fn deploy(&mut self, id: ActorID, code: Vec<u8>, state: Value) -> Result<(), VMError>;
}
