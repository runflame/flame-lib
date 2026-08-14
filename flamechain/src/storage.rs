//! Actor storage leases and the concrete FlameVM actor registry.

use std::collections::{BTreeMap, BTreeSet};

use flamevm::{
    ActorID, ActorRegistry, Int253, StoragePurchase, VMError, Value, code_root, state_root,
    vbyte_size,
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
    pub transient_memory_multiplier: u64,
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
            transient_memory_multiplier: 4,
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
            || self.transient_memory_multiplier == 0
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
        vbyte_size(code, state)?
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
            fee_sparks: Int253::from(fee),
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

    fn transient_memory_multiplier(&self) -> u64 {
        self.params.transient_memory_multiplier
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
    use flamevm::{ActorRegistry, empty_state};

    fn actor() -> ActorID {
        ActorID::Hash([7; 32])
    }

    #[test]
    fn purchase_rounds_up_and_rollback_restores_supply() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        store.push_checkpoint();
        let before = store.available_units();
        let quote = store
            .purchase_storage(&actor(), 1_024, 10)
            .unwrap()
            .unwrap();
        assert!(quote.fee_sparks.to_u128().unwrap() > 0);
        assert_eq!(store.available_units(), before - 1);
        store.pop_checkpoint_rollback();
        assert_eq!(store.available_units(), before);
        assert_eq!(store.actor_capacity(&actor(), 10).unwrap(), 0);
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
