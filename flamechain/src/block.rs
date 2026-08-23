//! Deterministic Flame block transition and reversible active chain.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use flamevm::{
    ActorID, ActorRegistry, BlockContext, Cell, CellID, Commitment, ExternalTx, Limits, Message,
    TxEntry, TxHeader, TxID, TxLog, VMError, Value,
};
use merkle::{Hash, MerkleItem, MerkleTree};
use merlin::Transcript;
use readerwriter::{Encodable, ExactSizeEncodable};

use crate::BlockHash;
use crate::storage::{ActorStore, DestroyedActor, RegistryUndo, StorageError, StorageParams};
use crate::utreexo::{self, Catchup, Forest, Proof, UtreexoError};

/// Consensus resource bounds. Networks can select smaller values through
/// [`ChainParams`] without changing the transition algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockLimits {
    pub max_transactions: usize,
    pub max_witness_bytes: usize,
    pub max_external_gas: u64,
    pub max_internal_gas: u64,
    pub max_messages: usize,
    pub max_proofs_per_transaction: usize,
    pub max_proof_depth: usize,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            max_transactions: 10_000,
            max_witness_bytes: 16 * 1024 * 1024,
            max_external_gas: 100_000_000,
            max_internal_gas: 25_000_000,
            max_messages: 100_000,
            max_proofs_per_transaction: 100_000,
            max_proof_depth: 63,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainParams {
    pub version: u32,
    pub storage: StorageParams,
    pub limits: BlockLimits,
}

impl Default for ChainParams {
    fn default() -> Self {
        Self {
            version: 1,
            storage: StorageParams::default(),
            limits: BlockLimits::default(),
        }
    }
}

/// An external transaction plus its declared resources and exactly one
/// Utreexo proof for every `Input` effect produced by verification.
pub struct BlockTx {
    pub tx: ExternalTx,
    pub limits: Limits,
    pub proofs: Vec<Proof>,
}

impl BlockTx {
    pub fn witness_hash(&self) -> Hash {
        let mut t = Transcript::new(b"flamechain.block.tx.witness");
        let header = self.tx.header();
        t.append_message(b"tx.version", &header.version.to_le_bytes());
        t.append_message(b"tx.locktime", &header.locktime.to_le_bytes());
        t.append_message(b"tx.script", self.tx.script());
        t.append_message(b"tx.signature", &self.tx.signature_bytes());
        t.append_message(b"tx.proof", &self.tx.proof_bytes());
        t.append_message(b"tx.gas", &self.limits.gas.to_le_bytes());
        t.append_message(
            b"utreexo.proof_count",
            &(self.proofs.len() as u64).to_le_bytes(),
        );
        for proof in &self.proofs {
            t.append_message(b"utreexo.proof", &proof.encode_to_vec());
        }
        let mut hash = [0; 32];
        t.challenge_bytes(b"witness_hash", &mut hash);
        Hash(hash)
    }

    /// Explicit in-memory witness weight. Canonical block transport is still
    /// intentionally TBD; this accounts for every field committed above.
    pub fn witness_size(&self) -> Option<usize> {
        let fixed = 8usize
            .checked_add(self.tx.script().len())?
            .checked_add(self.tx.signature_bytes().len())?
            .checked_add(self.tx.proof_bytes().len())?
            .checked_add(8)?
            .checked_add(8)?;
        self.proofs
            .iter()
            .try_fold(fixed, |sum, proof| sum.checked_add(proof.encoded_size()))
    }
}

struct WitnessHash(Hash);

impl MerkleItem for WitnessHash {
    fn commit(&self, t: &mut Transcript) {
        t.append_message(b"witness", &self.0.0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateCommitment {
    pub cells: Hash,
    pub actors: Hash,
    pub available_storage_units: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    /// Opaque identity of the authenticated Bitcoin/core block supplied by
    /// the caller. Flamechain commits it but does not validate Bitcoin.
    pub core_block_hash: [u8; 32],
    pub parent: BlockHash,
    pub witness_root: Hash,
    pub effects_root: Hash,
    pub state: StateCommitment,
}

impl BlockHeader {
    pub fn id(&self) -> BlockHash {
        let mut t = Transcript::new(b"flamechain.block.header");
        t.append_message(b"version", &self.version.to_le_bytes());
        t.append_message(b"height", &self.height.to_le_bytes());
        t.append_message(b"core_block_hash", &self.core_block_hash);
        t.append_message(b"parent", self.parent.as_bytes());
        t.append_message(b"witness_root", &self.witness_root.0);
        t.append_message(b"effects_root", &self.effects_root.0);
        t.append_message(b"cell_root", &self.state.cells.0);
        t.append_message(b"actor_root", &self.state.actors.0);
        t.append_message(
            b"available_storage_units",
            &self.state.available_storage_units.to_le_bytes(),
        );
        let mut id = [0; 32];
        t.challenge_bytes(b"id", &mut id);
        BlockHash::new(id)
    }
}

pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<BlockTx>,
}

impl Block {
    pub fn witness_root(&self) -> Hash {
        MerkleTree::root(
            b"flamechain.block.witnesses",
            self.transactions
                .iter()
                .map(|tx| WitnessHash(tx.witness_hash())),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionKind {
    External,
    Internal,
    InternalFailed,
    ActorDestroy,
}

impl ExecutionKind {
    fn tag(self) -> u8 {
        match self {
            Self::External => 0,
            Self::Internal => 1,
            Self::InternalFailed => 2,
            Self::ActorDestroy => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub kind: ExecutionKind,
    pub txid: TxID,
}

struct EffectID(ExecutionRecord);

impl MerkleItem for EffectID {
    fn commit(&self, t: &mut Transcript) {
        t.append_message(b"effect.kind", &[self.0.kind.tag()]);
        t.append_message(b"effect.txid", &(self.0.txid).0.0);
    }
}

pub struct AppliedBlock {
    pub id: BlockHash,
    pub catchup: Catchup,
    pub records: Vec<ExecutionRecord>,
}

pub struct ReorgOutcome {
    pub detached: Vec<BlockHash>,
    pub attached: Vec<AppliedBlock>,
}

#[derive(Clone)]
struct BlockUndo {
    header: BlockHeader,
    cells: Forest,
    actors: RegistryUndo,
}

struct Transition {
    cells: Forest,
    catchup: Catchup,
    records: Vec<ExecutionRecord>,
    state: StateCommitment,
    effects_root: Hash,
}

/// Active Flame state. Bitcoin header tracking and fork choice live outside;
/// callers provide an authenticated core-block identity and selected branch.
pub struct Blockchain {
    params: ChainParams,
    header: BlockHeader,
    cells: Forest,
    actors: ActorStore,
    active: Vec<BlockHash>,
    undo: BTreeMap<BlockHash, BlockUndo>,
}

impl Blockchain {
    pub fn new(params: ChainParams) -> Result<Self, ChainError> {
        if params.version != 1 {
            return Err(ChainError::UnsupportedVersion);
        }
        let actors = ActorStore::new(params.storage)?;
        let cells = Forest::new();
        let cell_root = cells.root(&utreexo::utreexo_hasher::<CellLeaf>());
        let header = BlockHeader {
            version: params.version,
            height: 0,
            core_block_hash: [0; 32],
            parent: BlockHash::new([0; 32]),
            witness_root: MerkleTree::empty_root(b"flamechain.block.witnesses"),
            effects_root: MerkleTree::empty_root(b"flamechain.block.effects"),
            state: StateCommitment {
                cells: cell_root,
                actors: actors.actor_root(),
                available_storage_units: actors.available_units(),
            },
        };
        let genesis = header.id();
        Ok(Self {
            params,
            header,
            cells,
            actors,
            active: vec![genesis],
            undo: BTreeMap::new(),
        })
    }

    pub fn tip(&self) -> BlockHash {
        self.header.id()
    }

    pub fn height(&self) -> u64 {
        self.header.height
    }

    pub fn version(&self) -> u32 {
        self.header.version
    }

    pub fn state_commitment(&self) -> StateCommitment {
        self.header.state
    }

    pub fn available_storage_bytes(&self) -> Result<u64, ChainError> {
        self.actors
            .available_units()
            .checked_mul(self.params.storage.unit_bytes)
            .ok_or(ChainError::LimitExceeded)
    }

    pub fn actor_usage(&self, actor: &ActorID) -> Result<u64, ChainError> {
        Ok(self.actors.actor_usage(actor)?)
    }

    pub fn actor_capacity(&self, actor: &ActorID, height: u64) -> Result<u64, ChainError> {
        if height < self.height() {
            return Err(VMError::StorageHeightInPast.into());
        }
        Ok(self.actors.actor_capacity(actor, height)?)
    }

    pub(crate) fn cell_forest(&self) -> &Forest {
        &self.cells
    }

    /// Builds an in-memory candidate by running the same transition as
    /// validation, then rolling it back. Canonical wire encoding remains a
    /// launch requirement rather than a second, premature implementation.
    pub fn build_block(
        &mut self,
        core_block_hash: [u8; 32],
        transactions: Vec<BlockTx>,
    ) -> Result<Block, ChainError> {
        let height = self
            .header
            .height
            .checked_add(1)
            .ok_or(ChainError::LimitExceeded)?;
        let mut block = Block {
            header: BlockHeader {
                version: self.params.version,
                height,
                core_block_hash,
                parent: self.tip(),
                witness_root: MerkleTree::empty_root(b"flamechain.block.witnesses"),
                effects_root: MerkleTree::empty_root(b"flamechain.block.effects"),
                state: self.header.state,
            },
            transactions,
        };
        block.header.witness_root = block.witness_root();
        self.check_header(&block)?;

        self.actors.push_outer_checkpoint();
        let transition = self.execute_body(&block);
        self.actors.rollback_outer_checkpoint();
        let transition = transition?;
        block.header.state = transition.state;
        block.header.effects_root = transition.effects_root;
        Ok(block)
    }

    pub fn connect(&mut self, block: &Block) -> Result<AppliedBlock, ChainError> {
        self.check_header(block)?;
        self.actors.push_outer_checkpoint();
        let old_header = self.header.clone();
        let old_cells = self.cells.clone();

        let transition = match self.execute_body(block) {
            Ok(transition)
                if transition.state == block.header.state
                    && transition.effects_root == block.header.effects_root =>
            {
                transition
            }
            Ok(_) => {
                self.actors.rollback_outer_checkpoint();
                return Err(ChainError::CommitmentMismatch);
            }
            Err(error) => {
                self.actors.rollback_outer_checkpoint();
                return Err(error);
            }
        };

        self.cells = transition.cells;
        self.header = block.header.clone();
        let id = self.header.id();
        let actors = self.actors.take_outer_checkpoint();
        self.undo.insert(
            id,
            BlockUndo {
                header: old_header,
                cells: old_cells,
                actors,
            },
        );
        self.active.push(id);
        Ok(AppliedBlock {
            id,
            catchup: transition.catchup,
            records: transition.records,
        })
    }

    fn execute_body(&mut self, block: &Block) -> Result<Transition, ChainError> {
        self.actors.begin_block(block.header.height)?;
        let mut work = self.cells.work_forest();
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut records = Vec::new();
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();

        for block_tx in &block.transactions {
            let log = block_tx.tx.verify(block_tx.limits)?;
            let txid = log.txid();
            self.apply_log(
                &mut work,
                &hasher,
                ExecutionKind::External,
                log,
                &block_tx.proofs,
                &mut sends,
                &mut seen_outputs,
                block.header.height,
            )?;
            records.push(ExecutionRecord {
                kind: ExecutionKind::External,
                txid,
            });
        }

        let mut internal_gas = 0u64;
        let mut message_count = 0usize;
        while let Some(message) = sends.pop_front() {
            if message_count >= self.params.limits.max_messages {
                return Err(ChainError::LimitExceeded);
            }
            message_count += 1;
            internal_gas = internal_gas
                .checked_add(message.gas)
                .ok_or(ChainError::LimitExceeded)?;
            if internal_gas > self.params.limits.max_internal_gas {
                return Err(ChainError::LimitExceeded);
            }

            let context = BlockContext {
                height: block.header.height,
            };
            let failed_message = message.clone();
            self.actors.push_checkpoint();
            let staged = match message.execute_tx(&mut self.actors, &context) {
                Ok(result) => Ok((
                    ExecutionKind::Internal,
                    result.into_log(),
                    Some((self.actors.actor_root(), self.actors.available_units())),
                )),
                Err(_) => Self::bounce_log(failed_message)
                    .map(|log| (ExecutionKind::InternalFailed, log, None)),
            };
            self.actors.pop_checkpoint_rollback();
            let (kind, log, executed_actor_state) = staged?;
            let txid = log.txid();
            self.apply_log(
                &mut work,
                &hasher,
                kind,
                log,
                &[],
                &mut sends,
                &mut seen_outputs,
                block.header.height,
            )?;
            if let Some(expected) = executed_actor_state {
                let replayed = (self.actors.actor_root(), self.actors.available_units());
                if replayed != expected {
                    return Err(ChainError::CommitmentMismatch);
                }
            }
            records.push(ExecutionRecord { kind, txid });
        }

        self.actors.push_checkpoint();
        let staged_destructions = (|| {
            let logs = self
                .actors
                .destroy_expired_actors()?
                .into_iter()
                .map(|destroyed| Self::destruction_log(block.header.height, destroyed))
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, ChainError>((
                logs,
                self.actors.actor_root(),
                self.actors.available_units(),
            ))
        })();
        self.actors.pop_checkpoint_rollback();
        let (destruction_logs, destroyed_root, destroyed_pool) = staged_destructions?;
        for log in destruction_logs {
            let txid = log.txid();
            self.apply_log(
                &mut work,
                &hasher,
                ExecutionKind::ActorDestroy,
                log,
                &[],
                &mut sends,
                &mut seen_outputs,
                block.header.height,
            )?;
            records.push(ExecutionRecord {
                kind: ExecutionKind::ActorDestroy,
                txid,
            });
        }
        if (self.actors.actor_root(), self.actors.available_units())
            != (destroyed_root, destroyed_pool)
        {
            return Err(ChainError::CommitmentMismatch);
        }

        let (cells, catchup) = work.normalize(&hasher);
        let state = StateCommitment {
            cells: cells.root(&hasher),
            actors: self.actors.actor_root(),
            available_storage_units: self.actors.available_units(),
        };
        let effects_root = MerkleTree::root(
            b"flamechain.block.effects",
            records.iter().copied().map(EffectID),
        );
        self.actors.assert_supply(block.header.height)?;
        Ok(Transition {
            cells,
            catchup,
            records,
            state,
            effects_root,
        })
    }

    fn bounce_log(message: Message) -> Result<TxLog, ChainError> {
        let receive = *message.id().as_bytes();
        let (anchor, _) = message.anchor.split();
        let refund_predicate = message.refund_predicate.clone();
        let payload = message.into_payload();
        Ok(TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Receive(receive),
            TxEntry::Output(Cell::new(
                refund_predicate,
                anchor,
                payload,
            )?),
        ]))
    }

    fn destruction_log(height: u64, destroyed: DestroyedActor) -> Result<TxLog, ChainError> {
        let mut entries = vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Data(height.to_le_bytes().to_vec()),
        ];
        Self::retire_stored_value(&destroyed.state, &mut entries)?;
        entries.push(TxEntry::ActorDestroy {
            actor: destroyed.actor,
        });
        Ok(TxLog::from(entries))
    }

    fn retire_stored_value(value: &Value, entries: &mut Vec<TxEntry>) -> Result<(), ChainError> {
        match value {
            Value::Dict(dict) => {
                for (_, value) in dict.entries() {
                    Self::retire_stored_value(value, entries)?;
                }
            }
            Value::ClearToken(token) if !token.is_portable() => {
                return Err(VMError::NonPortableInState.into());
            }
            Value::ClearToken(token) if !token.qty().is_zero() => entries.push(TxEntry::Retire(
                Commitment::unblinded(token.qty()).to_point(),
                Commitment::unblinded(token.flv()).to_point(),
            )),
            Value::Token(token) => {
                entries.push(TxEntry::Retire(
                    token.qty().to_point(),
                    token.flv().to_point(),
                ))
            }
            Value::Int253(_) | Value::String(_) | Value::Point(_) | Value::ClearToken(_) => {}
            _ => return Err(VMError::NonPortableInState.into()),
        }
        Ok(())
    }

    fn apply_log(
        &mut self,
        work: &mut utreexo::WorkForest,
        hasher: &merkle::Hasher<CellLeaf>,
        kind: ExecutionKind,
        log: TxLog,
        proofs: &[Proof],
        sends: &mut VecDeque<Message>,
        seen_outputs: &mut BTreeSet<CellID>,
        height: u64,
    ) -> Result<(), ChainError> {
        Self::validate_log_shape(kind, &log, height)?;
        let expected = log
            .iter()
            .filter(|entry| matches!(entry, TxEntry::Input(_)))
            .count();
        if expected != proofs.len() {
            return Err(ChainError::ProofCount);
        }

        let mut proofs = proofs.iter();
        let mut new_outputs = BTreeSet::new();
        let mut new_sends = Vec::new();
        let mut new_actors = Vec::new();
        self.actors.push_checkpoint();
        let result = work.batch(|work| {
            for entry in log.into_entries() {
                match entry {
                    TxEntry::Input(id) => {
                        work.delete(&CellLeaf(id), proofs.next().unwrap(), hasher)?;
                    }
                    TxEntry::Output(cell) => {
                        let id = cell.id();
                        if seen_outputs.contains(&id) || !new_outputs.insert(id) {
                            return Err(ChainError::DuplicateCell);
                        }
                        work.insert(&CellLeaf(id), hasher);
                    }
                    TxEntry::Send(message) => new_sends.push(message),
                    TxEntry::ActorDeploy { actor, code } => {
                        let actor = Self::canonical_effect_actor(&actor)?;
                        if ActorID::Constructor(code.clone()).to_hash() != actor.to_hash() {
                            return Err(ChainError::InvalidEffectLog);
                        }
                        self.actors.replay_deploy(actor.clone(), code)?;
                        new_actors.push(actor);
                    }
                    TxEntry::ActorSave { actor, state } => {
                        let actor = Self::canonical_effect_actor(&actor)?;
                        self.actors.replay_save(&actor, state, height)?;
                    }
                    TxEntry::SetCode { actor, code } => {
                        let actor = Self::canonical_effect_actor(&actor)?;
                        self.actors.replay_set_code(&actor, code, height)?;
                    }
                    TxEntry::StoragePurchase {
                        actor,
                        bytes,
                        expiry_height,
                        fee_sparks,
                    } => {
                        let actor = Self::canonical_effect_actor(&actor)?;
                        let actual = self
                            .actors
                            .purchase_storage(&actor, bytes, height)?
                            .ok_or(ChainError::InvalidEffectLog)?;
                        if actual.expiry_height != expiry_height || actual.fee_sparks != fee_sparks
                        {
                            return Err(ChainError::InvalidEffectLog);
                        }
                    }
                    TxEntry::ActorDestroy { actor } => {
                        let actor = Self::canonical_effect_actor(&actor)?;
                        self.actors.replay_destroy(&actor)?;
                    }
                    _ => {}
                }
            }
            for actor in &new_actors {
                if self.actors.exists(actor) {
                    self.actors.validate_actor_storage(actor, height)?;
                }
            }
            Ok::<_, ChainError>(())
        });
        match result {
            Ok(_) => {
                self.actors.pop_checkpoint_commit();
                seen_outputs.extend(new_outputs);
                sends.extend(new_sends);
                Ok(())
            }
            Err(error) => {
                self.actors.pop_checkpoint_rollback();
                Err(error)
            }
        }
    }

    fn canonical_effect_actor(actor: &ActorID) -> Result<ActorID, ChainError> {
        match actor {
            ActorID::Hash(id) => Ok(ActorID::Hash(*id)),
            ActorID::Constructor(_) => Err(ChainError::InvalidEffectLog),
        }
    }

    fn validate_log_shape(kind: ExecutionKind, log: &TxLog, height: u64) -> Result<(), ChainError> {
        let entries = log.entries();
        let Some(TxEntry::Header(header)) = entries.first() else {
            return Err(ChainError::InvalidEffectLog);
        };
        if entries[1..]
            .iter()
            .any(|entry| matches!(entry, TxEntry::Header(_)))
        {
            return Err(ChainError::InvalidEffectLog);
        }

        match kind {
            ExecutionKind::External => {
                if entries[1..].iter().any(|entry| {
                    matches!(
                        entry,
                        TxEntry::Receive(_)
                            | TxEntry::ActorDeploy { .. }
                            | TxEntry::IssuePub(..)
                            | TxEntry::ActorSave { .. }
                            | TxEntry::SetCode { .. }
                            | TxEntry::StoragePurchase { .. }
                            | TxEntry::ActorDestroy { .. }
                    )
                }) {
                    return Err(ChainError::InvalidEffectLog);
                }
            }
            ExecutionKind::Internal => {
                if header.version != 1
                    || header.locktime != 0
                    || !matches!(entries.get(1), Some(TxEntry::Receive(_)))
                {
                    return Err(ChainError::InvalidEffectLog);
                }
                let mut destroying = false;
                for (index, entry) in entries.iter().enumerate().skip(2) {
                    if destroying && !matches!(entry, TxEntry::ActorDestroy { .. }) {
                        return Err(ChainError::InvalidEffectLog);
                    }
                    match entry {
                        TxEntry::ActorDeploy { .. } if index == 2 => {}
                        TxEntry::ActorDeploy { .. }
                        | TxEntry::Receive(_)
                        | TxEntry::Input(_)
                        | TxEntry::IssuePriv(..)
                        | TxEntry::Fee(_) => return Err(ChainError::InvalidEffectLog),
                        TxEntry::ActorDestroy { .. } => destroying = true,
                        _ => {}
                    }
                }
            }
            ExecutionKind::InternalFailed => {
                if header.version != 1
                    || header.locktime != 0
                    || entries.len() != 3
                    || !matches!(entries[1], TxEntry::Receive(_))
                    || !matches!(entries[2], TxEntry::Output(_))
                {
                    return Err(ChainError::InvalidEffectLog);
                }
            }
            ExecutionKind::ActorDestroy => {
                if header.version != 1
                    || header.locktime != 0
                    || entries.len() < 3
                    || !matches!(
                        entries.get(1),
                        Some(TxEntry::Data(bytes)) if bytes.as_slice() == height.to_le_bytes()
                    )
                    || !matches!(entries.last(), Some(TxEntry::ActorDestroy { .. }))
                    || entries[2..entries.len() - 1]
                        .iter()
                        .any(|entry| !matches!(entry, TxEntry::Retire(..)))
                {
                    return Err(ChainError::InvalidEffectLog);
                }
            }
        }
        Ok(())
    }

    fn check_header(&self, block: &Block) -> Result<(), ChainError> {
        let expected_height = self
            .header
            .height
            .checked_add(1)
            .ok_or(ChainError::LimitExceeded)?;
        if block.header.version != self.params.version
            || block.header.height != expected_height
            || block.header.parent != self.tip()
        {
            return Err(ChainError::InvalidHeader);
        }
        if block.transactions.len() > self.params.limits.max_transactions {
            return Err(ChainError::LimitExceeded);
        }

        let mut witness_bytes = 0usize;
        let mut gas = 0u64;
        for tx in &block.transactions {
            if tx.tx.header().version != self.params.version
                || tx.proofs.len() > self.params.limits.max_proofs_per_transaction
                || tx.proofs.iter().any(|proof| {
                    proof.as_path().is_some_and(|path| {
                        path.neighbors.len() > self.params.limits.max_proof_depth
                    })
                })
            {
                return Err(ChainError::InvalidTransactionEnvelope);
            }
            witness_bytes = witness_bytes
                .checked_add(tx.witness_size().ok_or(ChainError::LimitExceeded)?)
                .ok_or(ChainError::LimitExceeded)?;
            gas = gas
                .checked_add(tx.limits.gas)
                .ok_or(ChainError::LimitExceeded)?;
        }
        if witness_bytes > self.params.limits.max_witness_bytes
            || gas > self.params.limits.max_external_gas
        {
            return Err(ChainError::LimitExceeded);
        }
        if block.witness_root() != block.header.witness_root {
            return Err(ChainError::InvalidHeader);
        }
        Ok(())
    }

    pub fn disconnect_tip(&mut self, expected: BlockHash) -> Result<(), ChainError> {
        if self.tip() != expected || self.header.height == 0 {
            return Err(ChainError::NotActiveTip);
        }
        let undo = self
            .undo
            .remove(&expected)
            .ok_or(ChainError::UndoUnavailable)?;
        self.actors.apply_undo(undo.actors);
        self.cells = undo.cells;
        self.header = undo.header;
        self.active.pop();
        Ok(())
    }

    /// Atomically detaches to `ancestor` and attaches the caller-selected
    /// branch. Fork choice and Bitcoin validation remain outside this type.
    pub fn reorganize<'a>(
        &mut self,
        ancestor: BlockHash,
        replacement: impl IntoIterator<Item = &'a Block>,
    ) -> Result<ReorgOutcome, ChainError> {
        let ancestor_pos = self
            .active
            .iter()
            .position(|id| *id == ancestor)
            .ok_or(ChainError::UnknownAncestor)?;
        let replacement: Vec<_> = replacement.into_iter().collect();
        let mut expected_parent = ancestor;
        let mut expected_height = ancestor_pos as u64 + 1;
        for block in &replacement {
            if block.header.parent != expected_parent || block.header.height != expected_height {
                return Err(ChainError::InvalidHeader);
            }
            expected_parent = block.header.id();
            expected_height = expected_height
                .checked_add(1)
                .ok_or(ChainError::LimitExceeded)?;
        }

        // ponytail: one full snapshot makes a rare failed reorg obviously
        // atomic; replace with retained forward blocks/deltas if profiling
        // shows reorg-time cloning matters.
        let snapshot = (
            self.header.clone(),
            self.cells.clone(),
            self.actors.clone(),
            self.active.clone(),
            self.undo.clone(),
        );

        let result = (|| {
            let mut detached = Vec::new();
            while self.active.len() > ancestor_pos + 1 {
                let tip = self.tip();
                self.disconnect_tip(tip)?;
                detached.push(tip);
            }
            let mut attached = Vec::new();
            for block in replacement {
                attached.push(self.connect(block)?);
            }
            Ok(ReorgOutcome { detached, attached })
        })();

        if result.is_err() {
            self.header = snapshot.0;
            self.cells = snapshot.1;
            self.actors = snapshot.2;
            self.active = snapshot.3;
            self.undo = snapshot.4;
        }
        result
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CellLeaf(pub CellID);

impl MerkleItem for CellLeaf {
    fn commit(&self, t: &mut Transcript) {
        t.append_message(b"cell.id", &self.0);
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ChainError {
    #[error("unsupported Flame protocol version")]
    UnsupportedVersion,
    #[error("invalid block header")]
    InvalidHeader,
    #[error("invalid external transaction envelope")]
    InvalidTransactionEnvelope,
    #[error("block resource limit exceeded")]
    LimitExceeded,
    #[error("missing or trailing Utreexo proof")]
    ProofCount,
    #[error("duplicate cell id in one block")]
    DuplicateCell,
    #[error("invalid transaction effect log")]
    InvalidEffectLog,
    #[error("block commitment mismatch")]
    CommitmentMismatch,
    #[error("requested block is not the active tip")]
    NotActiveTip,
    #[error("reorg ancestor is not on the retained active chain")]
    UnknownAncestor,
    #[error("undo data is unavailable")]
    UndoUnavailable,
    #[error(transparent)]
    Vm(#[from] VMError),
    #[error(transparent)]
    Utreexo(#[from] UtreexoError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use flamevm::{
        Anchor, ClearToken, Dict, FLAME_FLAVOR, Int253, Predicate, ScriptBuilder, Token,
        empty_state, state_root,
    };

    fn refund_predicate() -> Predicate {
        Predicate::opaque(Predicate::unspendable_key())
    }

    fn message(target: ActorID, payload: Vec<Value>, gas: u64, anchor: u8) -> Message {
        Message::new(
            target,
            None,
            Anchor([anchor; 32]),
            payload,
            gas,
            refund_predicate(),
        )
        .expect("test payload is portable")
    }

    fn assert_bounce_matches(log: &TxLog, original: &Message) {
        assert_eq!(log.entries().len(), 3);
        assert!(matches!(
            log.entries()[0],
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0
            })
        ));
        assert!(matches!(
            log.entries()[1],
            TxEntry::Receive(id) if id == *original.id().as_bytes()
        ));
        let TxEntry::Output(cell) = &log.entries()[2] else {
            panic!("bounce must contain exactly one output");
        };
        assert_eq!(cell.anchor, original.anchor.split().0);
        assert_eq!(
            cell.predicate.verification_key(),
            original.refund_predicate.verification_key()
        );
        assert_eq!(cell.payload().len(), original.payload().len());
        for (actual, expected) in cell.payload().iter().zip(original.payload()) {
            assert_eq!(state_root(actual), state_root(expected));
        }
    }

    fn fail_and_bounce(
        store: &mut ActorStore,
        original: Message,
        height: u64,
    ) -> (VMError, TxLog) {
        let escrow = original.clone();
        let error = match original.execute_tx(store, &BlockContext { height }) {
            Ok(_) => panic!("delivery must fail"),
            Err(error) => error,
        };
        let log = Blockchain::bounce_log(escrow.clone()).expect("admitted payload must bounce");
        assert_bounce_matches(&log, &escrow);
        (error, log)
    }

    fn deploy_actor(store: &mut ActorStore, id: u8, code: Vec<u8>) -> ActorID {
        let actor = ActorID::Hash([id; 32]);
        store
            .deploy(actor.clone(), code, empty_state())
            .expect("deploy test actor");
        store
            .purchase_storage(&actor, 1_024, 0)
            .expect("quote storage")
            .expect("storage available");
        actor
    }

    #[test]
    fn failed_delivery_matrix_returns_one_exact_bounce() {
        let payload = || {
            vec![Value::ClearToken(ClearToken::new(
                Int253::from(7u64),
                FLAME_FLAVOR,
            ))]
        };

        let mut missing = ActorStore::new(StorageParams::default()).unwrap();
        let (error, _) = fail_and_bounce(
            &mut missing,
            message(ActorID::Hash([1; 32]), payload(), 1_000_000, 1),
            0,
        );
        assert!(matches!(error, VMError::ActorNotFound));

        let mut malformed = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(&mut malformed, 2, vec![0xff]);
        let (error, _) = fail_and_bounce(
            &mut malformed,
            message(actor, payload(), 1_000_000, 2),
            0,
        );
        assert!(matches!(error, VMError::UnknownOpcode(0xff)));

        let mut failing = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(
            &mut failing,
            3,
            ScriptBuilder::new().push_int(0u64).verify().to_bytecode(),
        );
        let (error, _) = fail_and_bounce(
            &mut failing,
            message(actor, payload(), 1_000_000, 3),
            0,
        );
        assert!(matches!(error, VMError::VerifyFailed));

        let mut dirty = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(
            &mut dirty,
            4,
            ScriptBuilder::new().push_int(1u64).to_bytecode(),
        );
        let (error, _) = fail_and_bounce(
            &mut dirty,
            message(actor, payload(), 1_000_000, 4),
            0,
        );
        assert!(matches!(error, VMError::StackNotClean));

        let mut out_of_gas = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(&mut out_of_gas, 5, ScriptBuilder::new().nop().to_bytecode());
        let (error, _) = fail_and_bounce(
            &mut out_of_gas,
            message(actor, payload(), 0, 5),
            0,
        );
        assert!(matches!(error, VMError::OutOfGas));

        let mut checked_out = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(&mut checked_out, 6, ScriptBuilder::new().nop().to_bytecode());
        checked_out.load_state(&actor).unwrap();
        let (error, _) = fail_and_bounce(
            &mut checked_out,
            message(actor, payload(), 1_000_000, 6),
            0,
        );
        assert!(matches!(error, VMError::ActorEmpty));

        let mut pending = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(&mut pending, 7, ScriptBuilder::new().nop().to_bytecode());
        let expiry = StorageParams::default().lease_duration_blocks;
        pending.begin_block(expiry).unwrap();
        let (error, _) = fail_and_bounce(
            &mut pending,
            message(actor, payload(), 1_000_000, 7),
            expiry,
        );
        assert!(matches!(error, VMError::ActorPendingDestruction));
    }

    #[test]
    fn failed_constructor_rolls_back_actor_storage_and_effects() {
        let code = ScriptBuilder::new()
            .push_int(5u64)
            .push_str(b"mint".to_vec())
            .issuepub()
            .retire()
            .push_int(1_024u64)
            .addstorage()
            .drop_()
            .merge()
            .drop_()
            .drop_()
            .push_int(0u64)
            .verify()
            .to_bytecode();
        let target = ActorID::Constructor(code.clone());
        let mut store = ActorStore::new(StorageParams::default()).unwrap();
        let mut quote_store = store.clone();
        quote_store
            .deploy(target.clone(), code.clone(), empty_state())
            .unwrap();
        let fee = quote_store
            .quote_storage(&target, 1_024, 0)
            .unwrap()
            .unwrap()
            .fee_sparks;
        let original = message(
            target.clone(),
            vec![Value::ClearToken(ClearToken::new(fee, FLAME_FLAVOR))],
            10_000_000,
            8,
        );
        let pool = store.available_units();
        let actors = store.actor_root();

        let (error, bounce) = fail_and_bounce(&mut store, original, 0);

        assert!(matches!(error, VMError::VerifyFailed));
        assert!(!store.exists(&target));
        assert_eq!(store.available_units(), pool);
        assert_eq!(store.actor_root(), actors);
        store.assert_supply(0).unwrap();
        assert!(!bounce.iter().any(|entry| matches!(
            entry,
            TxEntry::IssuePub(..)
                | TxEntry::Retire(..)
                | TxEntry::ActorDeploy { .. }
                | TxEntry::StoragePurchase { .. }
                | TxEntry::ActorSave { .. }
        )));
    }

    #[test]
    fn actor_effect_replay_matches_direct_execution() {
        let mut state = Dict::new();
        state.insert(
            Int253::ZERO,
            Value::Token(
                Token::cleartext(Int253::from(11u64), FLAME_FLAVOR)
                    .expect("quantity is in range"),
            ),
        );
        let expected_state = Value::Dict(state);
        let replacement_code = ScriptBuilder::new().nop().to_bytecode();
        let constructor_code = ScriptBuilder::new()
            .push_int(1_024u64)
            .addstorage()
            .drop_()
            .merge()
            .drop_()
            .drop_()
            .push_str(replacement_code.clone())
            .setcode()
            .load()
            .drop_()
            .save()
            .to_bytecode();
        let target = ActorID::Constructor(constructor_code.clone());
        let actor = ActorID::Hash(target.to_hash());
        let baseline = ActorStore::new(StorageParams::default()).unwrap();
        let mut quote_store = baseline.clone();
        quote_store
            .deploy(target.clone(), constructor_code, empty_state())
            .unwrap();
        let fee = quote_store
            .quote_storage(&target, 1_024, 0)
            .unwrap()
            .unwrap()
            .fee_sparks;
        let delivery = message(
            target,
            vec![
                expected_state.clone(),
                Value::ClearToken(ClearToken::new(fee, FLAME_FLAVOR)),
            ],
            10_000_000,
            70,
        );

        let mut executed = baseline.clone();
        let log = delivery
            .execute_tx(&mut executed, &BlockContext { height: 0 })
            .expect("constructor execution succeeds")
            .into_log();
        assert!(matches!(
            log.entries().get(2),
            Some(TxEntry::ActorDeploy { actor: deployed, code })
                if deployed == &actor && !code.is_empty()
        ));
        assert!(
            log.iter()
                .any(|entry| matches!(entry, TxEntry::StoragePurchase { .. }))
        );
        assert!(
            log.iter()
                .any(|entry| matches!(entry, TxEntry::SetCode { .. }))
        );
        assert!(
            log.iter()
                .any(|entry| matches!(entry, TxEntry::ActorSave { .. }))
        );
        let expected_commitment = (executed.actor_root(), executed.available_units());

        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        chain.actors = baseline;
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut work = chain.cells.work_forest();
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();
        chain
            .apply_log(
                &mut work,
                &hasher,
                ExecutionKind::Internal,
                log,
                &[],
                &mut sends,
                &mut seen_outputs,
                0,
            )
            .expect("committed effects replay");

        assert_eq!(
            (chain.actors.actor_root(), chain.actors.available_units()),
            expected_commitment
        );
        assert!(sends.is_empty());
        assert_eq!(work.normalize(&hasher).0.count(), 0);
        chain.actors.assert_supply(0).unwrap();
        assert_eq!(chain.actors.load_code(&actor).unwrap(), replacement_code);
        let replayed_state = chain.actors.load_state(&actor).unwrap();
        assert_eq!(state_root(&replayed_state), state_root(&expected_state));
    }

    #[test]
    fn invalid_effect_shape_rolls_back_every_lane() {
        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        let actor_root = chain.actors.actor_root();
        let pool = chain.actors.available_units();
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut work = chain.cells.work_forest();
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();
        let code = ScriptBuilder::new().nop().to_bytecode();
        let actor = ActorID::Hash(ActorID::Constructor(code.clone()).to_hash());
        let invalid = TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Receive([1; 32]),
            TxEntry::Data(vec![0]),
            TxEntry::ActorDeploy { actor, code },
        ]);

        assert!(matches!(
            chain.apply_log(
                &mut work,
                &hasher,
                ExecutionKind::Internal,
                invalid,
                &[],
                &mut sends,
                &mut seen_outputs,
                0,
            ),
            Err(ChainError::InvalidEffectLog)
        ));
        assert_eq!(chain.actors.actor_root(), actor_root);
        assert_eq!(chain.actors.available_units(), pool);
        assert_eq!(work.normalize(&hasher).0.count(), 0);
        assert!(sends.is_empty());
        assert!(seen_outputs.is_empty());

        let code = ScriptBuilder::new().nop().to_bytecode();
        let actor = ActorID::Hash(ActorID::Constructor(code.clone()).to_hash());
        let underfunded = TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Receive([2; 32]),
            TxEntry::ActorDeploy { actor, code },
        ]);
        let mut work = chain.cells.work_forest();
        assert!(matches!(
            chain.apply_log(
                &mut work,
                &hasher,
                ExecutionKind::Internal,
                underfunded,
                &[],
                &mut sends,
                &mut seen_outputs,
                0,
            ),
            Err(ChainError::Vm(VMError::StorageCapacityExceeded))
        ));
        assert_eq!(chain.actors.actor_root(), actor_root);
        assert_eq!(chain.actors.available_units(), pool);
        assert_eq!(work.normalize(&hasher).0.count(), 0);
    }

    #[test]
    fn nested_call_effects_replay_in_order() {
        let mut baseline = ActorStore::new(StorageParams::default()).unwrap();
        let child_code = ScriptBuilder::new()
            .load()
            .drop_()
            .push_int(2u64)
            .save()
            .to_bytecode();
        let child = deploy_actor(&mut baseline, 81, child_code);
        let parent_code = ScriptBuilder::new()
            .load()
            .drop_()
            .push_int(1u64)
            .save()
            .push_int(0u64)
            .push_int(100_000u64)
            .push_str(child.to_hash().to_vec())
            .call()
            .drop_()
            .drop_()
            .to_bytecode();
        let parent = deploy_actor(&mut baseline, 80, parent_code);
        let delivery = message(parent.clone(), Vec::new(), 1_000_000, 80);

        let mut executed = baseline.clone();
        let log = delivery
            .execute_tx(&mut executed, &BlockContext { height: 0 })
            .expect("nested call succeeds")
            .into_log();
        let saves: Vec<_> = log
            .iter()
            .filter_map(|entry| match entry {
                TxEntry::ActorSave { actor, .. } => Some(actor.to_hash()),
                _ => None,
            })
            .collect();
        assert_eq!(saves, vec![parent.to_hash(), child.to_hash()]);
        let expected = (executed.actor_root(), executed.available_units());

        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        chain.actors = baseline;
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut work = chain.cells.work_forest();
        chain
            .apply_log(
                &mut work,
                &hasher,
                ExecutionKind::Internal,
                log,
                &[],
                &mut VecDeque::new(),
                &mut BTreeSet::new(),
                0,
            )
            .expect("nested effects replay");
        assert_eq!(
            (chain.actors.actor_root(), chain.actors.available_units()),
            expected
        );
        assert_eq!(
            state_root(&chain.actors.load_state(&parent).unwrap()),
            state_root(&Value::Int253(Int253::ONE))
        );
        assert_eq!(
            state_root(&chain.actors.load_state(&child).unwrap()),
            state_root(&Value::Int253(Int253::from(2u64)))
        );
    }

    #[test]
    fn explicit_actor_destruction_replays() {
        let mut baseline = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(
            &mut baseline,
            82,
            ScriptBuilder::new().load().drop_().to_bytecode(),
        );
        let delivery = message(actor.clone(), Vec::new(), 1_000_000, 82);

        let mut executed = baseline.clone();
        let log = delivery
            .execute_tx(&mut executed, &BlockContext { height: 0 })
            .expect("dismantling actor succeeds")
            .into_log();
        assert!(matches!(
            log.entries().last(),
            Some(TxEntry::ActorDestroy { actor: destroyed }) if destroyed == &actor
        ));
        let expected = (executed.actor_root(), executed.available_units());

        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        chain.actors = baseline;
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut work = chain.cells.work_forest();
        chain
            .apply_log(
                &mut work,
                &hasher,
                ExecutionKind::Internal,
                log,
                &[],
                &mut VecDeque::new(),
                &mut BTreeSet::new(),
                0,
            )
            .expect("destruction effects replay");

        assert_eq!(
            (chain.actors.actor_root(), chain.actors.available_units()),
            expected
        );
        assert!(!chain.actors.exists(&actor));
        chain.actors.assert_supply(0).unwrap();
    }

    #[test]
    fn output_and_send_effects_move_into_chain_lanes() {
        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut work = chain.cells.work_forest();
        let output = Cell::new(
            refund_predicate(),
            Anchor([91; 32]),
            vec![Value::Int253(Int253::ONE)],
        )
        .unwrap();
        let output_id = output.id();
        let outbound = message(ActorID::Hash([92; 32]), Vec::new(), 1_000, 92);
        let outbound_id = *outbound.id().as_bytes();
        let log = TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Receive([90; 32]),
            TxEntry::Output(output),
            TxEntry::Send(outbound),
        ]);
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();

        chain
            .apply_log(
                &mut work,
                &hasher,
                ExecutionKind::Internal,
                log,
                &[],
                &mut sends,
                &mut seen_outputs,
                0,
            )
            .unwrap();

        assert_eq!(work.normalize(&hasher).0.count(), 1);
        assert_eq!(seen_outputs, BTreeSet::from([output_id]));
        assert_eq!(sends.len(), 1);
        assert_eq!(*sends.front().unwrap().id().as_bytes(), outbound_id);
    }

    #[test]
    fn portable_bearers_are_delivered_or_recovered_without_duplication() {
        let mut dict = Dict::new();
        dict.insert(
            Int253::ZERO,
            Value::Token(
                Token::cleartext(Int253::from(11u64), FLAME_FLAVOR).expect("quantity is in range"),
            ),
        );
        let values = vec![
            Value::ClearToken(ClearToken::new(Int253::from(7u64), FLAME_FLAVOR)),
            Value::Token(
                Token::cleartext(Int253::from(9u64), FLAME_FLAVOR)
                    .expect("quantity is in range"),
            ),
            Value::Dict(dict),
        ];

        for (index, value) in values.into_iter().enumerate() {
            let mut delivered = ActorStore::new(StorageParams::default()).unwrap();
            let actor = deploy_actor(
                &mut delivered,
                20 + index as u8,
                ScriptBuilder::new().load().drop_().save().to_bytecode(),
            );
            let result = message(
                actor.clone(),
                vec![value.clone()],
                1_000_000,
                20 + index as u8,
            )
            .execute_tx(&mut delivered, &BlockContext { height: 0 })
            .expect("delivery succeeds");
            assert!(result
                .log()
                .iter()
                .any(|entry| matches!(entry, TxEntry::ActorSave { .. })));
            assert!(!result
                .log()
                .iter()
                .any(|entry| matches!(entry, TxEntry::Output(_))));
            let stored = delivered.load_state(&actor).unwrap();
            assert_eq!(state_root(&stored), state_root(&value));

            let mut recovered = ActorStore::new(StorageParams::default()).unwrap();
            let original = message(
                ActorID::Hash([40 + index as u8; 32]),
                vec![value],
                1_000_000,
                40 + index as u8,
            );
            let (_, bounce) = fail_and_bounce(&mut recovered, original, 0);
            assert_eq!(
                bounce
                    .iter()
                    .filter(|entry| matches!(entry, TxEntry::Output(_)))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn bounce_collisions_reject_atomically() {
        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut work = chain.cells.work_forest();
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();
        let original = message(ActorID::Hash([50; 32]), Vec::new(), 1_000, 50);
        let bounce = Blockchain::bounce_log(original).unwrap();
        let bounce_id = bounce
            .iter()
            .find_map(|entry| match entry {
                TxEntry::Output(cell) => Some(cell.id()),
                _ => None,
            })
            .unwrap();
        seen_outputs.insert(bounce_id);
        assert!(matches!(
            chain.apply_log(
                &mut work,
                &hasher,
                ExecutionKind::InternalFailed,
                bounce,
                &[],
                &mut sends,
                &mut seen_outputs,
                0,
            ),
            Err(ChainError::DuplicateCell)
        ));
        assert_eq!(work.normalize(&hasher).0.count(), 0);
    }

    #[test]
    fn expiry_destruction_retires_tokens_and_binds_height() {
        let actor = ActorID::Hash([9; 32]);
        let qty = Int253::from(7u64);
        let qty_point = Commitment::unblinded(qty).to_point();
        let flavor_point = Commitment::unblinded(FLAME_FLAVOR).to_point();
        let destroyed = || DestroyedActor {
            actor: actor.clone(),
            state: Value::ClearToken(ClearToken::new(qty, FLAME_FLAVOR)),
        };
        let first = Blockchain::destruction_log(10, destroyed()).unwrap();
        let second = Blockchain::destruction_log(11, destroyed()).unwrap();
        assert!(matches!(first.entries(), [
            TxEntry::Header(TxHeader { version: 1, locktime: 0 }),
            TxEntry::Data(height),
            TxEntry::Retire(q, f),
            TxEntry::ActorDestroy { actor: destroyed_actor },
        ] if height == &10u64.to_le_bytes()
            && q == &qty_point
            && f == &flavor_point
            && destroyed_actor == &actor));
        assert_ne!(first.txid(), second.txid());

        let mut params = ChainParams::default();
        params.storage.lease_duration_blocks = 1;
        let mut chain = Blockchain::new(params).unwrap();
        chain
            .actors
            .deploy(
                actor.clone(),
                ScriptBuilder::new().nop().to_bytecode(),
                Value::ClearToken(ClearToken::new(qty, FLAME_FLAVOR)),
            )
            .unwrap();
        chain
            .actors
            .purchase_storage(&actor, 1_024, 0)
            .unwrap()
            .unwrap();
        let expected_expiry_txid = Blockchain::destruction_log(1, destroyed()).unwrap().txid();
        let block = chain.build_block([10; 32], Vec::new()).unwrap();
        let applied = chain.connect(&block).unwrap();
        assert!(applied.records.iter().any(|record| {
            record.kind == ExecutionKind::ActorDestroy && record.txid == expected_expiry_txid
        }));
        assert!(!chain.actors.exists(&actor));
        chain.actors.assert_supply(1).unwrap();
    }

    #[test]
    fn empty_blocks_disconnect_and_failed_reorg_is_atomic() {
        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        let genesis = chain.tip();
        let initial_pool = chain.state_commitment().available_storage_units;

        let block1 = chain.build_block([1; 32], vec![]).unwrap();
        let applied1 = chain.connect(&block1).unwrap();
        let ancestor = applied1.id;
        assert_eq!(chain.height(), 1);
        assert_eq!(
            chain.state_commitment().available_storage_units,
            initial_pool + StorageParams::default().issued_units_per_block
        );

        let block2a = chain.build_block([2; 32], vec![]).unwrap();
        let tip2a = chain.connect(&block2a).unwrap().id;
        let mut block3a = chain.build_block([3; 32], vec![]).unwrap();
        block3a.header.state.available_storage_units += 1;

        chain.disconnect_tip(tip2a).unwrap();
        assert_eq!(chain.tip(), ancestor);
        let block2b = chain.build_block([4; 32], vec![]).unwrap();
        let tip2b = chain.connect(&block2b).unwrap().id;
        assert_ne!(tip2a, tip2b);

        let before = (chain.tip(), chain.height(), chain.state_commitment());
        let error = match chain.reorganize(ancestor, [&block2a, &block3a]) {
            Ok(_) => panic!("invalid replacement unexpectedly connected"),
            Err(error) => error,
        };
        assert!(matches!(error, ChainError::CommitmentMismatch));
        assert_eq!(
            (chain.tip(), chain.height(), chain.state_commitment()),
            before
        );

        let outcome = chain.reorganize(ancestor, [&block2a]).unwrap();
        assert_eq!(outcome.detached, vec![tip2b]);
        assert_eq!(chain.tip(), tip2a);
        chain.disconnect_tip(tip2a).unwrap();
        chain.disconnect_tip(ancestor).unwrap();
        assert_eq!(chain.tip(), genesis);
        assert_eq!(
            chain.state_commitment().available_storage_units,
            initial_pool
        );
    }
}
