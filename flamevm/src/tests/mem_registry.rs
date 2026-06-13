//! In-memory reference implementations of [`ActorRegistry`] and
//! [`Env`] — test scaffolding (the consensus crate provides the real,
//! persistent registry). Relocated out of the implementation per the
//! 1k-line reduction review.

use crate::actor::{
    grace_window, vbyte_size, Actor, ActorID, ActorRegistry, VbytePool
};
use crate::errors::VMError;
use crate::tx::{Env, TxEntry, TxLog};
use crate::value::Value;

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
    fn load_state(&mut self, id: &ActorID) -> Result<Value, VMError> {
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
        state: Value,
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

    fn load_code(&self, actor: &ActorID) -> Result<Vec<u8>, VMError> {
        let a = self
            .actors
            .get(&actor.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        if a.is_frozen() {
            return Err(VMError::ActorFrozen);
        }
        // Checked-out (state == None) → re-entrancy block for calls
        // (ADR 0017). Code is always present, so this is now an
        // explicit gate rather than a side effect of method lookup.
        if a.state.is_none() {
            return Err(VMError::ActorEmpty);
        }
        Ok(a.code.clone())
    }

    fn set_code(&mut self, actor: &ActorID, code: Vec<u8>) -> Result<(), VMError> {
        let h = actor.to_hash();
        match self.actors.get(&h) {
            None => return Err(VMError::ActorNotFound),
            Some(a) if a.is_frozen() => return Err(VMError::ActorFrozen),
            Some(_) => {}
        }
        self.record_actor(h);
        self.actors.get_mut(&h).unwrap().code = code;
        Ok(())
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
        code: Vec<u8>,
        state: Value,
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
            .insert(key, Actor::new_active(code, state, vbytes, height));
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
                        Some(state) => match vbyte_size(&actor.code, state) {
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

/// Reference [`Env`] backed by an in-memory [`MemRegistry`] at a fixed
/// block height. For tests and pre-storage node use.
pub struct MemEnv {
    pub registry: MemRegistry,
    pub height: u64,
}

impl Env for MemEnv {
    fn working_copy(&self) -> Box<dyn ActorRegistry> {
        Box::new(self.registry.clone())
    }
    fn height(&self) -> u64 {
        self.height
    }
    fn apply_changes(&mut self, log: &TxLog) {
        // First cut: replay actor-state saves onto existing actors.
        // Deploy / vbyte-credit / reaping flow is deferred — see
        // design.md §Transaction lifecycle & API (vbytes flow note).
        for entry in log.iter() {
            match entry {
                TxEntry::ActorSave { actor, state } => {
                    if let Some(a) = self.registry.actor_mut(actor) {
                        a.state = Some(state.clone());
                    }
                }
                TxEntry::SetCode { actor, code } => {
                    if let Some(a) = self.registry.actor_mut(actor) {
                        a.code = code.clone();
                    }
                }
                // Exhaustive on purpose: a newly-added effect variant must
                // force a decision here rather than be silently dropped.
                // The following are no-ops for this first-cut applier
                // (deploy/vbyte-credit/reaping deferred — vbytes-flow note).
                TxEntry::Header(_)
                | TxEntry::Data(_)
                | TxEntry::Input(_)
                | TxEntry::Receive(_)
                | TxEntry::Output(_)
                | TxEntry::IssuePub(_, _)
                | TxEntry::IssuePriv(_, _)
                | TxEntry::Retire(_, _)
                | TxEntry::Fee(_)
                | TxEntry::Send(_) => {}
            }
        }
    }
}
