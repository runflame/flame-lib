//! Deterministic Flame block transition and reversible active chain.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use cells::{
    BagOfCells, Cell, CellBuilder, CellDecode, CellEncode, CellEnvelope, CellError, CellID,
    CellRef, CellResolver, CellSlice, GasMeter, Trie, resolve_cell,
};

use flamevm::{
    ActorID, ActorRegistry, BlockContext, Contract, ContractID, Dict, ExternalTx, Limits, Message,
    TxEntry, TxHeader, TxID, TxLog, VMError, Value,
};
use merkle::{Hash, MerkleItem};
use merlin::Transcript;

use crate::BlockHash;
use crate::storage::{ActorStore, RegistryUndo, StorageError, StorageParams, StoredActor};
use crate::utreexo::{self, Catchup, Forest, Proof, UtreexoError};

/// Consensus resource bounds. Networks can select smaller values through
/// [`ChainParams`] without changing the transition algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockLimits {
    pub max_transactions: usize,
    pub max_witness_bytes: usize,
    pub max_transaction_script_bytes: usize,
    pub max_script_bytes: usize,
    pub max_transaction_gas: u64,
    pub max_gas_credit: u64,
    pub max_external_gas: u64,
    pub max_internal_gas: u64,
    pub max_multiplications_per_transaction: usize,
    pub max_multiplications: usize,
    pub max_messages: usize,
    pub max_proofs_per_transaction: usize,
    pub max_proof_depth: usize,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            max_transactions: 10_000,
            max_witness_bytes: 16 * 1024 * 1024,
            max_transaction_script_bytes: 1024 * 1024,
            max_script_bytes: 4 * 1024 * 1024,
            max_transaction_gas: 35_000_000,
            max_gas_credit: 10_000_000,
            max_external_gas: 100_000_000,
            max_internal_gas: 25_000_000,
            max_multiplications_per_transaction: 1_024,
            max_multiplications: 100_000,
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

/// An external transaction plus declared gas and one Utreexo proof per Input.
/// Its two child Cells contain the ExternalTx and an ordered proof Trie.
pub struct BlockTx {
    pub tx: ExternalTx,
    pub limits: Limits,
    pub proofs: Vec<Proof>,
}

impl BlockTx {
    pub fn witness_hash(&self) -> Result<CellID, CellError> {
        Ok(self.to_cell()?.id())
    }

    /// Exact transport size, including the typed root and its complete BoC.
    pub fn witness_size(&self) -> Option<usize> {
        self.to_bytes().ok().map(|bytes| bytes.len())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CellError> {
        encode_envelope(self)
    }

    fn decode_bounded<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
        version: u32,
        limits: BlockLimits,
    ) -> Result<Self, CellError> {
        let gas = slice.load_u64()?;
        if gas > limits.max_transaction_gas {
            return Err(CellError::InvalidFormat);
        }
        let tx_cell = resolve_cell(cells, &slice.load_ref()?)?;
        let tx = ExternalTx::from_cell_bounded(
            &tx_cell,
            cells,
            version,
            limits.max_transaction_script_bytes,
            limits.max_witness_bytes,
        )?;
        if tx.to_cell()?.id() != tx_cell.id() {
            return Err(CellError::InvalidFormat);
        }
        if version != 1
            || tx.header().version != version
            || tx.script().len() > limits.max_transaction_script_bytes
        {
            return Err(CellError::InvalidFormat);
        }
        let proof_cell = resolve_cell(cells, &slice.load_ref()?)?;
        let proofs = decode_sequence(
            &proof_cell,
            cells,
            limits.max_proofs_per_transaction,
            decode_proof_cell,
        )?;
        if proofs.iter().any(|proof| {
            proof
                .as_path()
                .is_some_and(|path| path.neighbors.len() > limits.max_proof_depth)
        }) {
            return Err(CellError::InvalidFormat);
        }
        Ok(Self {
            tx,
            limits: Limits { gas },
            proofs,
        })
    }

    pub fn from_bytes_bounded(
        bytes: &[u8],
        version: u32,
        limits: BlockLimits,
    ) -> Result<Self, CellError> {
        let mut envelope = decode_envelope(bytes, limits.max_witness_bytes)?;
        let root = envelope
            .cells()
            .get(&envelope.root())
            .expect("validated envelope root");
        let mut slice = CellSlice::new(&root);
        let value = Self::decode_bounded(
            &mut slice,
            &mut TypedDecode::new(&mut envelope, limits.max_witness_bytes)?,
            version,
            limits,
        )?;
        slice.finish()?;
        if value.to_bytes()? != bytes {
            return Err(CellError::InvalidFormat);
        }
        Ok(value)
    }
}

impl CellEncode for BlockTx {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store_u64(self.limits.gas)?
            .store_ref(CellRef::resident(self.tx.to_cell()?))?
            .store_ref(CellRef::resident(sequence_cell_with(
                &self.proofs,
                proof_cell,
            )?))?;
        Ok(())
    }
}

impl CellDecode for BlockTx {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Self::decode_bounded(slice, cells, 1, BlockLimits::default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateCommitment {
    /// Specialized Utreexo accumulator commitment.
    pub contracts: Hash,
    /// Cell Trie commitment to actor content and persistent availability.
    pub actors: CellID,
    pub available_storage_units: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    /// Opaque authenticated Bitcoin/core-block identity supplied by the caller.
    pub core_block_hash: [u8; 32],
    pub parent: BlockHash,
    pub witness_root: CellID,
    pub effects_root: CellID,
    pub state: StateCommitment,
}

impl BlockHeader {
    pub fn id(&self) -> BlockHash {
        BlockHash::new(self.to_cell().expect("fixed-size block header").id())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CellError> {
        encode_envelope(self)
    }

    pub fn from_bytes_bounded(bytes: &[u8], expected_version: u32) -> Result<Self, CellError> {
        if expected_version != 1 {
            return Err(CellError::InvalidFormat);
        }
        let mut envelope = decode_envelope(bytes, 250)?;
        let root = envelope
            .cells()
            .get(&envelope.root())
            .expect("validated envelope root");
        let header = Self::from_cell(&root, &mut TypedDecode::new(&mut envelope, 250)?)?;
        if header.version != expected_version {
            return Err(CellError::InvalidFormat);
        }
        if header.to_bytes()? != bytes {
            return Err(CellError::InvalidFormat);
        }
        Ok(header)
    }
}

impl CellEncode for BlockHeader {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store_u32(self.version)?
            .store_u64(self.height)?
            .store_bytes(&self.core_block_hash)?
            .store_bytes(self.parent.as_bytes())?
            .store_bytes(&self.witness_root)?
            .store_bytes(&self.effects_root)?
            .store_bytes(&self.state.contracts.0)?
            .store_bytes(&self.state.actors)?
            .store_u64(self.state.available_storage_units)?;
        Ok(())
    }
}

impl CellDecode for BlockHeader {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self {
            version: slice.load_u32()?,
            height: slice.load_u64()?,
            core_block_hash: <[u8; 32]>::decode(slice, cells)?,
            parent: BlockHash::new(<[u8; 32]>::decode(slice, cells)?),
            witness_root: <[u8; 32]>::decode(slice, cells)?,
            effects_root: <[u8; 32]>::decode(slice, cells)?,
            state: StateCommitment {
                contracts: Hash(<[u8; 32]>::decode(slice, cells)?),
                actors: <[u8; 32]>::decode(slice, cells)?,
                available_storage_units: slice.load_u64()?,
            },
        })
    }
}

pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<BlockTx>,
}

impl Block {
    pub fn witness_root(&self) -> Result<CellID, CellError> {
        Ok(sequence_cell(&self.transactions)?.id())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CellError> {
        encode_envelope(self)
    }

    fn decode_bounded<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
        params: ChainParams,
    ) -> Result<Self, CellError> {
        let header_cell = resolve_cell(cells, &slice.load_ref()?)?;
        let header = BlockHeader::from_cell(&header_cell, cells)?;
        if params.version != 1 || header.version != params.version {
            return Err(CellError::InvalidFormat);
        }
        let txs = resolve_cell(cells, &slice.load_ref()?)?;
        let mut witness_bytes = 0usize;
        let transactions = decode_sequence(
            &txs,
            cells,
            params.limits.max_transactions,
            |cell, cells| {
                let mut slice = CellSlice::new(cell);
                let tx = BlockTx::decode_bounded(&mut slice, cells, params.version, params.limits)?;
                slice.finish()?;
                witness_bytes = witness_bytes
                    .checked_add(tx.witness_size().ok_or(CellError::LimitExceeded)?)
                    .ok_or(CellError::LimitExceeded)?;
                if witness_bytes > params.limits.max_witness_bytes {
                    return Err(CellError::LimitExceeded);
                }
                Ok(tx)
            },
        )?;
        Ok(Self {
            header,
            transactions,
        })
    }

    /// Decodes a typed Cell graph without sharing one transaction's execution
    /// witness set with another. Each ExternalTx retains its own nested BoC.
    pub fn from_bytes_bounded(bytes: &[u8], params: ChainParams) -> Result<Self, CellError> {
        let mut envelope = decode_envelope(bytes, params.limits.max_witness_bytes)?;
        let root = envelope
            .cells()
            .get(&envelope.root())
            .expect("validated envelope root");
        let mut slice = CellSlice::new(&root);
        let block = Self::decode_bounded(
            &mut slice,
            &mut TypedDecode::new(&mut envelope, params.limits.max_witness_bytes)?,
            params,
        )?;
        slice.finish()?;
        // Unused bodies belong in the explicitly committed execution BoC,
        // not in a malleable outer transport graph.
        if block.to_bytes()? != bytes {
            return Err(CellError::InvalidFormat);
        }
        Ok(block)
    }
}

impl CellEncode for Block {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store_ref(CellRef::resident(self.header.to_cell()?))?
            .store_ref(CellRef::resident(sequence_cell(&self.transactions)?))?;
        Ok(())
    }
}

impl CellDecode for Block {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Self::decode_bounded(slice, cells, ChainParams::default())
    }
}

/// Fixed-width, ordered u32 keys keep list commitments independent of insertion order.
fn sequence_cell<T: CellEncode>(items: &[T]) -> Result<Cell, CellError> {
    sequence_cell_with(items, CellEncode::to_cell)
}

fn sequence_cell_with<T>(
    items: &[T],
    mut encode: impl FnMut(&T) -> Result<Cell, CellError>,
) -> Result<Cell, CellError> {
    let count = u32::try_from(items.len()).map_err(|_| CellError::LimitExceeded)?;
    let mut trie = Trie::new(4)?;
    for (index, item) in items.iter().enumerate() {
        trie.insert(&(index as u32).to_be_bytes(), encode(item)?, &mut ())?;
    }
    let mut builder = CellBuilder::new();
    builder.store_u32(count)?;
    if let Some(root) = trie.into_root() {
        builder.store_ref(root)?;
    }
    Ok(builder.build())
}

/// Utreexo keeps its existing codec. The block's proof Trie transports those
/// canonical bytes as a snake, without changing Proof, Path, or Forest.
fn proof_cell(proof: &Proof) -> Result<Cell, CellError> {
    let mut builder = CellBuilder::new();
    builder.store_snake(&readerwriter::Encodable::encode_to_vec(proof))?;
    Ok(builder.build())
}

fn decode_proof_cell<R: CellResolver + ?Sized>(
    cell: &Cell,
    cells: &mut R,
) -> Result<Proof, CellError> {
    let mut slice = CellSlice::new(cell);
    let bytes = slice.load_snake(cells, 1 + 8 + 4 + 32 * 63)?;
    slice.finish()?;
    readerwriter::Reader::read_all(
        &mut bytes.as_slice(),
        <Proof as readerwriter::Decodable>::decode,
    )
    .map_err(|error| match error {
        readerwriter::ReadError::InsufficientBytes => CellError::InsufficientBytes,
        readerwriter::ReadError::TrailingBytes => CellError::TrailingBytes,
        _ => CellError::InvalidFormat,
    })
}

fn decode_sequence<T, R: CellResolver + ?Sized>(
    cell: &Cell,
    cells: &mut R,
    limit: usize,
    mut decode: impl FnMut(&Cell, &mut R) -> Result<T, CellError>,
) -> Result<Vec<T>, CellError> {
    let mut slice = CellSlice::new(cell);
    let count = slice.load_u32()? as usize;
    if count > limit {
        return Err(CellError::LimitExceeded);
    }
    let trie = if count == 0 {
        Trie::new(4)?
    } else {
        Trie::from_cell(slice.load_ref()?, 4)?
    };
    slice.finish()?;
    trie.entries_exact(count, cells)?
        .into_iter()
        .enumerate()
        .map(|(index, (key, reference))| {
            if key != (index as u32).to_be_bytes() {
                return Err(CellError::InvalidFormat);
            }
            let cell = resolve_cell(cells, &reference)?;
            decode(&cell, cells)
        })
        .collect()
}

fn empty_sequence_id() -> CellID {
    sequence_cell::<u8>(&[]).expect("empty sequence").id()
}

fn encode_envelope(value: &impl CellEncode) -> Result<Vec<u8>, CellError> {
    let root = Arc::new(value.to_cell()?);
    Ok(CellEnvelope::new(root.id(), BagOfCells::collect(root)?)?.encode())
}

struct DecodeBudget(u64);

impl GasMeter for DecodeBudget {
    fn charge(&mut self, amount: u64) -> Result<(), CellError> {
        self.0 = self
            .0
            .checked_sub(amount)
            .ok_or(CellError::ResourceExhausted)?;
        Ok(())
    }
}

/// A small physical DAG may expand into many typed values. Bound logical
/// traversal separately from record parsing, charging before leaf allocation.
struct TypedDecode<'a> {
    envelope: &'a mut CellEnvelope,
    budget: DecodeBudget,
}

impl<'a> TypedDecode<'a> {
    fn new(envelope: &'a mut CellEnvelope, witness_limit: usize) -> Result<Self, CellError> {
        Ok(Self {
            envelope,
            budget: DecodeBudget(
                u64::try_from(witness_limit)
                    .map_err(|_| CellError::LimitExceeded)?
                    .checked_mul(4)
                    .ok_or(CellError::LimitExceeded)?,
            ),
        })
    }
}

impl CellResolver for TypedDecode<'_> {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        self.budget.charge(1)?;
        let cell = self.envelope.resolve(reference)?;
        self.budget
            .charge(cell.encoded_size() as u64 + cell.refs().len() as u64)?;
        Ok(cell)
    }
}

fn decode_envelope(bytes: &[u8], limit: usize) -> Result<CellEnvelope, CellError> {
    // The record parser's work is linear in canonical bytes, including refs.
    let budget = u64::try_from(bytes.len())
        .map_err(|_| CellError::LimitExceeded)?
        .checked_mul(4)
        .ok_or(CellError::LimitExceeded)?;
    CellEnvelope::decode(bytes, limit, &mut DecodeBudget(budget))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionKind {
    External,
    Internal,
    InternalFailed,
}

impl ExecutionKind {
    fn tag(self) -> u8 {
        match self {
            Self::External => 0,
            Self::Internal => 1,
            Self::InternalFailed => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub kind: ExecutionKind,
    pub txid: TxID,
}

impl CellEncode for ExecutionRecord {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store_u8(self.kind.tag())?
            .store_bytes(self.txid.as_bytes())?;
        Ok(())
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
    contracts: Forest,
    actors: RegistryUndo,
}

struct Transition {
    contracts: Forest,
    catchup: Catchup,
    records: Vec<ExecutionRecord>,
    state: StateCommitment,
    effects_root: CellID,
}

/// Active Flame state. Bitcoin header tracking and fork choice live outside;
/// callers provide an authenticated core-block identity and selected branch.
pub struct Blockchain {
    params: ChainParams,
    header: BlockHeader,
    contracts: Forest,
    actors: ActorStore,
    active: Vec<BlockHash>,
    undo: BTreeMap<BlockHash, BlockUndo>,
}

impl Blockchain {
    pub fn new(params: ChainParams) -> Result<Self, ChainError> {
        Self::with_genesis_contracts(params, &[]).map(|(chain, _)| chain)
    }

    /// Genesis whose contract forest already contains `ids`. Returns the
    /// catchup so callers can derive `Proof::Committed` for them by running
    /// `Proof::Transient` through [`Catchup::update_proof`].
    ///
    /// A fresh chain has no contract to seed an anchor from, so the first
    /// spendable contracts must be placed here rather than minted by a
    /// transaction. Duplicate ids are rejected: the accumulator stores ids,
    /// and two equal leaves would leave one of them unspendable.
    pub fn with_genesis_contracts(
        params: ChainParams,
        ids: &[ContractID],
    ) -> Result<(Self, Catchup), ChainError> {
        if params.version != 1 {
            return Err(ChainError::UnsupportedVersion);
        }
        let actors = ActorStore::new(params.storage)?;
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = Forest::new().work_forest();
        let mut seen = BTreeSet::new();
        for id in ids {
            if !seen.insert(*id) {
                return Err(ChainError::DuplicateContract);
            }
            work.insert(&ContractLeaf(*id), &hasher);
        }
        let (contracts, catchup) = work.normalize(&hasher);
        let contract_root = contracts.root(&hasher);
        let header = BlockHeader {
            version: params.version,
            height: 0,
            core_block_hash: [0; 32],
            parent: BlockHash::new([0; 32]),
            witness_root: empty_sequence_id(),
            effects_root: empty_sequence_id(),
            state: StateCommitment {
                contracts: contract_root,
                actors: actors.actor_root(),
                available_storage_units: actors.available_units(),
            },
        };
        let genesis = header.id();
        Ok((
            Self {
                params,
                header,
                contracts,
                actors,
                active: vec![genesis],
                undo: BTreeMap::new(),
            },
            catchup,
        ))
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

    pub fn limits(&self) -> BlockLimits {
        self.params.limits
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

    /// Snapshot content and exact body availability for archives/witness creation.
    pub fn actor_storage(&self, actor: &ActorID) -> Result<StoredActor, ChainError> {
        Ok(self.actors.stored_actor(actor)?)
    }

    pub fn actor_capacity(&self, actor: &ActorID, height: u64) -> Result<u64, ChainError> {
        if height < self.height() {
            return Err(VMError::StorageHeightInPast.into());
        }
        Ok(self.actors.actor_capacity(actor, height)?)
    }

    /// The committed contract accumulator at the tip. Callers verify
    /// membership proofs against it before building a spend.
    pub fn contract_forest(&self) -> &Forest {
        &self.contracts
    }

    /// Builds a candidate by running the same transition as validation, then
    /// rolling it back.
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
                witness_root: empty_sequence_id(),
                effects_root: empty_sequence_id(),
                state: self.header.state,
            },
            transactions,
        };
        block.header.witness_root = block.witness_root()?;
        self.check_header(&block)?;

        self.actors.push_outer_checkpoint();
        let transition = self.execute_body(&block);
        self.actors.rollback_outer_checkpoint();
        let transition = transition?;
        block.header.state = transition.state;
        block.header.effects_root = transition.effects_root;
        Block::from_bytes_bounded(&block.to_bytes()?, self.params)?;
        Ok(block)
    }

    pub fn connect(&mut self, block: &Block) -> Result<AppliedBlock, ChainError> {
        self.check_header(block)?;
        self.actors.push_outer_checkpoint();
        let old_header = self.header.clone();
        let old_contracts = self.contracts.clone();

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

        self.contracts = transition.contracts;
        self.header = block.header.clone();
        let id = self.header.id();
        let actors = self.actors.take_outer_checkpoint();
        self.undo.insert(
            id,
            BlockUndo {
                header: old_header,
                contracts: old_contracts,
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
        let mut work = self.contracts.work_forest();
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut records = Vec::new();
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();
        let mut external_gas = 0u64;
        let mut internal_gas = 0u64;
        let mut multiplications = 0usize;
        let mut message_count = 0usize;

        for block_tx in &block.transactions {
            let (log, metrics) = block_tx.tx.verify_with_metrics(block_tx.limits)?;
            let direct_send_gas = log.direct_send_gas().ok_or(ChainError::LimitExceeded)?;
            let gas_credit = metrics
                .gas_used
                .checked_sub(direct_send_gas)
                .ok_or(ChainError::LimitExceeded)?;
            if gas_credit > self.params.limits.max_gas_credit
                || metrics.multiplications > self.params.limits.max_multiplications_per_transaction
            {
                return Err(ChainError::LimitExceeded);
            }
            external_gas = external_gas
                .checked_add(gas_credit)
                .ok_or(ChainError::LimitExceeded)?;
            internal_gas = internal_gas
                .checked_add(direct_send_gas)
                .ok_or(ChainError::LimitExceeded)?;
            multiplications = multiplications
                .checked_add(metrics.multiplications)
                .ok_or(ChainError::LimitExceeded)?;
            if external_gas > self.params.limits.max_external_gas
                || internal_gas > self.params.limits.max_internal_gas
                || multiplications > self.params.limits.max_multiplications
            {
                return Err(ChainError::LimitExceeded);
            }
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
            // Finish this external transaction's complete FIFO message closure
            // before advancing; every descendant sees only this execution BoC.
            while let Some(message) = sends.pop_front() {
                if message_count >= self.params.limits.max_messages {
                    return Err(ChainError::LimitExceeded);
                }
                message_count += 1;
                let context = BlockContext {
                    height: block.header.height,
                };
                let failed_message = message.clone();
                self.actors.push_checkpoint();
                let staged = match message.execute_tx_with_cells(
                    &mut self.actors,
                    &context,
                    Arc::clone(&block_tx.tx.witnesses),
                ) {
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
        }

        self.actors.freeze_expired_actors()?;

        let (contracts, catchup) = work.normalize(&hasher);
        let state = StateCommitment {
            contracts: contracts.root(&hasher),
            actors: self.actors.actor_root(),
            available_storage_units: self.actors.available_units(),
        };
        let effects_root = sequence_cell(&records)?.id();
        self.actors.assert_supply(block.header.height)?;
        Ok(Transition {
            contracts,
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
            TxEntry::Output(Contract::new(
                refund_predicate,
                anchor,
                Value::Dict(Dict::from_values(payload)),
            )?),
        ]))
    }

    // Keep the separately journaled state lanes explicit at this replay boundary.
    #[allow(clippy::too_many_arguments)]
    fn apply_log(
        &mut self,
        work: &mut utreexo::WorkForest,
        hasher: &merkle::Hasher<ContractLeaf>,
        kind: ExecutionKind,
        log: TxLog,
        proofs: &[Proof],
        sends: &mut VecDeque<Message>,
        seen_outputs: &mut BTreeSet<ContractID>,
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
                        work.delete(&ContractLeaf(id), proofs.next().unwrap(), hasher)?;
                    }
                    TxEntry::Output(contract) => {
                        let id = contract.id();
                        if seen_outputs.contains(&id) || !new_outputs.insert(id) {
                            return Err(ChainError::DuplicateContract);
                        }
                        work.insert(&ContractLeaf(id), hasher);
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

    fn validate_log_shape(
        kind: ExecutionKind,
        log: &TxLog,
        _height: u64,
    ) -> Result<(), ChainError> {
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
        let mut script_bytes = 0usize;
        for tx in &block.transactions {
            if tx.tx.header().version != self.params.version
                || tx.tx.script().len() > self.params.limits.max_transaction_script_bytes
                || tx.limits.gas > self.params.limits.max_transaction_gas
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
            script_bytes = script_bytes
                .checked_add(tx.tx.script().len())
                .ok_or(ChainError::LimitExceeded)?;
        }
        if witness_bytes > self.params.limits.max_witness_bytes
            || script_bytes > self.params.limits.max_script_bytes
        {
            return Err(ChainError::LimitExceeded);
        }
        // In-memory builders must obey the same logical expansion bound as
        // network admission, not bypass it by skipping Cell decoding.
        Block::from_bytes_bounded(&block.to_bytes()?, self.params)?;
        if block.witness_root()? != block.header.witness_root {
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
        self.contracts = undo.contracts;
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
            self.contracts.clone(),
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
            self.contracts = snapshot.1;
            self.actors = snapshot.2;
            self.active = snapshot.3;
            self.undo = snapshot.4;
        }
        result
    }
}

/// Utreexo leaf for a contract id. Public so callers outside the crate can
/// verify and refresh their own membership proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContractLeaf(pub ContractID);

impl MerkleItem for ContractLeaf {
    fn commit(&self, t: &mut Transcript) {
        t.append_message(b"contract.id", &self.0);
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
    #[error("duplicate contract id in one block")]
    DuplicateContract,
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
    #[error(transparent)]
    Cells(#[from] CellError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use flamevm::{
        Anchor, ClearToken, Dict, FLAME_FLAVOR, Predicate, Scalar, ScriptBuilder, Token,
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

    fn external_tx(program: ScriptBuilder, locktime: u32) -> BlockTx {
        let limits = Limits { gas: 100_000 };
        BlockTx {
            tx: program
                .build_tx(
                    TxHeader {
                        version: 1,
                        locktime,
                    },
                    limits,
                )
                .expect("build external transaction")
                .without_signature()
                .expect("program has no signtx authorization"),
            limits,
            proofs: Vec::new(),
        }
    }

    fn external_txs(count: u32, program: impl Fn() -> ScriptBuilder) -> Vec<BlockTx> {
        (0..count)
            .map(|locktime| external_tx(program(), locktime))
            .collect()
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
        let TxEntry::Output(contract) = &log.entries()[2] else {
            panic!("bounce must contain exactly one output");
        };
        assert_eq!(contract.anchor, original.anchor.split().0);
        assert_eq!(
            contract.predicate.verification_key(),
            original.refund_predicate.verification_key()
        );
        assert_eq!(
            state_root(contract.payload()),
            state_root(&Value::Dict(Dict::from_values(original.payload().to_vec())))
        );
    }

    #[test]
    fn aggregate_limits_reject_many_individually_valid_transactions() {
        let single = external_tx(ScriptBuilder::new().nop(), 0);
        let (_, metrics) = single
            .tx
            .verify_with_metrics(single.limits)
            .expect("single transaction is valid");
        let witness_bytes = single.witness_size().unwrap();

        let mut gas_params = ChainParams::default();
        gas_params.limits.max_gas_credit = metrics.gas_used;
        gas_params.limits.max_external_gas = metrics.gas_used * 2;
        let mut chain = Blockchain::new(gas_params).unwrap();
        assert!(matches!(
            chain.build_block([1; 32], external_txs(3, || ScriptBuilder::new().nop())),
            Err(ChainError::LimitExceeded)
        ));

        let mut script_params = ChainParams::default();
        script_params.limits.max_transaction_script_bytes = 1;
        script_params.limits.max_script_bytes = 2;
        let mut chain = Blockchain::new(script_params).unwrap();
        assert!(matches!(
            chain.build_block([2; 32], external_txs(3, || ScriptBuilder::new().nop())),
            Err(ChainError::LimitExceeded)
        ));
        let mut lenient = Blockchain::new(ChainParams::default()).unwrap();
        let oversized = lenient
            .build_block([2; 32], external_txs(3, || ScriptBuilder::new().nop()))
            .unwrap();
        assert!(matches!(
            chain.connect(&oversized),
            Err(ChainError::LimitExceeded)
        ));

        let mut witness_params = ChainParams::default();
        witness_params.limits.max_witness_bytes = witness_bytes * 2;
        let mut chain = Blockchain::new(witness_params).unwrap();
        assert!(matches!(
            chain.build_block([3; 32], external_txs(3, || ScriptBuilder::new().nop())),
            Err(ChainError::LimitExceeded)
        ));

        let constrained = || {
            ScriptBuilder::new()
                .alloc(Some(Scalar::ONE))
                .drop_()
                .alloc(Some(Scalar::ONE))
                .drop_()
        };
        let single = external_tx(constrained(), 0);
        assert_eq!(
            single
                .tx
                .verify_with_metrics(single.limits)
                .unwrap()
                .1
                .multiplications,
            1
        );
        let mut multiplication_params = ChainParams::default();
        multiplication_params
            .limits
            .max_multiplications_per_transaction = 1;
        multiplication_params.limits.max_multiplications = 2;
        let mut chain = Blockchain::new(multiplication_params).unwrap();
        assert!(matches!(
            chain.build_block([4; 32], external_txs(3, constrained)),
            Err(ChainError::LimitExceeded)
        ));
    }

    #[test]
    fn canonical_block_vector_and_bounded_decoders() {
        let mut chain = Blockchain::new(ChainParams::default()).unwrap();
        let block = chain.build_block([0x11; 32], Vec::new()).unwrap();
        let bytes = block.to_bytes().unwrap();
        let header_cell = block.header.to_cell().unwrap();
        assert_eq!(header_cell.payload().len(), 212);
        assert!(header_cell.refs().is_empty());
        assert_eq!(
            &header_cell.payload()[..12],
            &[1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(block.header.id().into_bytes(), header_cell.id());
        assert_eq!(block.header.witness_root, empty_sequence_id());
        assert_eq!(block.header.effects_root, empty_sequence_id());
        assert_eq!(
            hex::encode(block.header.state.contracts.0),
            "4ef24bb0e331b2a6fb5de8c786cd2f1ee3853690086b1b7cccefd750ea22a538"
        );
        let decoded = Block::from_bytes_bounded(&bytes, ChainParams::default()).unwrap();
        assert_eq!(decoded.to_bytes().unwrap(), bytes);
        let envelope = decode_envelope(&bytes, bytes.len()).unwrap();
        let mut extra = envelope.cells().clone();
        extra
            .insert(Arc::new(Cell::new(vec![0xfe], vec![]).unwrap()))
            .unwrap();
        let extra = CellEnvelope::new(envelope.root(), extra).unwrap().encode();
        assert!(matches!(
            Block::from_bytes_bounded(&extra, ChainParams::default()),
            Err(CellError::InvalidFormat)
        ));
        let header_wire = block.header.to_bytes().unwrap();
        assert_eq!(
            BlockHeader::from_bytes_bounded(&header_wire, 1).unwrap(),
            block.header
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            Block::from_bytes_bounded(&trailing, ChainParams::default()),
            Err(CellError::TrailingBytes)
        ));
        let mut wrong_version = block;
        wrong_version.header.version = 2;
        assert!(matches!(
            Block::from_bytes_bounded(&wrong_version.to_bytes().unwrap(), ChainParams::default()),
            Err(CellError::InvalidFormat)
        ));

        let tx = external_tx(ScriptBuilder::new().nop(), 0);
        let tx_bytes = tx.to_bytes().unwrap();
        let decoded = BlockTx::from_bytes_bounded(&tx_bytes, 1, BlockLimits::default()).unwrap();
        assert_eq!(decoded.to_bytes().unwrap(), tx_bytes);
        let limits = BlockLimits {
            max_transaction_gas: 99_999,
            ..BlockLimits::default()
        };
        assert!(matches!(
            BlockTx::from_bytes_bounded(&tx_bytes, 1, limits),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn genesis_contracts_match_a_hand_built_forest() {
        let ids = [[0x11; 32], [0x22; 32], [0x33; 32]];
        let (chain, _) =
            Blockchain::with_genesis_contracts(ChainParams::default(), &ids).expect("genesis");

        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = Forest::new().work_forest();
        for id in &ids {
            work.insert(&ContractLeaf(*id), &hasher);
        }
        let (expected, _) = work.normalize(&hasher);

        assert_eq!(
            chain.contract_forest().root(&hasher),
            expected.root(&hasher)
        );
        assert_eq!(chain.state_commitment().contracts, expected.root(&hasher));
        assert_eq!(chain.contract_forest().count(), ids.len() as u64);
        assert_eq!(chain.height(), 0);
    }

    #[test]
    fn genesis_catchup_yields_committed_proofs() {
        let ids = [[0x11; 32], [0x22; 32], [0x33; 32]];
        let (chain, catchup) =
            Blockchain::with_genesis_contracts(ChainParams::default(), &ids).expect("genesis");
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();

        for id in &ids {
            let leaf = ContractLeaf(*id);
            let proof = catchup
                .update_proof(&leaf, Proof::Transient, &hasher)
                .expect("transient proof catches up");
            let Proof::Committed(path) = &proof else {
                panic!("genesis contract must become Committed");
            };
            chain
                .contract_forest()
                .verify(&leaf, path, &hasher)
                .expect("committed proof verifies against the genesis forest");
        }
    }

    #[test]
    fn genesis_rejects_duplicate_contract_ids() {
        assert!(matches!(
            Blockchain::with_genesis_contracts(ChainParams::default(), &[[7; 32], [7; 32]]),
            Err(ChainError::DuplicateContract)
        ));
    }

    #[test]
    fn empty_genesis_matches_plain_new() {
        let plain = Blockchain::new(ChainParams::default()).expect("new");
        let (seeded, _) =
            Blockchain::with_genesis_contracts(ChainParams::default(), &[]).expect("genesis");
        assert_eq!(plain.tip(), seeded.tip());
        assert_eq!(plain.state_commitment(), seeded.state_commitment());
    }

    #[test]
    fn canonical_single_contract_root_vector() {
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = Forest::new().work_forest();
        work.insert(&ContractLeaf([0x42; 32]), &hasher);
        let (forest, _) = work.normalize(&hasher);
        assert_eq!(
            hex::encode(forest.root(&hasher).0),
            "5836e5e33bc7a3d77ed86cb4e23717d995cd4bb7d57f6d26f5c98cafae40a01a"
        );
    }

    #[test]
    fn block_tx_witness_commits_gas_proofs_and_execution_bag() {
        let mut tx = external_tx(ScriptBuilder::new().nop(), 0);
        let original = tx.witness_hash().unwrap();
        let bytes = tx.to_bytes().unwrap();
        let decoded = BlockTx::from_bytes_bounded(&bytes, 1, BlockLimits::default()).unwrap();
        assert_eq!(decoded.witness_hash().unwrap(), original);
        assert_eq!(decoded.to_bytes().unwrap(), bytes);
        tx.limits.gas -= 1;
        assert_ne!(tx.witness_hash().unwrap(), original);
        tx.limits.gas += 1;
        tx.proofs.push(Proof::Transient);
        assert_ne!(tx.witness_hash().unwrap(), original);
        tx.proofs.clear();
        let witness = Arc::new(Cell::new(vec![42], vec![]).unwrap());
        Arc::make_mut(&mut tx.tx.witnesses).insert(witness).unwrap();
        assert_ne!(tx.witness_hash().unwrap(), original);
    }

    #[test]
    fn proof_transport_preserves_legacy_bytes_and_exact_consumption() {
        for proof in [
            Proof::Transient,
            Proof::Committed(merkle::Path {
                position: 2,
                neighbors: vec![Hash([1; 32]), Hash([2; 32])],
            }),
        ] {
            let bytes = readerwriter::Encodable::encode_to_vec(&proof);
            let cell = proof_cell(&proof).unwrap();
            assert_eq!(&cell.payload()[..4], &(bytes.len() as u32).to_le_bytes());
            assert_eq!(&cell.payload()[4..], bytes);
            assert!(cell.refs().is_empty());
            let restored = decode_proof_cell(&cell, &mut ()).unwrap();
            assert_eq!(readerwriter::Encodable::encode_to_vec(&restored), bytes);
        }
        let mut trailing = CellBuilder::new();
        trailing.store_snake(&[0, 1]).unwrap();
        assert!(matches!(
            decode_proof_cell(&trailing.build(), &mut ()),
            Err(CellError::TrailingBytes)
        ));
        let mut extra_ref = CellBuilder::new();
        extra_ref
            .store_snake(&[0])
            .unwrap()
            .store_ref(CellRef::pruned([0; 32]))
            .unwrap();
        assert!(matches!(
            decode_proof_cell(&extra_ref.build(), &mut ()),
            Err(CellError::TrailingReferences)
        ));
    }

    #[test]
    fn bounded_admission_rejects_shared_dag_expansion() {
        fn wire(cell: &Cell) -> Vec<u8> {
            CellEnvelope::new(
                cell.id(),
                BagOfCells::collect(Arc::new(cell.clone())).unwrap(),
            )
            .unwrap()
            .encode()
        }
        let tx = external_tx(ScriptBuilder::new().nop(), 0);
        let proof = proof_cell(&Proof::Committed(merkle::Path {
            position: 0,
            neighbors: vec![Hash([7; 32]); 63],
        }))
        .unwrap();
        let mut root =
            CellRef::resident(Cell::new(vec![0, 0, 0], vec![CellRef::resident(proof)]).unwrap());
        // Four shared radix-4 levels encode keys 0..256 using only five
        // distinct Trie bodies. Expanding the repeated proof would allocate
        // over 500 KiB from an envelope smaller than 4 KiB.
        for level in 0..4 {
            let payload = if level == 3 {
                vec![15, 0, 12, 0, 0, 0]
            } else {
                vec![15, 0, 0]
            };
            root = CellRef::resident(Cell::new(payload, vec![root; 4]).unwrap());
        }
        let mut proofs = CellBuilder::new();
        proofs.store_u32(256).unwrap().store_ref(root).unwrap();
        let mut block_tx = CellBuilder::new();
        block_tx
            .store_u64(tx.limits.gas)
            .unwrap()
            .store_ref(CellRef::resident(tx.tx.to_cell().unwrap()))
            .unwrap()
            .store_ref(CellRef::resident(proofs.build()))
            .unwrap();
        let block_tx = block_tx.build();
        let bytes = wire(&block_tx);
        assert!(bytes.len() < 4_096);
        let limits = BlockLimits {
            max_witness_bytes: 16_384,
            ..BlockLimits::default()
        };
        assert!(matches!(
            BlockTx::from_bytes_bounded(&bytes, 1, limits),
            Err(CellError::ResourceExhausted)
        ));
        // The graph is otherwise canonical, not merely malformed. Builders
        // holding the expanded values must obey the same configured bound.
        let expanded = BlockTx::from_bytes_bounded(&bytes, 1, BlockLimits::default()).unwrap();
        assert_eq!(expanded.proofs.len(), 256);
        let params = ChainParams {
            limits,
            ..ChainParams::default()
        };
        let mut chain = Blockchain::new(params).unwrap();
        let previous_tip = chain.tip();
        assert!(matches!(
            chain.build_block([0; 32], vec![expanded]),
            Err(ChainError::Cells(CellError::ResourceExhausted))
        ));
        assert_eq!(chain.tip(), previous_tip);

        let header = Blockchain::new(ChainParams::default()).unwrap().header;
        let mut transactions = Trie::new(4).unwrap();
        transactions.insert(&[0; 4], block_tx, &mut ()).unwrap();
        let mut transaction_list = CellBuilder::new();
        transaction_list
            .store_u32(1)
            .unwrap()
            .store_ref(transactions.into_root().unwrap())
            .unwrap();
        let mut block = CellBuilder::new();
        block
            .store_ref(CellRef::resident(header.to_cell().unwrap()))
            .unwrap()
            .store_ref(CellRef::resident(transaction_list.build()))
            .unwrap();
        let bytes = wire(&block.build());
        assert!(matches!(
            Block::from_bytes_bounded(&bytes, params),
            Err(CellError::ResourceExhausted)
        ));
    }

    #[test]
    fn descendants_run_immediately_with_only_the_initiating_witness_bag() {
        let mut params = ChainParams::default();
        params.storage.lease_duration_blocks = 1;
        let mut chain = Blockchain::new(params).unwrap();
        let actor = deploy_actor(
            &mut chain.actors,
            61,
            ScriptBuilder::new().nop().to_bytecode(),
        );
        let tree = flamevm::PredicateTree::scripts_only(
            vec![ScriptBuilder::new().drop_().to_bytecode()],
            [3; 32],
        )
        .unwrap();
        let (opening, branch_cells) = tree.witness_for(0).unwrap();
        let inputs = [70, 71].map(|anchor| {
            Contract::new(
                Predicate::tree(tree.clone()),
                Anchor([anchor; 32]),
                empty_state(),
            )
            .unwrap()
        });
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
        for input in &inputs {
            work.insert(&ContractLeaf(input.id()), &hasher);
        }
        let (forest, catchup) = work.normalize(&hasher);
        chain.contracts = forest;
        let archive = chain.actor_storage(&actor).unwrap().cells;
        let expiry = chain.build_block([1; 32], vec![]).unwrap();
        chain.connect(&expiry).unwrap();
        let frozen = chain.actor_storage(&actor).unwrap().cells.id();

        let send = |input: &Contract| {
            ScriptBuilder::new()
                .with_cells(branch_cells.clone())
                .push_str(flamevm::String::contract(input.clone()))
                .input()
                .push_point(*opening.internal_key.as_bytes())
                .push_str(opening.root.to_vec())
                .push_int(opening.index)
                .push_int(20_000u64)
                .push_int(0u64)
                .open()
                .verify()
                .drop_()
                .push_int(0u64)
                .push_str(refund_predicate().to_point().as_bytes().to_vec())
                .push_int(20_000u64)
                .push_str(actor.to_hash().to_vec())
                .send()
        };
        let mut first = external_tx(send(&inputs[0]), 0);
        let mut second = external_tx(send(&inputs[1]).with_cells(archive.as_ref().clone()), 1);
        first.proofs.push(
            catchup
                .update_proof(&ContractLeaf(inputs[0].id()), Proof::Transient, &hasher)
                .unwrap(),
        );
        second.proofs.push(
            catchup
                .update_proof(&ContractLeaf(inputs[1].id()), Proof::Transient, &hasher)
                .unwrap(),
        );
        let block = chain.build_block([2; 32], vec![first, second]).unwrap();
        // Decode through the shared transport graph, which must not turn the
        // second transaction's private availability set into a global witness pool.
        let decoded = Block::from_bytes_bounded(&block.to_bytes().unwrap(), params).unwrap();
        let applied = chain.connect(&decoded).unwrap();
        assert_eq!(
            applied
                .records
                .iter()
                .map(|record| record.kind)
                .collect::<Vec<_>>(),
            [
                ExecutionKind::External,
                ExecutionKind::InternalFailed,
                ExecutionKind::External,
                ExecutionKind::Internal
            ]
        );
        assert_eq!(chain.actor_storage(&actor).unwrap().cells.id(), frozen);
    }

    #[test]
    fn sequence_decoder_rejects_wrong_count_sparse_keys_and_extra_refs() {
        let mut trie = Trie::new(4).unwrap();
        trie.insert(&1u32.to_be_bytes(), 7u64.to_cell().unwrap(), &mut ())
            .unwrap();
        let mut sparse = CellBuilder::new();
        sparse
            .store_u32(1)
            .unwrap()
            .store_ref(trie.into_root().unwrap())
            .unwrap();
        assert!(matches!(
            decode_sequence(&sparse.build(), &mut (), 1, u64::from_cell),
            Err(CellError::InvalidFormat)
        ));
        let mut empty = CellBuilder::new();
        empty
            .store_u32(0)
            .unwrap()
            .store_ref(CellRef::pruned([1; 32]))
            .unwrap();
        assert!(matches!(
            decode_sequence(&empty.build(), &mut (), 1, u64::from_cell),
            Err(CellError::TrailingReferences)
        ));
        assert!(matches!(
            decode_sequence(&2u32.to_cell().unwrap(), &mut (), 1, u64::from_cell),
            Err(CellError::LimitExceeded)
        ));
    }

    fn fail_and_bounce(store: &mut ActorStore, original: Message, height: u64) -> (VMError, TxLog) {
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
                Scalar::from(7u64),
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
        let (error, _) =
            fail_and_bounce(&mut malformed, message(actor, payload(), 1_000_000, 2), 0);
        assert!(matches!(error, VMError::UnknownOpcode(0xff)));

        let mut failing = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(
            &mut failing,
            3,
            ScriptBuilder::new().push_int(0u64).verify().to_bytecode(),
        );
        let (error, _) = fail_and_bounce(&mut failing, message(actor, payload(), 1_000_000, 3), 0);
        assert!(matches!(error, VMError::VerifyFailed));

        let mut dirty = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(
            &mut dirty,
            4,
            ScriptBuilder::new().push_int(1u64).to_bytecode(),
        );
        let (error, _) = fail_and_bounce(&mut dirty, message(actor, payload(), 1_000_000, 4), 0);
        assert!(matches!(error, VMError::StackNotClean));

        let mut out_of_gas = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(&mut out_of_gas, 5, ScriptBuilder::new().nop().to_bytecode());
        let (error, _) = fail_and_bounce(&mut out_of_gas, message(actor, payload(), 0, 5), 0);
        assert!(matches!(error, VMError::OutOfGas));

        let mut checked_out = ActorStore::new(StorageParams::default()).unwrap();
        let actor = deploy_actor(
            &mut checked_out,
            6,
            ScriptBuilder::new().nop().to_bytecode(),
        );
        checked_out.load_state(&actor).unwrap();
        let (error, _) =
            fail_and_bounce(&mut checked_out, message(actor, payload(), 1_000_000, 6), 0);
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
            Scalar::ZERO,
            Value::Token(
                Token::cleartext(Scalar::from(11u64), FLAME_FLAVOR).expect("quantity is in range"),
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
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
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
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
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
        let mut work = chain.contracts.work_forest();
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
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
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
            state_root(&Value::Scalar(Scalar::ONE))
        );
        assert_eq!(
            state_root(&chain.actors.load_state(&child).unwrap()),
            state_root(&Value::Scalar(Scalar::from(2u64)))
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
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
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
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
        let output = Contract::new(
            refund_predicate(),
            Anchor([91; 32]),
            Value::Scalar(Scalar::ONE),
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
            Scalar::ZERO,
            Value::Token(
                Token::cleartext(Scalar::from(11u64), FLAME_FLAVOR).expect("quantity is in range"),
            ),
        );
        let values = vec![
            Value::ClearToken(ClearToken::new(Scalar::from(7u64), FLAME_FLAVOR)),
            Value::Token(
                Token::cleartext(Scalar::from(9u64), FLAME_FLAVOR).expect("quantity is in range"),
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
            assert!(
                result
                    .log()
                    .iter()
                    .any(|entry| matches!(entry, TxEntry::ActorSave { .. }))
            );
            assert!(
                !result
                    .log()
                    .iter()
                    .any(|entry| matches!(entry, TxEntry::Output(_)))
            );
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
        let hasher = utreexo::utreexo_hasher::<ContractLeaf>();
        let mut work = chain.contracts.work_forest();
        let mut sends = VecDeque::new();
        let mut seen_outputs = BTreeSet::new();
        let original = message(ActorID::Hash([50; 32]), Vec::new(), 1_000, 50);
        let bounce = Blockchain::bounce_log(original).unwrap();
        let bounce_id = bounce
            .iter()
            .find_map(|entry| match entry {
                TxEntry::Output(contract) => Some(contract.id()),
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
            Err(ChainError::DuplicateContract)
        ));
        assert_eq!(work.normalize(&hasher).0.count(), 0);
    }

    #[test]
    fn expiry_freezes_tokens_without_retirement_and_disconnect_restores_bodies() {
        let actor = ActorID::Hash([9; 32]);
        let state = Value::ClearToken(ClearToken::new(Scalar::from(7u64), FLAME_FLAVOR));
        let mut params = ChainParams::default();
        params.storage.lease_duration_blocks = 1;
        let mut chain = Blockchain::new(params).unwrap();
        chain
            .actors
            .deploy(
                actor.clone(),
                ScriptBuilder::new().nop().to_bytecode(),
                state.clone(),
            )
            .unwrap();
        chain
            .actors
            .purchase_storage(&actor, 1_024, 0)
            .unwrap()
            .unwrap();
        let before = chain.actor_storage(&actor).unwrap();
        let before_root = chain.actors.actor_root();
        let block = chain.build_block([10; 32], Vec::new()).unwrap();
        let applied = chain.connect(&block).unwrap();
        assert!(applied.records.is_empty());
        assert!(chain.actors.exists(&actor));
        assert!(chain.actors.load_state(&actor).is_err());
        let frozen = chain.actor_storage(&actor).unwrap();
        assert_ne!(before.cells.id(), frozen.cells.id());
        assert_eq!(chain.actors.actor_usage(&actor).unwrap(), 0);
        chain.actors.assert_supply(1).unwrap();

        chain.actors.push_checkpoint();
        let mut witnesses = before.cells.as_ref().clone();
        let restored = chain
            .actors
            .load_state_with_cells(&actor, &mut witnesses)
            .unwrap();
        assert_eq!(state_root(&restored), state_root(&state));
        chain.actors.pop_checkpoint_rollback();
        assert_eq!(
            chain.actor_storage(&actor).unwrap().cells.id(),
            frozen.cells.id()
        );

        chain.disconnect_tip(applied.id).unwrap();
        assert_eq!(chain.actors.actor_root(), before_root);
        assert_eq!(
            chain.actor_storage(&actor).unwrap().cells.id(),
            before.cells.id()
        );
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
