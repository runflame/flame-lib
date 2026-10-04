//! Actor storage leases and the concrete FlameVM actor registry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cells::{
    BagOfCells, Cell, CellBuilder, CellDecode, CellEncode, CellEnvelope, CellError, CellID,
    CellRef, CellResolver, CellSlice, Trie, resolve_cell,
};

use flamevm::{
    ActorID, ActorRegistry, Scalar, StoragePurchase, VMError, Value, code_root, empty_state,
    state_root,
};

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

/// Actor content and its exact consensus-owned resident graph.
#[derive(Clone, Debug)]
pub struct StoredActor {
    pub root: CellID,
    pub cells: Arc<BagOfCells>,
}

/// A read-only view of an actor's committed content and storage at one height.
#[derive(Clone, Debug)]
pub struct ActorInfo {
    /// Root cell hash of the current code, even when its bytes are unavailable.
    pub code_root: CellID,
    /// Root cell hash of the current state, even when its bytes are unavailable.
    pub state_root: CellID,
    /// Logical bytecode length, excluding cell framing.
    pub code_size: u64,
    /// Logical state size in encoded cell bytes.
    pub state_size: u64,
    /// Resident code/state bytes plus lease records.
    pub storage_used: u64,
    /// Capacity of leases valid at the snapshot height, in bytes.
    pub storage_capacity: u64,
    /// Bytecode, if all its cells are resident.
    pub code: Option<Vec<u8>>,
    /// State root and reachable resident cells. Descendants may still be pruned.
    /// Absent if even the state root body is unavailable.
    pub state: Option<CellEnvelope>,
}

#[derive(Clone)]
struct LiveActor {
    code: Option<Vec<u8>>,
    code_bytes: u64,
    state: Option<Value>,
    state_bytes: u64,
    code_root: [u8; 32],
    state_root: [u8; 32],
    /// Only explicitly retained code/state bodies; transaction resolution never
    /// inserts into this bag. Registry/lease metadata is reconstructed separately.
    cells: Arc<BagOfCells>,
    resident_bytes: u64,
}

#[derive(Clone, Default)]
struct ActorSlot {
    live: Option<LiveActor>,
    leases: BTreeMap<u64, u64>,
}

impl CellEncode for Lease {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store_u64(self.expiry_height)?
            .store_u64(self.units)?;
        Ok(())
    }
}

impl CellDecode for Lease {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        _cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self {
            expiry_height: slice.load_u64()?,
            units: slice.load_u64()?,
        })
    }
}

impl CellEncode for ActorSlot {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_u8(u8::from(self.live.is_some()))?;
        if let Some(live) = &self.live {
            builder
                .store_u64(live.code_bytes)?
                .store_u64(live.state_bytes)?
                .store_ref(CellRef::pruned(live.code_root))?
                .store_ref(CellRef::pruned(live.state_root))?;
        }
        let mut leases = Trie::new(8)?;
        for (&expiry, &units) in &self.leases {
            leases.insert(&expiry.to_be_bytes(), units.to_cell()?, &mut ())?;
        }
        let mut lease_list = CellBuilder::new();
        lease_list
            .store_u32(u32::try_from(self.leases.len()).map_err(|_| CellError::LimitExceeded)?)?;
        if let Some(root) = leases.into_root() {
            lease_list.store_ref(root)?;
        }
        builder.store_ref(CellRef::resident(lease_list.build()))?;
        Ok(())
    }
}

/// Retain reachable resident bodies and bodies already owned by this actor.
/// External witnesses are deliberately not a source for persistent collection.
fn collect_owned(
    roots: impl IntoIterator<Item = Arc<Cell>>,
    prior: &BagOfCells,
) -> Result<BagOfCells, CellError> {
    let mut bag = BagOfCells::new();
    let mut visited = BTreeSet::new();
    let mut pending: Vec<_> = roots.into_iter().collect();
    while let Some(cell) = pending.pop() {
        if !visited.insert(Arc::as_ptr(&cell)) {
            continue;
        }
        for reference in cell.refs() {
            match reference {
                CellRef::Resident(child) => pending.push(Arc::clone(child)),
                CellRef::Pruned(id) => {
                    if let Some(child) = prior.get(id) {
                        pending.push(child);
                    }
                }
            }
        }
        bag.insert(cell)?;
    }
    Ok(bag)
}

impl LiveActor {
    fn retain_changes(&mut self) -> Result<(), CellError> {
        let code = match &self.code {
            Some(code) => {
                let mut builder = CellBuilder::new();
                builder.store_snake(code)?;
                CellRef::resident(builder.build())
            }
            None => CellRef::pruned(self.code_root),
        };
        let state = match &self.state {
            Some(state) => CellRef::resident(state.to_cell()?),
            None => CellRef::pruned(self.state_root),
        };
        let roots = [code, state]
            .into_iter()
            .filter_map(|reference| match reference {
                CellRef::Resident(cell) => Some(cell),
                CellRef::Pruned(id) => self.cells.get(&id),
            });
        let cells = collect_owned(roots, &self.cells)?;
        self.resident_bytes = cells.iter().try_fold(0u64, |size, (_, cell)| {
            size.checked_add(cell.encoded_size() as u64)
                .ok_or(CellError::LimitExceeded)
        })?;
        self.cells = Arc::new(cells);
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct RegistryUndo {
    pool: Option<u64>,
    actors: BTreeMap<[u8; 32], Option<ActorSlot>>,
    expiries: BTreeMap<u64, Option<BTreeSet<[u8; 32]>>>,
    pending: Option<BTreeSet<[u8; 32]>>,
    checked_out: Option<BTreeSet<[u8; 32]>>,
}

/// Concrete actor registry with Cell commitments and explicit body availability.
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

    pub(crate) fn freeze_expired_actors(&mut self) -> Result<(), VMError> {
        if self.pending_destruction.is_empty() {
            return Ok(());
        }
        self.record_pending();
        let ids = std::mem::take(&mut self.pending_destruction);
        for id in ids {
            self.record_actor(id);
            if let Some(slot) = self.actors.get_mut(&id) {
                let live = slot.live.as_mut().ok_or(VMError::ActorNotFound)?;
                // The hashes continue to own every linear value. Expiry only
                // removes body availability; it never retires the contents.
                live.code = None;
                live.state = None;
                live.cells = Arc::new(BagOfCells::new());
                live.resident_bytes = 0;
            }
        }
        Ok(())
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

    fn state_bytes(state: &Value) -> Result<u64, VMError> {
        BagOfCells::collect(Arc::new(state.to_cell()?))?
            .iter()
            .try_fold(0u64, |size, (_, cell)| {
                size.checked_add(cell.encoded_size() as u64)
                    .ok_or(VMError::StorageArithmeticOverflow)
            })
    }

    fn usage_slot(&self, slot: &ActorSlot) -> Result<u64, VMError> {
        let live = slot.live.as_ref().ok_or(VMError::ActorNotFound)?;
        live.resident_bytes
            .checked_add({
                (slot.leases.len() as u64)
                    .checked_mul(self.params.lease_record_bytes)
                    .ok_or(VMError::StorageArithmeticOverflow)?
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

    pub(crate) fn stored_actor(&self, actor: &ActorID) -> Result<StoredActor, VMError> {
        let slot = self.require_live(actor.to_hash())?;
        Self::stored_slot(slot).map_err(Into::into)
    }

    pub(crate) fn actor_info(&self, actor: &ActorID, height: u64) -> Result<ActorInfo, VMError> {
        let slot = self.require_live(actor.to_hash())?;
        let live = slot.live.as_ref().ok_or(VMError::ActorNotFound)?;

        // Read only committed public cells, never cached values or private witnesses.
        // In particular, load_state would check out the actor for execution.
        let mut cells = live.cells.as_ref().clone();
        let code = match self.load_code_with_cells(actor, &mut cells) {
            Ok(code) => Some(code),
            Err(VMError::Cell(CellError::MissingCell(_))) => None,
            Err(error) => return Err(error),
        };

        let state = live
            .cells
            .get(&live.state_root)
            .map(|root| CellEnvelope::new(live.state_root, collect_owned([root], &live.cells)?))
            .transpose()?;

        Ok(ActorInfo {
            code_root: live.code_root,
            state_root: live.state_root,
            code_size: live.code_bytes,
            state_size: live.state_bytes,
            storage_used: self.usage_slot(slot)?,
            storage_capacity: self.capacity_slot(slot, height)?,
            code,
            state,
        })
    }

    fn stored_slot(slot: &ActorSlot) -> Result<StoredActor, CellError> {
        let root = Arc::new(slot.to_cell()?);
        let empty = BagOfCells::new();
        let prior = slot
            .live
            .as_ref()
            .map_or(&empty, |live| live.cells.as_ref());
        let cells = Arc::new(collect_owned([Arc::clone(&root)], prior)?);
        Ok(StoredActor {
            root: root.id(),
            cells,
        })
    }

    pub(crate) fn actor_root(&self) -> CellID {
        let mut trie = Trie::new(32).expect("actor ID width");
        for (id, slot) in &self.actors {
            let graph = Self::stored_slot(slot).expect("admitted actor Cell graph");
            let mut entry = CellBuilder::new();
            entry
                .store_bytes(&graph.cells.id())
                .expect("availability ID fits")
                .store_ref(CellRef::pruned(graph.root))
                .expect("actor root reference fits");
            trie.insert(id, entry.build(), &mut ())
                .expect("resident actor trie");
        }
        let mut root = CellBuilder::new();
        root.store_u64(self.actors.len() as u64)
            .expect("actor count fits");
        if let Some(reference) = trie.into_root() {
            root.store_ref(reference).expect("one actor trie reference");
        }
        root.build().id()
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
        self.require_live(key)?;
        let state_bytes = Self::state_bytes(&state)?;
        let root = state_root(&state);
        self.record_actor(key);
        let live = self.actors.get_mut(&key).unwrap().live.as_mut().unwrap();
        live.state = Some(state);
        live.state_bytes = state_bytes;
        live.state_root = root;
        live.retain_changes()?;
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
        slot.live.take().ok_or(VMError::ActorNotFound)?;
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

impl ActorRegistry for ActorStore {
    fn load_state(&mut self, id: &ActorID) -> Result<Value, VMError> {
        let mut cells = self.actor_cells(id)?.as_ref().clone();
        self.load_state_with_cells(id, &mut cells)
    }

    fn load_state_with_cells(
        &mut self,
        id: &ActorID,
        cells: &mut dyn CellResolver,
    ) -> Result<Value, VMError> {
        let key = id.to_hash();
        let live = self.require_live(key)?.live.as_ref().unwrap();
        if self.checked_out.contains(&key) {
            return Err(VMError::ActorEmpty);
        }
        let cell = resolve_cell(cells, &CellRef::pruned(live.state_root))?;
        // Canonical storage reads never inherit private witnesses or loaded
        // Dict branches from an in-memory cache left by an earlier transaction.
        let state = Value::from_trusted_cell(&cell, cells)?;
        self.record_actor(key);
        self.actors
            .get_mut(&key)
            .unwrap()
            .live
            .as_mut()
            .unwrap()
            .state = None;
        self.record_checked_out();
        self.checked_out.insert(key);
        Ok(state)
    }

    fn save_state(&mut self, id: &ActorID, state: Value) -> Result<(), VMError> {
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        let key = id.to_hash();
        self.require_live(key)?;
        if !self.checked_out.contains(&key) {
            return Err(VMError::SaveWithoutLoad);
        }
        let state_bytes = Self::state_bytes(&state)?;
        let root = state_root(&state);
        self.record_actor(key);
        let live = self.actors.get_mut(&key).unwrap().live.as_mut().unwrap();
        live.state = Some(state);
        live.state_bytes = state_bytes;
        live.state_root = root;
        live.retain_changes()?;
        self.record_checked_out();
        self.checked_out.remove(&key);
        Ok(())
    }

    fn load_code(&self, actor: &ActorID) -> Result<Vec<u8>, VMError> {
        let mut cells = self.actor_cells(actor)?.as_ref().clone();
        self.load_code_with_cells(actor, &mut cells)
    }

    fn load_code_with_cells(
        &self,
        actor: &ActorID,
        cells: &mut dyn CellResolver,
    ) -> Result<Vec<u8>, VMError> {
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        if self.checked_out.contains(&actor.to_hash()) {
            return Err(VMError::ActorEmpty);
        }
        let cell = resolve_cell(cells, &CellRef::pruned(live.code_root))?;
        let mut slice = CellSlice::new(&cell);
        let code = slice.load_snake(
            cells,
            usize::try_from(live.code_bytes).map_err(|_| VMError::StorageArithmeticOverflow)?,
        )?;
        slice.finish()?;
        if code.len() as u64 != live.code_bytes {
            return Err(CellError::InvalidFormat.into());
        }
        Ok(code)
    }

    fn actor_cells(&self, actor: &ActorID) -> Result<Arc<BagOfCells>, VMError> {
        // Refreshed by the VM at instruction boundaries, including after a
        // nested call returns. Reuse the committed code/state body set: the
        // registry's actor/lease metadata is not an execution witness source.
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        Ok(Arc::clone(&live.cells))
    }

    fn actor_code_bytes(&self, actor: &ActorID) -> Result<u64, VMError> {
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        if self.checked_out.contains(&actor.to_hash()) {
            return Err(VMError::ActorEmpty);
        }
        Ok(live.code_bytes)
    }

    fn actor_state_bytes(&self, actor: &ActorID) -> Result<u64, VMError> {
        let live = self.require_live(actor.to_hash())?.live.as_ref().unwrap();
        if self.checked_out.contains(&actor.to_hash()) {
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
        live.code_bytes = code.len() as u64;
        live.code = Some(code);
        live.retain_changes()?;
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
        let state_bytes = Self::state_bytes(&state)?;
        let mut live = LiveActor {
            code_root: code_root(&code),
            state_root: state_root(&state),
            code_bytes: code.len() as u64,
            code: Some(code),
            state: Some(state),
            state_bytes,
            cells: Arc::new(BagOfCells::new()),
            resident_bytes: 0,
        };
        live.retain_changes()?;
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
    use flamevm::{ActorRegistry, ClearToken, Dict, FLAME_FLAVOR, Scalar, Value, empty_state};

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
        let graph = store.stored_actor(&actor()).unwrap();
        let root = graph.cells.get(&graph.root).unwrap();
        assert_eq!(root.payload().len(), 17);
        assert_eq!(root.payload()[0], 1);
        assert_eq!(root.refs().len(), 3);
        assert_eq!(root.refs()[0].id(), code_root(&[0x1d]));
        assert_eq!(
            root.refs()[1].id(),
            state_root(&Value::Scalar(Scalar::from(42u64)))
        );
        let before = store.actor_root();
        store.purchase_storage(&actor(), 1_024, 1).unwrap().unwrap();
        assert_ne!(before, store.actor_root());
    }

    #[test]
    fn execution_body_scope_is_shared_and_rollback_restores_it() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store
            .deploy(actor(), vec![0], Value::Scalar(Scalar::from(42u64)))
            .unwrap();
        let before = store.actor_cells(&actor()).unwrap();
        assert!(Arc::ptr_eq(&before, &store.actor_cells(&actor()).unwrap()));
        store.purchase_storage(&actor(), 1_024, 0).unwrap().unwrap();
        assert!(Arc::ptr_eq(&before, &store.actor_cells(&actor()).unwrap()));
        let archive = store.stored_actor(&actor()).unwrap();
        assert!(archive.cells.get(&archive.root).is_some());
        assert!(before.get(&archive.root).is_none());

        store.push_checkpoint();
        store.load_state(&actor()).unwrap();
        store
            .save_state(&actor(), Value::Scalar(Scalar::from(43u64)))
            .unwrap();
        let after = store.actor_cells(&actor()).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(
            after
                .get(&state_root(&Value::Scalar(Scalar::from(43u64))))
                .is_some()
        );
        store.pop_checkpoint_rollback();
        assert!(Arc::ptr_eq(&before, &store.actor_cells(&actor()).unwrap()));
    }

    #[test]
    fn warm_and_cold_storage_reads_have_identical_public_values() {
        let token = flamevm::Token::cleartext(Scalar::from(7u64), FLAME_FLAVOR).unwrap();
        assert!(token.qty().assignment().is_some());
        let mut warm = ActorStore::new(StorageParams::default()).unwrap();
        warm.deploy(actor(), vec![0], Value::Token(token)).unwrap();
        let mut cold = warm.clone();
        let live = cold
            .actors
            .get_mut(&actor().to_hash())
            .unwrap()
            .live
            .as_mut()
            .unwrap();
        live.code = None;
        live.state = None;
        assert_eq!(warm.actor_root(), cold.actor_root());
        let warm_state = warm.load_state(&actor()).unwrap();
        let cold_state = cold.load_state(&actor()).unwrap();
        assert_eq!(state_root(&warm_state), state_root(&cold_state));
        for state in [warm_state, cold_state] {
            let Value::Token(token) = state else {
                panic!("expected token");
            };
            assert_eq!(token.qty().assignment(), None);
            assert_eq!(token.flv().assignment(), None);
        }
        assert!(matches!(
            warm.load_state(&actor()),
            Err(VMError::ActorEmpty)
        ));
        assert!(matches!(
            cold.load_state(&actor()),
            Err(VMError::ActorEmpty)
        ));
    }

    #[test]
    fn actor_info_reads_public_cells_without_checkout_or_cached_witnesses() {
        let token = flamevm::Token::cleartext(Scalar::from(7u64), FLAME_FLAVOR).unwrap();
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], Value::Token(token)).unwrap();
        let root_before = store.actor_root();
        for _ in 0..2 {
            let info = store.actor_info(&actor(), 0).unwrap();
            assert_eq!(info.code, Some(vec![0]));
            let mut envelope = info.state.unwrap();
            let root = envelope.cells().get(&envelope.root()).unwrap();
            let Value::Token(token) = Value::from_cell(&root, &mut envelope).unwrap() else {
                panic!("expected a public token");
            };
            assert_eq!(token.qty().assignment(), None);
            assert_eq!(token.flv().assignment(), None);
        }
        assert_eq!(store.actor_root(), root_before);
        assert!(store.checked_out.is_empty());

        // Cached code/state must not substitute for absent committed bodies.
        let live = store
            .actors
            .get_mut(&actor().to_hash())
            .unwrap()
            .live
            .as_mut()
            .unwrap();
        assert!(live.code.is_some() && live.state.is_some());
        live.cells = Arc::new(BagOfCells::new());
        let info = store.actor_info(&actor(), 0).unwrap();
        assert!(info.code.is_none() && info.state.is_none());
    }

    #[test]
    fn actor_info_preserves_pruned_state_references() {
        let mut state = Dict::new();
        state.insert(Scalar::ZERO, Value::Scalar(Scalar::ONE));
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], Value::Dict(state)).unwrap();
        let live = store
            .actors
            .get_mut(&actor().to_hash())
            .unwrap()
            .live
            .as_mut()
            .unwrap();
        let mut resident = BagOfCells::new();
        resident
            .insert(live.cells.get(&live.code_root).unwrap())
            .unwrap();
        resident
            .insert(live.cells.get(&live.state_root).unwrap())
            .unwrap();
        live.cells = Arc::new(resident);
        let info = store.actor_info(&actor(), 0).unwrap();
        assert_eq!(info.code, Some(vec![0]));
        let mut envelope = info.state.unwrap();
        assert_eq!(envelope.root(), info.state_root);
        assert_eq!(envelope.cells().len(), 1);
        let root = envelope.cells().get(&envelope.root()).unwrap();
        assert!(matches!(
            Value::from_cell(&root, &mut envelope),
            Err(CellError::MissingCell(_))
        ));
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
        assert!(matches!(
            store.load_state(&actor()),
            Err(VMError::ActorEmpty)
        ));
    }

    #[test]
    fn quote_rounding_and_pool_boundaries_are_exact() {
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        store.deploy(actor(), vec![0], empty_state()).unwrap();
        let quote = store.quote_storage(&actor(), 1_024, 10).unwrap().unwrap();
        assert_eq!(quote.fee_sparks, Scalar::from(1_000_007_630u64));
        assert_eq!(quote.expiry_height, 52_510);
        let params = StorageParams::default();
        let largest =
            (params.initial_pool_units - params.minimum_remaining_units) * params.unit_bytes;
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
                .quote_storage(&actor(), params.initial_pool_units * params.unit_bytes, 0,)
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
            store
                .quote_storage(&actor(), 10 * params.unit_bytes, 0)
                .unwrap(),
            None
        );
        store
            .purchase_storage(&actor(), 9 * params.unit_bytes, 0)
            .unwrap()
            .unwrap();
        assert_eq!(store.available_units(), 1);
        assert_eq!(
            store.quote_storage(&actor(), params.unit_bytes, 0).unwrap(),
            None
        );
        store.assert_supply(0).unwrap();
    }

    #[test]
    fn leases_coalesce_expire_and_freeze_at_exact_heights() {
        let params = small_params();
        let state = Value::Scalar(Scalar::from(7u64));
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
        store.freeze_expired_actors().unwrap();
        assert!(store.exists(&actor()));
        assert_eq!(
            store.actors[&actor().to_hash()]
                .live
                .as_ref()
                .unwrap()
                .state_root,
            state_hash
        );
        assert_eq!(store.actor_usage(&actor()).unwrap(), 0);
        assert!(store.load_state(&actor()).is_err());
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
    fn nested_expiry_and_freezing_commit_is_undone_by_outer_rollback() {
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
        store.freeze_expired_actors().unwrap();
        store.pop_checkpoint_commit();
        assert!(store.exists(&actor()));
        assert!(store.load_state(&actor()).is_err());

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
