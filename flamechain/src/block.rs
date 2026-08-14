//! Deterministic Flame block transition and reversible active chain.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use flamevm::{
    ActorID, ActorRegistry, BlockContext, Cell, CellID, Commitment, ExternalTx, Limits, Message,
    TxEntry, TxHeader, TxID, TxLog, VMError, Value,
};
use merkle::{Hash, MerkleItem, MerkleTree};
use merlin::Transcript;

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
    pub max_external_memory: u64,
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
            max_external_memory: 512 * 1024 * 1024,
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
        t.append_message(b"tx.mem", &self.limits.mem.to_le_bytes());
        t.append_message(
            b"utreexo.proof_count",
            &(self.proofs.len() as u64).to_le_bytes(),
        );
        for proof in &self.proofs {
            match proof {
                Proof::Transient => t.append_message(b"utreexo.proof.kind", &[0]),
                Proof::Committed(path) => {
                    t.append_message(b"utreexo.proof.kind", &[1]);
                    t.append_message(b"utreexo.proof.position", &path.position.to_le_bytes());
                    t.append_message(
                        b"utreexo.proof.depth",
                        &(path.neighbors.len() as u64).to_le_bytes(),
                    );
                    for neighbor in &path.neighbors {
                        t.append_message(b"utreexo.proof.neighbor", &neighbor.0);
                    }
                }
            }
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
            .checked_add(16)?
            .checked_add(8)?;
        self.proofs.iter().try_fold(fixed, |sum, proof| {
            let proof_size = match proof {
                Proof::Transient => 1,
                Proof::Committed(path) => 1usize
                    .checked_add(8)?
                    .checked_add(8)?
                    .checked_add(path.neighbors.len().checked_mul(32)?)?,
            };
            sum.checked_add(proof_size)
        })
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
            self.apply_log(
                &mut work,
                &hasher,
                &log,
                &block_tx.proofs,
                &mut sends,
                &mut seen_outputs,
            )?;
            records.push(ExecutionRecord {
                kind: ExecutionKind::External,
                txid: log.txid(),
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
            let (kind, log) = match message.execute_tx(&mut self.actors, &context) {
                Ok(result) => (ExecutionKind::Internal, result.into_log()),
                Err(_) => (
                    ExecutionKind::InternalFailed,
                    Self::bounce_log(failed_message),
                ),
            };
            self.apply_log(&mut work, &hasher, &log, &[], &mut sends, &mut seen_outputs)?;
            records.push(ExecutionRecord {
                kind,
                txid: log.txid(),
            });
        }

        for destroyed in self.actors.destroy_expired_actors()? {
            let log = Self::destruction_log(block.header.height, destroyed)?;
            records.push(ExecutionRecord {
                kind: ExecutionKind::ActorDestroy,
                txid: log.txid(),
            });
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

    fn bounce_log(message: Message) -> TxLog {
        let receive = *message.id().as_bytes();
        let (anchor, _) = message.anchor.split();
        TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Receive(receive),
            TxEntry::Output(Cell::new(message.refund_predicate, anchor, message.payload)),
        ])
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
            Value::ClearToken(token) if token.qty().is_negative() => {
                return Err(VMError::NonPortableInState.into());
            }
            Value::ClearToken(token) if !token.qty().is_zero() => entries.push(TxEntry::Retire(
                Commitment::unblinded(token.qty()).to_point(),
                Commitment::unblinded(token.flv()).to_point(),
            )),
            Value::Token(token) => {
                entries.push(TxEntry::Retire(token.qty.to_point(), token.flv.to_point()))
            }
            Value::Int253(_) | Value::String(_) | Value::Point(_) | Value::ClearToken(_) => {}
            _ => return Err(VMError::NonPortableInState.into()),
        }
        Ok(())
    }

    fn apply_log(
        &self,
        work: &mut utreexo::WorkForest,
        hasher: &merkle::Hasher<CellLeaf>,
        log: &TxLog,
        proofs: &[Proof],
        sends: &mut VecDeque<Message>,
        seen_outputs: &mut BTreeSet<CellID>,
    ) -> Result<(), ChainError> {
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
        work.batch(|work| {
            for entry in log.iter() {
                match entry {
                    TxEntry::Input(id) => {
                        work.delete(&CellLeaf(*id), proofs.next().unwrap(), hasher)?;
                    }
                    TxEntry::Output(cell) => {
                        let id = cell.id();
                        if seen_outputs.contains(&id) || !new_outputs.insert(id) {
                            return Err(ChainError::DuplicateCell);
                        }
                        work.insert(&CellLeaf(id), hasher);
                    }
                    TxEntry::Send(message) => new_sends.push(message.clone()),
                    _ => {}
                }
            }
            Ok::<_, ChainError>(())
        })?;
        seen_outputs.extend(new_outputs);
        sends.extend(new_sends);
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
        let mut memory = 0u64;
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
            memory = memory
                .checked_add(tx.limits.mem)
                .ok_or(ChainError::LimitExceeded)?;
        }
        if witness_bytes > self.params.limits.max_witness_bytes
            || gas > self.params.limits.max_external_gas
            || memory > self.params.limits.max_external_memory
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
    use flamevm::{ClearToken, FLAME_FLAVOR, Int253};

    #[test]
    fn expiry_destruction_retires_tokens_and_binds_height() {
        let destroyed = || DestroyedActor {
            actor: ActorID::Hash([9; 32]),
            state: Value::ClearToken(ClearToken::new(Int253::from(7u64), FLAME_FLAVOR)),
        };
        let first = Blockchain::destruction_log(10, destroyed()).unwrap();
        let second = Blockchain::destruction_log(11, destroyed()).unwrap();
        assert!(
            first
                .iter()
                .any(|entry| matches!(entry, TxEntry::Retire(_, _)))
        );
        assert!(
            first
                .iter()
                .any(|entry| matches!(entry, TxEntry::ActorDestroy { .. }))
        );
        assert_ne!(first.txid(), second.txid());
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
