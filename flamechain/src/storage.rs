//! Actor storage leases and the concrete FlameVM actor registry.

use std::collections::{BTreeMap, BTreeSet};

use flamevm::{
    ActorID, ActorRegistry, Scalar, StoragePurchase, VMError, Value, code_root,
    code_state_bytes, empty_state, state_root,
};
use merkle::{Hash, MerkleItem, MerkleTree};
use merlin::Transcript;

/// Number of sparks in one Flame.
pub const SPARKS_PER_FLAME: u64 = 100_000_000;
/// Initial storage market parameters. They are immutable consensus inputs to a
/// [`crate::Blockchain`]; changing them requires a versioned activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageParams {
    pub unit_bytes: u64,
    pub initial_pool_units: u64,
    pub lease_duration_blocks: u64,
    pub issued_units_per_block: u64,
    pub minimum_lease_units: u64,
    pub minimum_remaining_units: u64,
    pub lease_record_bytes: u64,
    pub initial_price_sparks_per_unit: u64,
}

impl Default for StorageParams {
    fn default() -> Self {
        Self {
            unit_bytes: 1_024,
            initial_pool_units: 128 * 1_024 * 1_024 / 1_024,
            lease_duration_blocks: 52_500,
            issued_units_per_block: 8,
            minimum_lease_units: 1,
            minimum_remaining_units: 1,
            lease_record_bytes: 16,
            initial_price_sparks_per_unit: 10 * SPARKS_PER_FLAME,
        }
    }
}

impl StorageParams {
    pub fn validate(self) -> Result<Self, StorageError> {
        if self.unit_bytes == 0
            || self.initial_pool_units <= self.minimum_remaining_units
            || self.lease_duration_blocks == 0
            || self.minimum_lease_units == 0
            || self.minimum_remaining_units == 0
            || self.initial_price_sparks_per_unit == 0
        {
            return Err(StorageError::InvalidParameters);
        }
        self.price_product()?;
        Ok(self)
    }

    pub fn price_product(self) -> Result<u128, StorageError> {
        u128::from(self.initial_pool_units)
            .checked_mul(u128::from(self.initial_price_sparks_per_unit))
            .ok_or(StorageError::ArithmeticOverflow)
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    #[error("invalid storage parameters")]
    InvalidParameters,
    #[error("storage arithmetic overflow")]
    ArithmeticOverflow,
    #[error("storage supply invariant failed")]
    SupplyInvariant,
    #[error("storage expiry index does not match actor leases")]
    ExpiryIndexInvariant,
}

/// One coalesced actor lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub expiry_height: u64,
    pub units: u64,
}

pub(crate) struct DestroyedActor {
    pub actor: ActorID,
    pub state: Value,
}

#[derive(Clone)]
struct LiveActor {
    code: Vec<u8>,
    state: Option<Value>,
    state_bytes: u64,
    code_root: [u8; 32],
    state_root: [u8; 32],
}

#[derive(Clone, Default)]
struct ActorSlot {
    live: Option<LiveActor>,
    leases: BTreeMap<u64, u64>,
}

#[derive(Clone, Default)]
pub(crate) struct RegistryUndo {
    pool: Option<u64>,
    actors: BTreeMap<[u8; 32], Option<ActorSlot>>,
    expiries: BTreeMap<u64, Option<BTreeSet<[u8; 32]>>>,
    pending: Option<BTreeSet<[u8; 32]>>,
    checked_out: Option<BTreeSet<[u8; 32]>>,
}

/// Concrete in-memory actor registry. Persistence can encode this state later;
/// consensus execution depends only on its deterministic methods.
#[derive(Clone)]
pub(crate) struct ActorStore {
    params: StorageParams,
    actors: BTreeMap<[u8; 32], ActorSlot>,
    expiries: BTreeMap<u64, BTreeSet<[u8; 32]>>,
    available_units: u64,
    pending_destruction: BTreeSet<[u8; 32]>,
    checked_out: BTreeSet<[u8; 32]>,
    checkpoints: Vec<RegistryUndo>,
}

impl ActorStore {
    pub(crate) fn new(params: StorageParams) -> Result<Self, StorageError> {
        let params = params.validate()?;
        Ok(Self {
            available_units: params.initial_pool_units,
            params,
            actors: BTreeMap::new(),
            expiries: BTreeMap::new(),
            pending_destruction: BTreeSet::new(),
            checked_out: BTreeSet::new(),
            checkpoints: Vec::new(),
        })
    }

    pub(crate) fn available_units(&self) -> u64 {
        self.available_units
    }

    pub(crate) fn push_outer_checkpoint(&mut self) {
        self.push_checkpoint();
    }

    pub(crate) fn take_outer_checkpoint(&mut self) -> RegistryUndo {
        self.checkpoints.pop().expect("outer checkpoint is open")
    }

    pub(crate) fn rollback_outer_checkpoint(&mut self) {
        let undo = self.checkpoints.pop().expect("outer checkpoint is open");
        self.apply_undo(undo);
    }

    pub(crate) fn apply_undo(&mut self, undo: RegistryUndo) {
        if let Some(pool) = undo.pool {
            self.available_units = pool;
        }
        if let Some(pending) = undo.pending {
            self.pending_destruction = pending;
        }
        if let Some(checked_out) = undo.checked_out {
            self.checked_out = checked_out;
        }
        for (height, prior) in undo.expiries {
            match prior {
                Some(bucket) => {
                    self.expiries.insert(height, bucket);
                }
                None => {
                    self.expiries.remove(&height);
                }
            }
        }
        for (id, prior) in undo.actors {
            match prior {
                Some(actor) => {
                    self.actors.insert(id, actor);
                }
                None => {
                    self.actors.remove(&id);
                }
            }
        }
    }

    fn record_pool(&mut self) {
        if let Some(undo) = self.checkpoints.last_mut() {
            undo.pool.get_or_insert(self.available_units);
        }
    }

    fn record_pending(&mut self) {
        if let Some(undo) = self.checkpoints.last_mut() {
            undo.pending
                .get_or_insert_with(|| self.pending_destruction.clone());
        }
    }

    fn record_checked_out(&mut self) {
        if let Some(undo) = self.checkpoints.last_mut() {
            undo.checked_out
                .get_or_insert_with(|| self.checked_out.clone());
        }
    }

    fn record_actor(&mut self, id: [u8; 32]) {
        let Self {
            actors,
            checkpoints,
            ..
        } = self;
        if let Some(undo) = checkpoints.last_mut() {
            undo.actors
                .entry(id)
                .or_insert_with(|| actors.get(&id).cloned());
        }
    }

    fn record_expiry(&mut self, height: u64) {
        let Self {
            expiries,
            checkpoints,
            ..
        } = self;
        if let Some(undo) = checkpoints.last_mut() {
            undo.expiries
                .entry(height)
                .or_insert_with(|| expiries.get(&height).cloned());
        }
    }

    pub(crate) fn begin_block(&mut self, height: u64) -> Result<(), VMError> {
        if !self.pending_destruction.is_empty() || !self.checked_out.is_empty() {
            return Err(VMError::ActorPendingDestruction);
        }

        self.record_expiry(height);
        let expired = self.expiries.remove(&height).unwrap_or_default();
        let mut recycled = 0u64;
        let mut empty_tombstones = Vec::new();
        let mut newly_pending = BTreeSet::new();
        for id in expired {
            self.record_actor(id);
            let remove_tombstone = {
                let slot = self
                    .actors
                    .get_mut(&id)
                    .ok_or(VMError::StorageArithmeticOverflow)?;
                recycled = recycled
                    .checked_add(
                        slot.leases
                            .remove(&height)
                            .ok_or(VMError::StorageArithmeticOverflow)?,
                    )
                    .ok_or(VMError::StorageArithmeticOverflow)?;
                slot.live.is_none() && slot.leases.is_empty()
            };
            if remove_tombstone {
                empty_tombstones.push(id);
            } else if self.actors[&id].live.is_some()
                && self.usage_slot(&self.actors[&id])?
                    > self.capacity_slot(&self.actors[&id], height)?
            {
                newly_pending.insert(id);
            }
        }
        for id in empty_tombstones {
            self.actors.remove(&id);
        }

        self.record_pool();
        self.available_units = self
            .available_units
            .checked_add(recycled)
            .and_then(|v| v.checked_add(self.params.issued_units_per_block))
            .ok_or(VMError::StorageArithmeticOverflow)?;

        if !newly_pending.is_empty() {
            self.record_pending();
            self.pending_destruction = newly_pending;
        }
        Ok(())
    }

    pub(crate) fn destroy_expired_actors(&mut self) -> Result<Vec<DestroyedActor>, VMError> {
        if self.pending_destruction.is_empty() {
            return Ok(Vec::new());
        }
        self.record_pending();
        let ids = std::mem::take(&mut self.pending_destruction);
        let mut destroyed = Vec::with_capacity(ids.len());
        for id in ids {
            self.record_actor(id);
            if let Some(slot) = self.actors.get_mut(&id) {
                let live = slot.live.take().ok_or(VMError::ActorNotFound)?;
                let state = live.state.ok_or(VMError::ActorEmpty)?;
                destroyed.push(DestroyedActor {
                    actor: ActorID::Hash(id),
                    state,
                });
                if slot.leases.is_empty() {
                    self.actors.remove(&id);
                }
            }
        }
        Ok(destroyed)
    }

    pub(crate) fn assert_supply(&self, height: u64) -> Result<(), StorageError> {
        let issued = height
            .checked_mul(self.params.issued_units_per_block)
            .and_then(|v| v.checked_add(self.params.initial_pool_units))
            .ok_or(StorageError::ArithmeticOverflow)?;
        let leased = self
            .actors
            .values()
            .flat_map(|slot| slot.leases.values())
            .try_fold(0u64, |sum, units| sum.checked_add(*units))
            .ok_or(StorageError::ArithmeticOverflow)?;
        if self.available_units.checked_add(leased) != Some(issued) {
            return Err(StorageError::SupplyInvariant);
        }

        let mut expected = BTreeMap::<u64, BTreeSet<[u8; 32]>>::new();
        for (&id, slot) in &self.actors {
            for &expiry in slot.leases.keys() {
                expected.entry(expiry).or_default().insert(id);
            }
        }
        if expected != self.expiries {
            return Err(StorageError::ExpiryIndexInvariant);
        }
        if !self.checked_out.is_empty() || !self.pending_destruction.is_empty() {
            return Err(StorageError::SupplyInvariant);
        }
        Ok(())
    }

    fn require_live(&self, id: [u8; 32]) -> Result<&ActorSlot, VMError> {
        if self.pending_destruction.contains(&id) {
            return Err(VMError::ActorPendingDestruction);
        }
        let slot = self.actors.get(&id).ok_or(VMError::ActorNotFound)?;
        if slot.live.is_none() {
            return Err(VMError::ActorNotFound);
        }
        Ok(slot)
    }

    fn state_bytes(code: &[u8], state: &Value) -> Result<u64, VMError> {
        code_state_bytes(code, state)?
            .checked_sub(code.len() as u64)
            .ok_or(VMError::StorageArithmeticOverflow)
    }

    fn usage_slot(&self, slot: &ActorSlot) -> Result<u64, VMError> {
        let live = slot.live.as_ref().ok_or(VMError::ActorNotFound)?;
        (live.code.len() as u64)
            .checked_add(live.state_bytes)
            .and_then(|v| {
                (slot.leases.len() as u64)
                    .checked_mul(self.params.lease_record_bytes)
                    .and_then(|metadata| v.checked_add(metadata))
            })
            .ok_or(VMError::StorageArithmeticOverflow)
    }

    fn capacity_slot(&self, slot: &ActorSlot, height: u64) -> Result<u64, VMError> {
        let Some(first_valid_expiry) = height.checked_add(1) else {
            return Ok(0);
        };
        let units = slot
            .leases
            .range(first_valid_expiry..)
            .try_fold(0u64, |sum, (_, units)| sum.checked_add(*units))
            .ok_or(VMError::StorageArithmeticOverflow)?;
        units
            .checked_mul(self.params.unit_bytes)
            .ok_or(VMError::StorageArithmeticOverflow)
    }

    fn quote(
        &self,
        actor: &ActorID,
        bytes: u64,
        height: u64,
    ) -> Result<Option<StoragePurchase>, VMError> {
        let slot = self.require_live(actor.to_hash())?;
        if !bytes.is_multiple_of(self.params.unit_bytes) {
            return Ok(None);
        }
        let units = bytes / self.params.unit_bytes;
        if units < self.params.minimum_lease_units {
            return Ok(None);
        }
        let Some(after) = self.available_units.checked_sub(units) else {
            return Ok(None);
        };
        if after < self.params.minimum_remaining_units {
            return Ok(None);
        }
        let expiry_height = height
            .checked_add(self.params.lease_duration_blocks)
            .ok_or(VMError::StorageArithmeticOverflow)?;
        if slot
            .leases
            .get(&expiry_height)
            .copied()
            .unwrap_or(0)
            .checked_add(units)
            .is_none()
        {
            return Ok(None);
        }
        let product = self
            .params
            .price_product()
            .map_err(|_| VMError::StorageArithmeticOverflow)?;
        let numerator = u128::from(units)
            .checked_mul(product)
            .ok_or(VMError::StorageArithmeticOverflow)?;
        let divisor = u128::from(after);
        let fee = numerator / divisor + u128::from(numerator % divisor != 0);
        if fee == 0 {
            return Err(VMError::StorageArithmeticOverflow);
        }
        Ok(Some(StoragePurchase {
            fee_sparks: Scalar::from(fee),
            expiry_height,
        }))
    }

    pub(crate) fn actor_root(&self) -> Hash {
        MerkleTree::root(
            b"flamechain.actors",
            self.actors.iter().map(|(&id, slot)| ActorLeaf {
                id,
                live: slot.live.as_ref().map(|live| {
                    (
                        live.code_root,
                        live.state_root,
                        live.code.len() as u64,
                        live.state_bytes,
                    )
                }),
                leases: slot.leases.iter().map(|(&h, &u)| (h, u)).collect(),
            }),
        )
    }

    pub(crate) fn replay_deploy(&mut self, actor: ActorID, code: Vec<u8>) -> Result<(), VMError> {
        self.deploy(actor, code, empty_state())
    }

    pub(crate) fn replay_save(
        &mut self,
        actor: &ActorID,
        state: Value,
        height: u64,
    ) -> Result<(), VMError> {
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        let key = actor.to_hash();
        let code = self.require_live(key)?.live.as_ref().unwrap().code.clone();
        let state_bytes = Self::state_bytes(&code, &state)?;
        let root = state_root(&state);
        self.record_actor(key);
        let live = self.actors.get_mut(&key).unwrap().live.as_mut().unwrap();
        live.state = Some(state);
        live.state_bytes = state_bytes;
        live.state_root = root;
        self.validate_actor_storage(actor, height)
    }

    pub(crate) fn replay_set_code(
        &mut self,
        actor: &ActorID,
        code: Vec<u8>,
        height: u64,
    ) -> Result<(), VMError> {
        self.set_code(actor, code)?;
        self.validate_actor_storage(actor, height)
    }

    pub(crate) fn replay_destroy(&mut self, actor: &ActorID) -> Result<(), VMError> {
        let key = actor.to_hash();
        self.record_actor(key);
        let slot = self.actors.get_mut(&key).ok_or(VMError::ActorNotFound)?;
        let live = slot.live.take().ok_or(VMError::ActorNotFound)?;
        if live.state.is_none() {
            return Err(VMError::ActorEmpty);
        }
        let remove_slot = slot.leases.is_empty();
        if remove_slot {
            self.actors.remove(&key);
        }
        if self.pending_destruction.contains(&key) {
            self.record_pending();
            self.pending_destruction.remove(&key);
        }
        Ok(())
    }
}

struct ActorLeaf {
    id: [u8; 32],
    live: Option<([u8; 32], [u8; 32], u64, u64)>,
    leases: Vec<(u64, u64)>,
}

impl MerkleItem for ActorLeaf {
    fn commit(&self, t: &mut Transcript) {
        t.append_message(b"actor.id", &self.id);
        match self.live {
            Some((code, state, code_bytes, state_bytes)) => {
                t.append_message(b"actor.live", &[1]);
                t.append_message(b"actor.code_root", &code);
                t.append_message(b"actor.state_root", &state);
                t.append_message(b"actor.code_bytes", &code_bytes.to_le_bytes());
                t.append_message(b"actor.state_bytes", &state_bytes.to_le_bytes());
            }
            None => t.append_message(b"actor.live", &[0]),
        }
        t.append_message(
            b"actor.lease_count",
            &(self.leases.len() as u64).to_le_bytes(),
        );
        for (expiry, units) in &self.leases {
            t.append_message(b"actor.lease_expiry", &expiry.to_le_bytes());
            t.append_message(b"actor.lease_units", &units.to_le_bytes());
        }
    }
}

impl ActorRegistry for ActorStore {
    fn load_state(&mut self, id: &ActorID) -> Result<Value, VMError> {
        let key = id.to_hash();
        self.require_live(key)?;
        self.record_actor(key);
        let state = self
            .actors
            .get_mut(&key)
            .and_then(|slot| slot.live.as_mut())
            .and_then(|live| live.state.take())
            .ok_or(VMError::ActorEmpty)?;
        self.record_checked_out();
        self.checked_out.insert(key);
        Ok(state)
    }

    fn save_state(&mut self, id: &ActorID, state: Value) -> Result<(), VMError> {
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        let key = id.to_hash();
        let code = self.require_live(key)?.live.as_ref().unwrap().code.clone();
        if self.actors[&key].live.as_ref().unwrap().state.is_some() {
            return Err(VMError::SaveWithoutLoad);
        }
        let state_bytes = Self::state_bytes(&code, &state)?;
        let root = state_root(&state);
        self.record_actor(key);
        let live = self.actors.get_mut(&key).unwrap().live.as_mut().unwrap();
        live.state = Some(state);
        live.state_bytes = state_bytes;
        live.state_root = root;
        self.record_checked_out();
        self.checked_out.remove(&key);
        Ok(())
    }

    fn load_code(&self, actor: &ActorID) -> Result<Vec<u8>, VMError> {
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        if live.state.is_none() {
            return Err(VMError::ActorEmpty);
        }
        Ok(live.code.clone())
    }

    fn actor_code_bytes(&self, actor: &ActorID) -> Result<u64, VMError> {
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        if live.state.is_none() {
            return Err(VMError::ActorEmpty);
        }
        Ok(live.code.len() as u64)
    }

    fn actor_state_bytes(&self, actor: &ActorID) -> Result<u64, VMError> {
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        if live.state.is_none() {
            return Err(VMError::ActorEmpty);
        }
        Ok(live.state_bytes)
    }

    fn set_code(&mut self, actor: &ActorID, code: Vec<u8>) -> Result<(), VMError> {
        let key = actor.to_hash();
        self.require_live(key)?;
        self.record_actor(key);
        let live = self.actors.get_mut(&key).unwrap().live.as_mut().unwrap();
        live.code_root = code_root(&code);
        live.code = code;
        Ok(())
    }

    fn actor_usage(&self, actor: &ActorID) -> Result<u64, VMError> {
        self.usage_slot(self.require_live(actor.to_hash())?)
    }

    fn actor_capacity(&self, actor: &ActorID, height: u64) -> Result<u64, VMError> {
        self.capacity_slot(self.require_live(actor.to_hash())?, height)
    }

    fn quote_storage(
        &self,
        actor: &ActorID,
        bytes: u64,
        current_height: u64,
    ) -> Result<Option<StoragePurchase>, VMError> {
        self.quote(actor, bytes, current_height)
    }

    fn purchase_storage(
        &mut self,
        actor: &ActorID,
        bytes: u64,
        current_height: u64,
    ) -> Result<Option<StoragePurchase>, VMError> {
        let Some(purchase) = self.quote(actor, bytes, current_height)? else {
            return Ok(None);
        };
        let units = bytes / self.params.unit_bytes;
        let key = actor.to_hash();
        self.record_pool();
        self.record_actor(key);
        self.record_expiry(purchase.expiry_height);
        self.available_units = self
            .available_units
            .checked_sub(units)
            .ok_or(VMError::StorageArithmeticOverflow)?;
        let current = self.actors[&key]
            .leases
            .get(&purchase.expiry_height)
            .copied()
            .unwrap_or(0);
        self.actors.get_mut(&key).unwrap().leases.insert(
            purchase.expiry_height,
            current
                .checked_add(units)
                .ok_or(VMError::StorageArithmeticOverflow)?,
        );
        self.expiries
            .entry(purchase.expiry_height)
            .or_default()
            .insert(key);
        Ok(Some(purchase))
    }

    fn validate_actor_storage(&self, actor: &ActorID, height: u64) -> Result<(), VMError> {
        let slot = self.require_live(actor.to_hash())?;
        if self.usage_slot(slot)? <= self.capacity_slot(slot, height)? {
            Ok(())
        } else {
            Err(VMError::StorageCapacityExceeded)
        }
    }

    fn exists(&self, actor: &ActorID) -> bool {
        self.actors
            .get(&actor.to_hash())
            .and_then(|slot| slot.live.as_ref())
            .is_some()
    }

    fn push_checkpoint(&mut self) {
        self.checkpoints.push(RegistryUndo::default());
    }

    fn pop_checkpoint_commit(&mut self) {
        let Some(child) = self.checkpoints.pop() else {
            return;
        };
        let Some(parent) = self.checkpoints.last_mut() else {
            return;
        };
        if parent.pool.is_none() {
            parent.pool = child.pool;
        }
        if parent.pending.is_none() {
            parent.pending = child.pending;
        }
        if parent.checked_out.is_none() {
            parent.checked_out = child.checked_out;
        }
        for (id, prior) in child.actors {
            parent.actors.entry(id).or_insert(prior);
        }
        for (height, prior) in child.expiries {
            parent.expiries.entry(height).or_insert(prior);
        }
    }

    fn pop_checkpoint_rollback(&mut self) {
        if let Some(undo) = self.checkpoints.pop() {
            self.apply_undo(undo);
        }
    }

    fn commit_tx_destructions(&mut self) -> Vec<ActorID> {
        self.record_checked_out();
        let ids: Vec<_> = std::mem::take(&mut self.checked_out).into_iter().collect();
        for id in &ids {
            self.record_actor(*id);
            let slot = self.actors.get_mut(id).unwrap();
            slot.live = None;
            if slot.leases.is_empty() {
                self.actors.remove(id);
            }
        }
        ids.into_iter().map(ActorID::Hash).collect()
    }

    fn deploy(&mut self, id: ActorID, code: Vec<u8>, state: Value) -> Result<(), VMError> {
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        let key = id.to_hash();
        if self.actors.contains_key(&key) {
            return Err(VMError::ActorAlreadyExists);
        }
        let state_bytes = Self::state_bytes(&code, &state)?;
        let live = LiveActor {
            code_root: code_root(&code),
            state_root: state_root(&state),
            code,
            state: Some(state),
            state_bytes,
        };
        self.record_actor(key);
        self.actors.insert(
            key,
            ActorSlot {
                live: Some(live),
                leases: BTreeMap::new(),
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flamevm::{
        ActorRegistry, ClearToken, Dict, FLAME_FLAVOR, Scalar, Value,
        empty_state,
    };

    fn actor() -> ActorID {
        ActorID::Hash([7; 32])
    }

    #[test]
    fn canonical_actor_root_vector() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store
            .deploy(actor(), vec![0x1d], Value::Scalar(Scalar::from(42u64)))
            .unwrap();
        store.purchase_storage(&actor(), 1_024, 0).unwrap().unwrap();
        assert_eq!(
            hex::encode(store.actor_root().0),
            "2694dc0a474475efe48096f89f41ad71bbadd7694021fc6ede5b3397d967f572"
        );
    }

    fn small_params() -> StorageParams {
        StorageParams {
            unit_bytes: 64,
            initial_pool_units: 10,
            lease_duration_blocks: 2,
            issued_units_per_block: 1,
            minimum_lease_units: 1,
            minimum_remaining_units: 1,
            lease_record_bytes: 2,
            initial_price_sparks_per_unit: 3,
        }
    }

    fn nested_nonportable_state() -> Value {
        let mut inner = Dict::new();
        inner.insert(
            Scalar::ZERO,
            Value::ClearToken(ClearToken::new(Scalar::from(-1i64), FLAME_FLAVOR)),
        );
        let mut outer = Dict::new();
        outer.insert(Scalar::ZERO, Value::Dict(inner));
        Value::Dict(outer)
    }

    #[test]
    fn actor_state_boundaries_reject_nested_nonportable_dicts() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        assert!(matches!(
            store.deploy(actor(), vec![0], nested_nonportable_state()),
            Err(VMError::NonPortableInState)
        ));

        store.deploy(actor(), vec![0], empty_state()).unwrap();
        store.load_state(&actor()).unwrap();
        assert!(matches!(
            store.save_state(&actor(), nested_nonportable_state()),
            Err(VMError::NonPortableInState)
        ));
        assert!(matches!(store.load_state(&actor()), Err(VMError::ActorEmpty)));
    }

    #[test]
    fn quote_rounding_and_pool_boundaries_are_exact() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        let quote = store
            .quote_storage(&actor(), 1_024, 10)
            .unwrap()
            .unwrap();
        assert_eq!(quote.fee_sparks, Scalar::from(1_000_007_630u64));
        assert_eq!(quote.expiry_height, 52_510);
        let params = StorageParams::default();
        let largest = (params.initial_pool_units - params.minimum_remaining_units)
            * params.unit_bytes;
        assert_eq!(largest, 134_216_704);
        assert_eq!(
            store
                .quote_storage(&actor(), largest, 0)
                .unwrap()
                .unwrap()
                .fee_sparks,
            Scalar::from(17_179_738_112_000_000_000u64)
        );
        assert_eq!(
            store
                .quote_storage(
                    &actor(),
                    params.initial_pool_units * params.unit_bytes,
                    0,
                )
                .unwrap(),
            None
        );

        let params = small_params();
        let mut store = ActorStore::new(params).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        assert_eq!(store.quote_storage(&actor(), 0, 0).unwrap(), None);
        assert_eq!(store.quote_storage(&actor(), 63, 0).unwrap(), None);
        assert_eq!(
            store
                .quote_storage(&actor(), 9 * params.unit_bytes, 0)
                .unwrap()
                .unwrap()
                .fee_sparks,
            Scalar::from(270u64)
        );
        assert_eq!(
            store.quote_storage(&actor(), 10 * params.unit_bytes, 0).unwrap(),
            None
        );
        store
            .purchase_storage(&actor(), 9 * params.unit_bytes, 0)
            .unwrap()
            .unwrap();
        assert_eq!(store.available_units(), 1);
        assert_eq!(
            store
                .quote_storage(&actor(), params.unit_bytes, 0)
                .unwrap(),
            None
        );
        store.assert_supply(0).unwrap();
    }

    #[test]
    fn leases_coalesce_expire_and_destroy_at_exact_heights() {
        let params = small_params();
        let state = Value::ClearToken(ClearToken::new(Scalar::from(7u64), FLAME_FLAVOR));
        let state_hash = state_root(&state);
        let mut store = ActorStore::new(params).unwrap();
        store.deploy(actor(), vec![0], state).unwrap();
        let base_usage = store.actor_usage(&actor()).unwrap();

        let first = store
            .purchase_storage(&actor(), params.unit_bytes, 0)
            .unwrap()
            .unwrap();
        let second = store
            .purchase_storage(&actor(), 2 * params.unit_bytes, 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            (first.fee_sparks, first.expiry_height),
            (Scalar::from(4u64), 2)
        );
        assert_eq!(
            (second.fee_sparks, second.expiry_height),
            (Scalar::from(9u64), 2)
        );
        assert_eq!(
            store.actors[&actor().to_hash()].leases,
            BTreeMap::from([(2, 3)])
        );
        assert_eq!(store.actor_usage(&actor()).unwrap(), base_usage + 2);
        assert_eq!(store.actor_capacity(&actor(), 1).unwrap(), 192);
        assert_eq!(store.actor_capacity(&actor(), 2).unwrap(), 0);

        let third = store
            .purchase_storage(&actor(), params.unit_bytes, 1)
            .unwrap()
            .unwrap();
        assert_eq!(
            (third.fee_sparks, third.expiry_height),
            (Scalar::from(5u64), 3)
        );
        assert_eq!(
            store.actors[&actor().to_hash()].leases,
            BTreeMap::from([(2, 3), (3, 1)])
        );
        assert_eq!(store.actor_usage(&actor()).unwrap(), base_usage + 4);
        assert_eq!(store.actor_capacity(&actor(), 2).unwrap(), 64);

        store.begin_block(1).unwrap();
        store.begin_block(2).unwrap();
        assert_eq!(store.available_units(), 11);
        assert_eq!(
            store.actors[&actor().to_hash()].leases,
            BTreeMap::from([(3, 1)])
        );
        assert_eq!(store.actor_capacity(&actor(), 2).unwrap(), 64);
        store.assert_supply(2).unwrap();

        store.begin_block(3).unwrap();
        assert!(matches!(
            store.actor_capacity(&actor(), 3),
            Err(VMError::ActorPendingDestruction)
        ));
        let destroyed = store.destroy_expired_actors().unwrap();
        assert_eq!(destroyed.len(), 1);
        assert_eq!(destroyed[0].actor, actor());
        assert_eq!(state_root(&destroyed[0].state), state_hash);
        assert!(!store.exists(&actor()));
        assert_eq!(store.available_units(), 13);
        store.assert_supply(3).unwrap();
    }

    #[test]
    fn nested_purchase_commit_is_undone_by_outer_rollback() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        let other = ActorID::Hash([8; 32]);
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        store.deploy(other.clone(), vec![0], empty_state()).unwrap();
        let before_pool = store.available_units();
        let before_root = store.actor_root();

        store.push_checkpoint();
        store.purchase_storage(&actor(), 1_024, 0).unwrap().unwrap();
        store.push_checkpoint();
        store.purchase_storage(&other, 2_048, 0).unwrap().unwrap();
        store.pop_checkpoint_commit();
        assert_eq!(store.available_units(), before_pool - 3);

        store.pop_checkpoint_rollback();
        assert_eq!(store.available_units(), before_pool);
        assert_eq!(store.actor_root(), before_root);
        assert_eq!(store.actor_capacity(&actor(), 0).unwrap(), 0);
        assert_eq!(store.actor_capacity(&other, 0).unwrap(), 0);
        store.assert_supply(0).unwrap();
    }

    #[test]
    fn nested_expiry_and_destruction_commit_is_undone_by_outer_rollback() {
        let params = small_params();
        let mut store = ActorStore::new(params).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        store
            .purchase_storage(&actor(), params.unit_bytes, 0)
            .unwrap()
            .unwrap();
        let before_pool = store.available_units();
        let before_root = store.actor_root();
        let before_expiries = store.expiries.clone();

        store.push_checkpoint();
        store.begin_block(1).unwrap();
        store.begin_block(2).unwrap();
        store.push_checkpoint();
        assert_eq!(store.destroy_expired_actors().unwrap().len(), 1);
        store.pop_checkpoint_commit();
        assert!(!store.exists(&actor()));

        store.pop_checkpoint_rollback();
        assert!(store.exists(&actor()));
        assert_eq!(store.available_units(), before_pool);
        assert_eq!(store.actor_root(), before_root);
        assert_eq!(store.expiries, before_expiries);
        assert_eq!(
            store.actor_capacity(&actor(), 0).unwrap(),
            params.unit_bytes
        );
        store.assert_supply(0).unwrap();
    }

    #[test]
    fn expiry_recycles_without_redeploying_tombstone() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        store.purchase_storage(&actor(), 1_024, 0).unwrap().unwrap();
        store.load_state(&actor()).unwrap();
        assert_eq!(store.commit_tx_destructions(), vec![actor()]);
        assert!(matches!(
            store.deploy(actor(), vec![0], empty_state()),
            Err(VMError::ActorAlreadyExists)
        ));
        store
            .begin_block(StorageParams::default().lease_duration_blocks)
            .unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
    }

    #[test]
    fn expiry_index_is_checked_against_committed_leases() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        store.purchase_storage(&actor(), 1_024, 0).unwrap().unwrap();
        store.expiries.clear();
        assert_eq!(
            store.assert_supply(0),
            Err(StorageError::ExpiryIndexInvariant)
        );
    }
}
