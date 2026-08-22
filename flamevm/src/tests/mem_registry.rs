//! Small in-memory [`ActorRegistry`] used by FlameVM unit tests.

use std::collections::BTreeMap;

use crate::actor::{code_state_bytes, ActorID, ActorRegistry, StoragePurchase};
use crate::errors::VMError;
use crate::value::Value;

#[derive(Clone)]
pub struct TestActor {
    pub code: Vec<u8>,
    pub state: Option<Value>,
    pub capacity: u64,
    committed_usage: u64,
}

impl TestActor {
    pub fn is_checked_out(&self) -> bool {
        self.state.is_none()
    }
}

#[derive(Clone)]
struct CheckpointFrame {
    actor_undo: BTreeMap<[u8; 32], Option<TestActor>>,
}

#[derive(Clone, Default)]
pub struct MemRegistry {
    actors: BTreeMap<[u8; 32], TestActor>,
    checkpoints: Vec<CheckpointFrame>,
    storage_quote: Option<StoragePurchase>,
}

impl MemRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn record_actor(&mut self, id: [u8; 32]) {
        let Self {
            actors,
            checkpoints,
            ..
        } = self;
        if let Some(frame) = checkpoints.last_mut() {
            frame
                .actor_undo
                .entry(id)
                .or_insert_with(|| actors.get(&id).cloned());
        }
    }

    pub fn actor(&self, id: &ActorID) -> Option<&TestActor> {
        self.actors.get(&id.to_hash())
    }

    pub fn actor_mut(&mut self, id: &ActorID) -> Option<&mut TestActor> {
        self.actors.get_mut(&id.to_hash())
    }

    pub fn set_storage_quote(&mut self, quote: Option<StoragePurchase>) {
        self.storage_quote = quote;
    }

    pub fn deploy(
        &mut self,
        id: ActorID,
        code: Vec<u8>,
        state: Value,
        capacity: u64,
    ) -> Result<(), VMError> {
        self.deploy_with_capacity(id, code, state, capacity)
    }

    pub fn deploy_with_capacity(
        &mut self,
        id: ActorID,
        code: Vec<u8>,
        state: Value,
        capacity: u64,
    ) -> Result<(), VMError> {
        let key = id.to_hash();
        if self.actors.contains_key(&key) {
            return Err(VMError::ActorAlreadyExists);
        }
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        let committed_usage = code_state_bytes(&code, &state)?;
        self.record_actor(key);
        self.actors.insert(
            key,
            TestActor {
                code,
                state: Some(state),
                capacity,
                committed_usage,
            },
        );
        Ok(())
    }

    pub fn load_state(&mut self, id: &ActorID) -> Result<Value, VMError> {
        let key = id.to_hash();
        let actor = self.actors.get(&key).ok_or(VMError::ActorNotFound)?;
        if actor.state.is_none() {
            return Err(VMError::ActorEmpty);
        }
        self.record_actor(key);
        Ok(self.actors.get_mut(&key).unwrap().state.take().unwrap())
    }

    pub fn save_state(&mut self, id: &ActorID, state: Value) -> Result<(), VMError> {
        let key = id.to_hash();
        let actor = self.actors.get(&key).ok_or(VMError::ActorNotFound)?;
        if !actor.is_checked_out() {
            return Err(VMError::SaveWithoutLoad);
        }
        self.record_actor(key);
        let actor = self.actors.get_mut(&key).unwrap();
        actor.committed_usage = code_state_bytes(&actor.code, &state)?;
        actor.state = Some(state);
        Ok(())
    }

    pub fn load_code(&self, id: &ActorID) -> Result<Vec<u8>, VMError> {
        let actor = self
            .actors
            .get(&id.to_hash())
            .ok_or(VMError::ActorNotFound)?;
        if actor.is_checked_out() {
            return Err(VMError::ActorEmpty);
        }
        Ok(actor.code.clone())
    }

    pub fn set_code(&mut self, id: &ActorID, code: Vec<u8>) -> Result<(), VMError> {
        let key = id.to_hash();
        let actor = self.actors.get(&key).ok_or(VMError::ActorNotFound)?;
        let state = actor.state.as_ref().ok_or(VMError::ActorEmpty)?;
        let usage = code_state_bytes(&code, state)?;
        self.record_actor(key);
        let actor = self.actors.get_mut(&key).unwrap();
        actor.code = code;
        actor.committed_usage = usage;
        Ok(())
    }

    pub fn exists(&self, id: &ActorID) -> bool {
        self.actors.contains_key(&id.to_hash())
    }

    pub fn push_checkpoint(&mut self) {
        self.checkpoints.push(CheckpointFrame {
            actor_undo: BTreeMap::new(),
        });
    }

    pub fn pop_checkpoint_commit(&mut self) {
        let Some(frame) = self.checkpoints.pop() else {
            return;
        };
        if let Some(parent) = self.checkpoints.last_mut() {
            for (id, prior) in frame.actor_undo {
                parent.actor_undo.entry(id).or_insert(prior);
            }
        }
    }

    pub fn pop_checkpoint_rollback(&mut self) {
        let Some(frame) = self.checkpoints.pop() else {
            return;
        };
        for (id, prior) in frame.actor_undo {
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
}

impl ActorRegistry for MemRegistry {
    fn load_state(&mut self, id: &ActorID) -> Result<Value, VMError> {
        MemRegistry::load_state(self, id)
    }

    fn save_state(&mut self, id: &ActorID, state: Value) -> Result<(), VMError> {
        MemRegistry::save_state(self, id, state)
    }

    fn load_code(&self, id: &ActorID) -> Result<Vec<u8>, VMError> {
        MemRegistry::load_code(self, id)
    }

    fn actor_code_bytes(&self, id: &ActorID) -> Result<u64, VMError> {
        let actor = self.actor(id).ok_or(VMError::ActorNotFound)?;
        if actor.is_checked_out() {
            return Err(VMError::ActorEmpty);
        }
        Ok(actor.code.len() as u64)
    }

    fn actor_state_bytes(&self, id: &ActorID) -> Result<u64, VMError> {
        let actor = self.actor(id).ok_or(VMError::ActorNotFound)?;
        if actor.is_checked_out() {
            return Err(VMError::ActorEmpty);
        }
        actor
            .committed_usage
            .checked_sub(actor.code.len() as u64)
            .ok_or(VMError::StorageArithmeticOverflow)
    }

    fn set_code(&mut self, id: &ActorID, code: Vec<u8>) -> Result<(), VMError> {
        MemRegistry::set_code(self, id, code)
    }

    fn actor_usage(&self, id: &ActorID) -> Result<u64, VMError> {
        self.actor(id)
            .map(|actor| actor.committed_usage)
            .ok_or(VMError::ActorNotFound)
    }

    fn actor_capacity(&self, id: &ActorID, _height: u64) -> Result<u64, VMError> {
        self.actor(id)
            .map(|actor| actor.capacity)
            .ok_or(VMError::ActorNotFound)
    }

    fn quote_storage(
        &self,
        id: &ActorID,
        _bytes: u64,
        _height: u64,
    ) -> Result<Option<StoragePurchase>, VMError> {
        if !self.exists(id) {
            return Err(VMError::ActorNotFound);
        }
        Ok(self.storage_quote)
    }

    fn purchase_storage(
        &mut self,
        id: &ActorID,
        bytes: u64,
        height: u64,
    ) -> Result<Option<StoragePurchase>, VMError> {
        let Some(quote) = self.quote_storage(id, bytes, height)? else {
            return Ok(None);
        };
        let key = id.to_hash();
        self.record_actor(key);
        let actor = self.actors.get_mut(&key).unwrap();
        actor.capacity = actor
            .capacity
            .checked_add(bytes)
            .ok_or(VMError::StorageArithmeticOverflow)?;
        Ok(Some(quote))
    }

    fn validate_actor_storage(&self, id: &ActorID, _height: u64) -> Result<(), VMError> {
        let actor = self.actor(id).ok_or(VMError::ActorNotFound)?;
        if actor.committed_usage > actor.capacity {
            return Err(VMError::StorageCapacityExceeded);
        }
        Ok(())
    }

    fn exists(&self, id: &ActorID) -> bool {
        MemRegistry::exists(self, id)
    }

    fn push_checkpoint(&mut self) {
        MemRegistry::push_checkpoint(self)
    }

    fn pop_checkpoint_commit(&mut self) {
        MemRegistry::pop_checkpoint_commit(self)
    }

    fn pop_checkpoint_rollback(&mut self) {
        MemRegistry::pop_checkpoint_rollback(self)
    }

    fn commit_tx_destructions(&mut self) -> Vec<ActorID> {
        let destroyed: Vec<_> = self
            .actors
            .iter()
            .filter(|(_, actor)| actor.is_checked_out())
            .map(|(id, _)| ActorID::Hash(*id))
            .collect();
        for id in &destroyed {
            self.actors.remove(&id.to_hash());
        }
        destroyed
    }

    fn deploy(&mut self, id: ActorID, code: Vec<u8>, state: Value) -> Result<(), VMError> {
        MemRegistry::deploy_with_capacity(self, id, code, state, 0)
    }
}
