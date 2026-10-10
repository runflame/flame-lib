use bulletproofs::r1cs::R1CSProof;
use bulletproofs::PedersenGens;
use cells::{
    Cell, CellBuilder, CellDecode, CellEncode, CellError, CellIndex, CellReader, CellRef,
    CellSlice, Trie,
};
use core::convert::TryFrom;
use curve25519_dalek::ristretto::CompressedRistretto;
use musig::Signature;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::actor::{ActorID, ActorRegistry};
use crate::contract::{Contract, ContractID};
use crate::encoding::{blob_cell, read_blob};
use crate::errors::VMError;
use crate::message::Message;
use crate::prover::Prover;
use crate::script::ScriptBuilder;
use crate::verifier::Verifier;
use crate::vm::{BlockContext, DeferredSig, VM};
use crate::{Scalar, Value};

#[allow(dead_code)]
mod wire {
    use std::convert::TryInto;
    include!(concat!(env!("OUT_DIR"), "/tx_wire.rs"));
}

/// Header metadata for the transaction
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct TxHeader {
    /// Version of the transaction
    pub version: u32,

    /// Timestamp before which tx is invalid, compatible with Bitcoin
    pub locktime: u32,
}

#[derive(Clone)]
pub struct ExternalTx {
    /// Header metadata
    pub header: TxHeader,

    /// Script representing the transaction
    pub script: Vec<u8>,

    /// Aggregate TxID-bound signature, absent when the script records no
    /// `signtx` authorizations. Explicit `signcall` signatures live in script.
    pub signature: Option<Signature>,

    /// Constraint system proof for all the constraints
    pub proof: R1CSProof,

    /// Immutable public bodies available to this execution and all its descendants.
    pub witnesses: Arc<CellIndex>,
    /// Factual CellID of the body, including the supplied program and log claim.
    pub txid: TxID,
    /// Canonical mask-one pruning record for the log's level-zero hash/depth.
    log_commitment: Cell,
    /// Literal refs, including explicit continuations, excluding the lookup index.
    program_root: Cell,
}

impl CellEncode for TxHeader {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        b.store_u32(self.version)?.store_u32(self.locktime)?;
        Ok(())
    }
}
impl CellDecode for TxHeader {
    fn decode<R: CellReader + ?Sized>(
        s: &mut CellSlice<'_>,
        _r: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self {
            version: s.load_u32()?,
            locktime: s.load_u32()?,
        })
    }
}

fn transaction_program(program: &Cell, witnesses: &CellIndex) -> Result<Cell, CellError> {
    Cell::new(
        Vec::new(),
        vec![program.clone().into(), witnesses.to_cell()?.into()],
    )
}

pub(crate) fn pruned_log(entries: &[TxEntry]) -> Result<Cell, CellError> {
    let log = log_cell(entries)?;
    Cell::from_pruned(1, vec![log.hash(0)?], vec![log.depth(0)?])
}

#[cfg(test)]
fn body_cell(
    header: TxHeader,
    script: &[u8],
    witnesses: &CellIndex,
    log: &Cell,
) -> Result<Cell, CellError> {
    program_body(header, &crate::script::script_cell(script)?, witnesses, log)
}

fn program_body(
    header: TxHeader,
    program: &Cell,
    witnesses: &CellIndex,
    log: &Cell,
) -> Result<Cell, CellError> {
    if !log.is_pruned() || log.level_mask() != 1 {
        return Err(CellError::InvalidFormat);
    }
    wire::TxBody {
        header: wire::TxHeader {
            version: header.version,
            locktime: header.locktime,
        },
        program: transaction_program(program, witnesses)?.into(),
        log: cells::ctl::Ref::from_reference(log.clone().into()),
    }
    .to_cell()
}

pub(crate) fn external_program_body(
    header: TxHeader,
    program: &Cell,
    witnesses: &CellIndex,
    entries: &[TxEntry],
) -> Result<Cell, CellError> {
    program_body(header, program, witnesses, &pruned_log(entries)?)
}

impl ExternalTx {
    pub fn header(&self) -> TxHeader {
        self.header
    }
    pub fn script(&self) -> &[u8] {
        &self.script
    }
    /// Executable program, including literal operands but not the lookup index.
    pub fn program(&self) -> Result<Cell, CellError> {
        Cell::new(self.script.clone(), self.program_root.refs().to_vec())
    }
    pub fn witnesses(&self) -> &CellIndex {
        &self.witnesses
    }
    /// All supplied execution bodies: the lookup snapshot plus resident program
    /// subcells. Share this exact context with every induced actor transaction.
    pub fn execution_cells(&self) -> Result<Arc<CellIndex>, CellError> {
        let mut available = self.witnesses.as_ref().clone();
        available.extend(&CellIndex::collect(Arc::new(self.program()?))?)?;
        Ok(Arc::new(available))
    }
    pub fn signature_bytes(&self) -> Option<[u8; 64]> {
        self.signature.map(|s| s.to_bytes())
    }
    pub fn proof_bytes(&self) -> Vec<u8> {
        self.proof.to_bytes()
    }
    pub fn effect_id(&self) -> EffectID {
        EffectID(self.log_commitment.hash(0).expect("level zero"))
    }

    pub fn body(&self) -> Result<Cell, CellError> {
        let body = program_body(
            self.header,
            &self.program()?,
            &self.witnesses,
            &self.log_commitment,
        )?;
        if body.id() != self.txid.0 {
            return Err(CellError::InvalidFormat);
        }
        Ok(body)
    }

    /// Factual root ID including the signature and R1CS proof.
    pub fn witness_id(&self) -> Result<cells::CellID, CellError> {
        Ok(self.to_cell()?.id())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CellError> {
        self.to_cell()?
            .encode_transport(&mut self.witnesses.as_ref().clone())
    }

    pub fn from_bytes_bounded(
        bytes: &[u8],
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, CellError> {
        // Outer admission bounds bytes; decoding also bounds graph/hash work.
        let mut gas = (bytes.len() as u64)
            .saturating_mul(1024)
            .saturating_add(1024);
        let root = Cell::decode_transport(bytes, bytes.len(), &mut gas)?;
        let mut slice = CellSlice::new(&root);
        let tx = Self::decode_bounded(
            &mut slice,
            &mut (),
            expected_version,
            max_script_bytes,
            max_proof_bytes,
        )?;
        slice.finish()?;
        if tx.to_bytes()? != bytes {
            return Err(CellError::InvalidFormat);
        }
        Ok(tx)
    }

    pub fn verify(&self, limits: Limits) -> Result<TxLog, VMError> {
        self.verify_with_metrics(limits).map(|(log, _)| log)
    }
    pub fn verify_with_metrics(&self, limits: Limits) -> Result<(TxLog, TxMetrics), VMError> {
        self.body()?;
        let result = Verifier::verify_cell_with_cells(
            &PedersenGens::default(),
            self.program()?,
            &self.proof,
            self.header,
            limits.gas,
            self.signature,
            &self.witnesses,
        )?;
        if result.txid != self.txid
            || pruned_log(&result.txlog)?.commitment() != self.log_commitment.commitment()
        {
            return Err(CellError::InvalidFormat.into());
        }
        Ok((
            TxLog(result.txlog),
            TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                multiplications: result.multiplications,
            },
        ))
    }

    pub fn encoded_size(&self) -> usize {
        self.to_bytes()
            .expect("constructed transaction is encodable")
            .len()
    }
}

impl CellEncode for ExternalTx {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        let signature = self.signature_bytes().unwrap_or([0; 64]);
        if self.signature.is_some() && signature == [0; 64] {
            return Err(CellError::InvalidFormat);
        }
        let proof = self.proof_bytes();
        wire::Tx {
            body: cells::ctl::Ref::from_reference(self.body()?.into()),
            signature,
            r1cs_length: u16::try_from(proof.len()).map_err(|_| CellError::LimitExceeded)?,
            r1cs_proof: proof,
        }
        .encode(b)
    }
}

impl ExternalTx {
    /// Reads one transaction root with bounded root-code and inline-proof sizes.
    /// The surrounding transport owns the total byte bound and canonical Cell hierarchy.
    pub fn from_cell_bounded<R: CellReader + ?Sized>(
        cell: &Cell,
        resolver: &mut R,
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, CellError> {
        let mut slice = CellSlice::new(cell);
        let tx = Self::decode_bounded(
            &mut slice,
            resolver,
            expected_version,
            max_script_bytes,
            max_proof_bytes,
        )?;
        slice.finish()?;
        Ok(tx)
    }

    fn decode_bounded<R: CellReader + ?Sized>(
        s: &mut CellSlice<'_>,
        r: &mut R,
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, CellError> {
        let length = s.preload(|s| {
            s.load_ref()?;
            s.load_bytes(64)?;
            s.load_u16()
        })?;
        if usize::from(length) > max_proof_bytes {
            return Err(CellError::LimitExceeded);
        }
        let tx = wire::Tx::decode(s, r)?;
        let signature = if tx.signature == [0; 64] {
            None
        } else {
            Some(Signature::from_bytes(tx.signature).map_err(|_| CellError::InvalidFormat)?)
        };
        let proof = R1CSProof::from_bytes(&tx.r1cs_proof).map_err(|_| CellError::InvalidFormat)?;
        let body_cell = cells::read_cell(r, tx.body.reference())?;
        let body = wire::TxBody::from_cell(&body_cell, r)?;
        let header = TxHeader {
            version: body.header.version,
            locktime: body.header.locktime,
        };
        if expected_version != 1 || header.version != expected_version {
            return Err(CellError::InvalidFormat);
        }
        let script_cell = cells::read_cell(r, &body.program)?;
        let mut script_slice = CellSlice::new(&script_cell);
        let program = cells::read_cell(r, &script_slice.load_ref()?)?;
        if program.is_pruned() {
            return Err(CellError::PrunedCell);
        }
        if program.payload().len() > max_script_bytes {
            return Err(CellError::LimitExceeded);
        }
        let script = program.payload().to_vec();
        let program_root = program.load_graph(r)?;
        let witness_root = cells::read_cell(r, &script_slice.load_ref()?)?;
        script_slice.finish()?;
        let witnesses = CellIndex::from_cell(&witness_root, r)?;
        let log_commitment = cells::read_cell(r, body.log.reference())?.as_ref().clone();
        let txid = TxID(body_cell.id());
        let decoded = Self {
            header,
            script,
            signature,
            proof,
            witnesses: Arc::new(witnesses),
            txid,
            log_commitment,
            program_root,
        };
        decoded.body()?;
        Ok(decoded)
    }
}

impl CellDecode for ExternalTx {
    fn decode<R: CellReader + ?Sized>(s: &mut CellSlice<'_>, r: &mut R) -> Result<Self, CellError> {
        Self::decode_bounded(s, r, 1, u32::MAX as usize, u32::MAX as usize)
    }
}

/// Resource limits for one transaction's execution.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Compute (gas) budget.
    pub gas: u64,
}

/// Ordered transaction effects — the canonical change set a node
/// applies to its state. Its semantic root is [`EffectID`], not external TxID.
pub struct TxLog(Vec<TxEntry>);

/// For the node layer (and tests): wrap a re-derived effect list.
/// The crate itself only ever produces TxLogs by execution.
impl From<Vec<TxEntry>> for TxLog {
    fn from(entries: Vec<TxEntry>) -> Self {
        TxLog(entries)
    }
}

impl TxLog {
    /// Level-zero commitment to the computed effects.
    pub fn effect_id(&self) -> EffectID {
        EffectID(
            log_cell(&self.0)
                .expect("admitted effects have Cell encodings")
                .hash(0)
                .expect("level zero"),
        )
    }
    /// The effect entries in canonical order.
    pub fn entries(&self) -> &[TxEntry] {
        &self.0
    }
    /// Iterates the effect entries.
    pub fn iter(&self) -> std::slice::Iter<'_, TxEntry> {
        self.0.iter()
    }

    /// Sum of gas grants on messages emitted directly by this execution.
    /// Descendant messages are charged to their own derived logs.
    pub fn direct_send_gas(&self) -> Option<u64> {
        self.0.iter().try_fold(0u64, |sum, entry| match entry {
            TxEntry::Send(message) => sum.checked_add(message.gas),
            _ => Some(sum),
        })
    }

    /// Consumes the log and returns its ordered effects. Consensus code uses
    /// this to move linear contracts and messages into the block transition
    /// without cloning bearer values.
    pub fn into_entries(self) -> Vec<TxEntry> {
        self.0
    }
}

/// Actual resource counters surfaced for consensus admission and diagnostics.
/// They are re-derived by execution and are not part of the [`TxID`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxMetrics {
    pub gas_used: u64,
    pub total_fee: u64,
    pub multiplications: usize,
}

/// What the sender must aggregate-sign before broadcast. Build the
/// signature with `musig::Signature::sign_multi(keys, items, t)` where
/// `t` is a `b"flamevm.signtx"` transcript with `txid` appended under
/// `b"txid"`; pass the result to [`UnsignedTx::sign`].
pub struct SigningInstructions {
    pub txid: TxID,
    /// The tx-bound `(verification_key, contract_id)` authorizations, in
    /// `signtx` order — the multi-message context to sign.
    pub items: Vec<(CompressedRistretto, ContractID)>,
}

/// A built-but-unsigned external transaction (lifecycle step 1→2).
pub struct UnsignedTx {
    header: TxHeader,
    script: Vec<u8>,
    witnesses: Arc<CellIndex>,
    proof: R1CSProof,
    log: TxLog,
    metrics: TxMetrics,
    txbound_items: Vec<(CompressedRistretto, ContractID)>,
    txid: TxID,
    log_commitment: Cell,
    program_root: Cell,
}

impl UnsignedTx {
    /// Exact public witness set frozen by the prover. Private assignments and
    /// openings are absent; signing preserves this same bag in `ExternalTx`.
    pub fn witnesses(&self) -> &CellIndex {
        &self.witnesses
    }

    /// The transaction effects.
    pub fn log(&self) -> &TxLog {
        &self.log
    }
    /// Resource counters (measurement only).
    pub fn metrics(&self) -> TxMetrics {
        self.metrics
    }
    pub fn txid(&self) -> TxID {
        self.txid
    }
    /// The keys + txid the sender signs over (lifecycle step 2 input).
    pub fn signing_instructions(&self) -> SigningInstructions {
        SigningInstructions {
            txid: self.txid,
            items: self.txbound_items.clone(),
        }
    }
    /// Attaches the aggregate signature → broadcastable [`ExternalTx`].
    pub fn sign(self, signature: Signature) -> ExternalTx {
        ExternalTx {
            header: self.header,
            script: self.script,
            signature: Some(signature),
            proof: self.proof,
            txid: self.txid,
            log_commitment: self.log_commitment,
            witnesses: self.witnesses,
            program_root: self.program_root,
        }
    }

    /// Finalizes a transaction that recorded no `signtx` authorizations.
    pub fn without_signature(self) -> Result<ExternalTx, VMError> {
        if !self.txbound_items.is_empty() {
            return Err(VMError::MissingTxBoundSignature);
        }
        Ok(ExternalTx {
            header: self.header,
            script: self.script,
            signature: None,
            proof: self.proof,
            txid: self.txid,
            log_commitment: self.log_commitment,
            witnesses: self.witnesses,
            program_root: self.program_root,
        })
    }
}

impl ScriptBuilder {
    /// Lifecycle step 1: build an unsigned external transaction by
    /// running the witness-bearing program through the prover. Embedded Contract
    /// bodies, selected predicate paths, and nested witnesses are collected into
    /// the frozen Cell hierarchy carried through `UnsignedTx` to `ExternalTx::verify`.
    /// Bulletproof generators are managed inside the crate.
    pub fn build_tx(self, header: TxHeader, limits: Limits) -> Result<UnsignedTx, VMError> {
        let pc_gens = PedersenGens::default();
        let result = Prover::prove(&pc_gens, self, header, limits.gas)?;
        let txbound_items = result
            .deferred_sigs
            .iter()
            .filter_map(|s| match s {
                DeferredSig::TxBound {
                    verification_key,
                    contract_id,
                } => Some((*verification_key, *contract_id)),
                DeferredSig::Explicit { .. } => None,
            })
            .collect();
        Ok(UnsignedTx {
            txid: result.txid,
            log_commitment: pruned_log(&result.txlog)?,
            header,
            script: result.program.payload().to_vec(),
            program_root: result
                .program
                .load_graph(&mut result.cells.as_ref().clone())?,
            witnesses: result.cells,
            proof: result.proof.expect("prover always sets the proof"),
            metrics: TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                multiplications: result.multiplications,
            },
            txbound_items,
            log: TxLog(result.txlog),
        })
    }
}

/// An internal transaction's outcome (lifecycle step 4): the effects
/// to apply + resource counters. Produced by [`Message::execute_tx`].
pub struct InternalTx {
    log: TxLog,
    metrics: TxMetrics,
}

impl InternalTx {
    /// The effects to apply to chain state.
    pub fn log(&self) -> &TxLog {
        &self.log
    }
    /// Resource counters (measurement only).
    pub fn metrics(&self) -> TxMetrics {
        self.metrics
    }

    /// Consumes the result and returns its effect log.
    pub fn into_log(self) -> TxLog {
        self.log
    }
}

impl Message {
    /// Runs one derived internal transaction directly against the chain's
    /// checkpointed actor registry. This avoids cloning and partially replaying
    /// consensus state; FlameVM commits or rolls back the registry atomically.
    pub fn execute_tx(
        self,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
    ) -> Result<InternalTx, VMError> {
        self.execute_tx_with_cells(registry, block, Arc::new(CellIndex::new()))
    }

    /// Executes a descendant with its initiating external transaction's exact
    /// witness availability, never a block-wide coalesced bag.
    pub fn execute_tx_with_cells(
        self,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
        cells: Arc<CellIndex>,
    ) -> Result<InternalTx, VMError> {
        // Internal-tx header: fixed default for now — its source is part
        // of the block envelope design.
        let header = TxHeader {
            version: 1,
            locktime: 0,
        };
        let result = VM::execute_internal_with_cells(header, self, registry, block, cells)?;
        Ok(InternalTx {
            log: TxLog(result.txlog),
            metrics: TxMetrics {
                gas_used: result.gas_used,
                total_fee: result.total_fee,
                multiplications: result.multiplications,
            },
        })
    }
}

/// External body CellID, or the effect-derived ID of an internal execution.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxID(pub [u8; 32]);

/// Level-zero hash of the computed effect log.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectID(pub [u8; 32]);

/// Entry in the computed effects. The external body commits their level-zero root.
///
/// `Clone`/`Serialize`/`Deserialize` are still withheld (the linear
/// `Contract`/`Token` payloads don't participate); downstream code wanting
/// those should hash entries to bytes first.
#[derive(Debug)]
pub enum TxEntry {
    /// Tx header — bound at run start as the first txlog entry so
    /// `version` and `locktime` participate in `TxID::from_log`.
    Header(TxHeader),

    /// Plain data entry created by `log` instruction. Contains arbitrary binary string.
    Data(Vec<u8>),

    /// Input: a consumed contract's identity. Emitted by `input` (external-only).
    /// Commits the contract that the transaction has consumed without re-storing
    /// the payload — the existence of the contract is independently asserted by
    /// the Utreexo proof outside the VM.
    Input(ContractID),

    /// Receive: the MessageID consumed by an internal transaction. Emitted
    /// by `VM::execute_internal` as the first effect after `Header`,
    /// committing the originating `Send`'s anchor into the Internal TxID
    /// merkle root. Without this entry, the Internal TxID would not
    /// commit to its triggering Send (the originating `TxEntry::Send`
    /// lives in the *external* tx's log, hence in External TxID only).
    /// Symmetric with `Input` for external transactions: both are
    /// "what triggered me" effects emitted before the body runs.
    Receive([u8; 32]),

    /// Successful first delivery to a constructor-form actor. Carries the
    /// canonical actor id and full constructor code so state-machine replay
    /// does not need the original Message. The initial state is the canonical
    /// empty state.
    ActorDeploy { actor: ActorID, code: Vec<u8> },

    /// Output: a newly sealed contract, emitted by the `output` opcode.
    Output(Contract),

    /// Cleartext issuance (emitted by `op_issuepub`). Carries the
    /// cleartext `(qty, flv)` pair as `Scalar`s — public on the wire,
    /// directly auditable. Flavor is `flavor_from_actor(actor, tag)`
    /// for the actor that ran `issuepub`.
    IssuePub(Scalar, Scalar),

    /// Confidential issuance (emitted by `op_issuepriv`). Carries
    /// `(qty_point, flv_point)` — the live Pedersen commitment to the
    /// qty and the unblinded commitment to the flavor scalar. Flavor
    /// is `flavor_from_predicate(predicate, tag)` for the predicate
    /// whose `ContractOpen` frame ran `issuepriv`. Soundness of the qty
    /// commitment + 64-bit range proof is established through the
    /// constraint system.
    IssuePriv(CompressedRistretto, CompressedRistretto),

    /// Retirement: an asset value has been *destroyed* from circulation.
    /// Same `(qty_point, flv_point)` shape as `Issue`. Cleartext or
    /// encrypted symmetrically.
    Retire(CompressedRistretto, CompressedRistretto),

    /// Fee: a transaction fee of `qty` flames recorded by `op_fee`.
    /// Carried as a bare `u64` (no commitment) — the cleartext
    /// branch is currently the only defined fee shape; the matching
    /// debt half is the `WideToken` returned to the stack.
    /// Aggregated by `VM::total_fee` (a `CheckedFee`) into the
    /// eventual `TxResult.total_fee`.
    Fee(u64),

    /// Actor-state mutation recorded by `op_save`. Carries the
    /// actor's identity and the **full** post-save state Value —
    /// symmetric with `Output(Contract)` which carries the full Contract.
    /// The state machine consumes this entry by replacing the
    /// actor's stored state with `state`; no re-execution of the
    /// script needed. See docs/flamevm.md §"Design"; TxLog records effects, not
    /// control flow".
    ///
    /// The entry Cell contains the actor hash and a reference to the state
    /// Value Cell. Its identity commits to that child without flattening it.
    ActorSave { actor: ActorID, state: Value },

    /// Actor-code replacement recorded by `setcode`. Carries the full
    /// new code blob for state-machine replay; the entry Cell contains the
    /// actor hash and a reference to native executable code. Symmetric with
    /// `ActorSave`. See ADR 0018.
    SetCode { actor: ActorID, code: Vec<u8> },

    /// Outbound asynchronous message scheduled by `op_send`. Carries
    /// the full [`Message`](Message) — its `anchor` is
    /// the `left` half of a split of `last_anchor` at the send site,
    /// and its `id()` is the canonical MessageID (deterministic at
    /// broadcast time, identifies the future internal-tx delivery).
    ///
    /// The block builder reads `TxEntry::Send` entries directly from
    /// the TxLog — there is no separate "sends" queue — and feeds the
    /// embedded `Message` straight into `VM::execute_internal`.
    /// Symmetric with `TxEntry::Output(Contract)`: each effect that owns
    /// an addressable artifact embeds the artifact itself.
    Send(Message),

    /// Persistent storage purchased by an actor. Replay recomputes the quote
    /// from the ordered storage-pool state and requires this allocation and
    /// fee to match exactly.
    StoragePurchase {
        actor: ActorID,
        bytes: u64,
        expiry_height: u64,
        fee_sparks: Scalar,
    },

    /// Explicit removal after state dismantling. Lease expiry freezes the
    /// actor's committed bodies instead of destroying its linear contents.
    ActorDestroy { actor: ActorID },
}

impl TxID {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    /// Effect-derived ID for internal execution, not an external body TxID.
    pub fn from_log(txlog: &[TxEntry]) -> Self {
        Self(
            log_cell(txlog)
                .expect("admitted effects have Cell encodings")
                .id(),
        )
    }
}

fn log_cell(entries: &[TxEntry]) -> Result<Cell, CellError> {
    let mut trie = Trie::new(8)?;
    for (i, entry) in entries.iter().enumerate() {
        trie.insert(&(i as u64).to_be_bytes(), entry.to_cell()?, &mut ())?;
    }
    let mut root = CellBuilder::new();
    root.store_u64(entries.len() as u64)?;
    if let Some(reference) = trie.into_root() {
        root.store_ref(reference)?;
    }
    Ok(root.build())
}

impl TxEntry {
    pub const TAG_HEADER: u8 = 0;
    pub const TAG_DATA: u8 = 1;
    pub const TAG_INPUT: u8 = 2;
    pub const TAG_RECEIVE: u8 = 3;
    pub const TAG_OUTPUT: u8 = 4;
    pub const TAG_ISSUE_PUB: u8 = 5;
    pub const TAG_ISSUE_PRIV: u8 = 6;
    pub const TAG_RETIRE: u8 = 7;
    pub const TAG_FEE: u8 = 8;
    pub const TAG_ACTOR_SAVE: u8 = 9;
    pub const TAG_SET_CODE: u8 = 10;
    pub const TAG_SEND: u8 = 11;
    pub const TAG_STORAGE_PURCHASE: u8 = 12;
    pub const TAG_ACTOR_DESTROY: u8 = 13;
    pub const TAG_ACTOR_DEPLOY: u8 = 14;
}

impl CellEncode for TxEntry {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        match self {
            Self::Header(h) => {
                b.store_u8(Self::TAG_HEADER)?.store(h)?;
            }
            Self::Data(bytes) => {
                b.store_u8(Self::TAG_DATA)?
                    .store_ref(CellRef::resident(blob_cell(bytes)?))?;
            }
            Self::Input(id) => {
                b.store_u8(Self::TAG_INPUT)?.store_bytes(id)?;
            }
            Self::Receive(id) => {
                b.store_u8(Self::TAG_RECEIVE)?.store_bytes(id)?;
            }
            Self::Output(contract) => {
                b.store_u8(Self::TAG_OUTPUT)?
                    .store_ref(CellRef::resident(contract.to_cell()?))?;
            }
            Self::IssuePub(qty, flv) => {
                b.store_u8(Self::TAG_ISSUE_PUB)?.store(qty)?.store(flv)?;
            }
            Self::IssuePriv(qty, flv) => {
                b.store_u8(Self::TAG_ISSUE_PRIV)?
                    .store_bytes(qty.as_bytes())?
                    .store_bytes(flv.as_bytes())?;
            }
            Self::Retire(qty, flv) => {
                b.store_u8(Self::TAG_RETIRE)?
                    .store_bytes(qty.as_bytes())?
                    .store_bytes(flv.as_bytes())?;
            }
            Self::Fee(qty) => {
                b.store_u8(Self::TAG_FEE)?.store_u64(*qty)?;
            }
            Self::ActorSave { actor, state } => {
                b.store_u8(Self::TAG_ACTOR_SAVE)?
                    .store_bytes(&actor.to_hash())?
                    .store_ref(CellRef::resident(state.to_cell()?))?;
            }
            Self::ActorDeploy { actor, code } | Self::SetCode { actor, code } => {
                b.store_u8(if matches!(self, Self::ActorDeploy { .. }) {
                    Self::TAG_ACTOR_DEPLOY
                } else {
                    Self::TAG_SET_CODE
                })?
                .store_bytes(&actor.to_hash())?
                .store_ref(CellRef::resident(crate::code_cell(code)?))?;
            }
            Self::Send(message) => {
                b.store_u8(Self::TAG_SEND)?
                    .store_ref(CellRef::resident(message.to_cell()?))?;
            }
            Self::StoragePurchase {
                actor,
                bytes,
                expiry_height,
                fee_sparks,
            } => {
                b.store_u8(Self::TAG_STORAGE_PURCHASE)?
                    .store_bytes(&actor.to_hash())?
                    .store_u64(*bytes)?
                    .store_u64(*expiry_height)?
                    .store(fee_sparks)?;
            }
            Self::ActorDestroy { actor } => {
                b.store_u8(Self::TAG_ACTOR_DESTROY)?
                    .store_bytes(&actor.to_hash())?;
            }
        }
        Ok(())
    }
}

impl CellDecode for TxEntry {
    fn decode<R: CellReader + ?Sized>(s: &mut CellSlice<'_>, r: &mut R) -> Result<Self, CellError> {
        Ok(match s.load_u8()? {
            Self::TAG_HEADER => Self::Header(TxHeader::decode(s, r)?),
            Self::TAG_DATA => Self::Data(read_blob(&s.load_ref()?, r, u32::MAX as usize)?),
            Self::TAG_INPUT => Self::Input(<[u8; 32]>::decode(s, r)?),
            Self::TAG_RECEIVE => Self::Receive(<[u8; 32]>::decode(s, r)?),
            Self::TAG_OUTPUT => {
                let cell = cells::read_cell(r, &s.load_ref()?)?;
                Self::Output(Contract::from_cell(&cell, r)?)
            }
            Self::TAG_ISSUE_PUB => Self::IssuePub(Scalar::decode(s, r)?, Scalar::decode(s, r)?),
            Self::TAG_ISSUE_PRIV => Self::IssuePriv(
                CompressedRistretto(<[u8; 32]>::decode(s, r)?),
                CompressedRistretto(<[u8; 32]>::decode(s, r)?),
            ),
            Self::TAG_RETIRE => Self::Retire(
                CompressedRistretto(<[u8; 32]>::decode(s, r)?),
                CompressedRistretto(<[u8; 32]>::decode(s, r)?),
            ),
            Self::TAG_FEE => Self::Fee(s.load_u64()?),
            Self::TAG_ACTOR_SAVE => {
                let actor = ActorID::Hash(<[u8; 32]>::decode(s, r)?);
                let cell = cells::read_cell(r, &s.load_ref()?)?;
                Self::ActorSave {
                    actor,
                    state: Value::from_cell(&cell, r)?,
                }
            }
            tag @ (Self::TAG_ACTOR_DEPLOY | Self::TAG_SET_CODE) => {
                let actor = ActorID::Hash(<[u8; 32]>::decode(s, r)?);
                let cell = cells::read_cell(r, &s.load_ref()?)?;
                let code = crate::code_from_cell(&cell, r, u32::MAX as usize)?;
                if tag == Self::TAG_ACTOR_DEPLOY {
                    Self::ActorDeploy { actor, code }
                } else {
                    Self::SetCode { actor, code }
                }
            }
            Self::TAG_SEND => {
                let cell = cells::read_cell(r, &s.load_ref()?)?;
                Self::Send(Message::from_cell(&cell, r)?)
            }
            Self::TAG_STORAGE_PURCHASE => Self::StoragePurchase {
                actor: ActorID::Hash(<[u8; 32]>::decode(s, r)?),
                bytes: s.load_u64()?,
                expiry_height: s.load_u64()?,
                fee_sparks: Scalar::decode(s, r)?,
            },
            Self::TAG_ACTOR_DESTROY => Self::ActorDestroy {
                actor: ActorID::Hash(<[u8; 32]>::decode(s, r)?),
            },
            _ => return Err(CellError::InvalidFormat),
        })
    }
}

impl CellEncode for TxLog {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        let cell = log_cell(&self.0)?;
        b.store_bytes(cell.payload())?;
        for reference in cell.refs() {
            b.store_ref(reference.clone())?;
        }
        Ok(())
    }
}

impl CellDecode for TxLog {
    fn decode<R: CellReader + ?Sized>(s: &mut CellSlice<'_>, r: &mut R) -> Result<Self, CellError> {
        let len = usize::try_from(s.load_u64()?).map_err(|_| CellError::LimitExceeded)?;
        let trie = if len == 0 {
            Trie::new(8)?
        } else {
            Trie::from_cell(s.load_ref()?, 8)?
        };
        let mut entries = Vec::new();
        for (i, (key, reference)) in trie.entries_exact(len, r)?.into_iter().enumerate() {
            if key != (i as u64).to_be_bytes() {
                return Err(CellError::InvalidFormat);
            }
            let cell = cells::read_cell(r, &reference)?;
            entries.push(TxEntry::from_cell(&cell, r)?);
        }
        Ok(Self(entries))
    }
}

impl CellEncode for UnsignedTx {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        let body = program_body(
            self.header,
            &Cell::new(self.script.clone(), self.program_root.refs().to_vec())?,
            &self.witnesses,
            &self.log_commitment,
        )?;
        if body.id() != self.txid.0 {
            return Err(CellError::InvalidFormat);
        }
        b.store_bytes(body.payload())?;
        for reference in body.refs() {
            b.store_ref(reference.clone())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;
    use crate::{Anchor, Predicate, PredicateTree, String};
    use std::convert::TryInto;

    fn build(program: ScriptBuilder) -> ExternalTx {
        program
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                Limits { gas: 1_000_000 },
            )
            .unwrap()
            .without_signature()
            .unwrap()
    }

    #[test]
    fn body_id_binds_program_even_when_effects_are_identical() {
        let a = build(ScriptBuilder::new().nop());
        let mut b = build(ScriptBuilder::new().nop().nop());
        assert_eq!(a.effect_id(), b.effect_id());
        assert_ne!(a.txid, b.txid);
        assert_eq!(a.body().unwrap().id(), a.txid.0);
        assert_eq!(a.to_cell().unwrap().id(), a.witness_id().unwrap());
        b.proof = a.proof.clone();
        assert!(matches!(
            b.verify(Limits { gas: 1_000_000 }),
            Err(VMError::InvalidR1CSProof)
        ));
    }

    #[test]
    fn inline_authorization_changes_witness_id_but_not_body_id() {
        let a = build(ScriptBuilder::new().nop());
        let mut b = a.clone();
        b.proof = build(ScriptBuilder::new().nop()).proof;
        assert_eq!(a.txid, b.txid);
        assert_eq!(
            a.body().unwrap().commitment(),
            b.body().unwrap().commitment()
        );
        assert_ne!(a.witness_id().unwrap(), b.witness_id().unwrap());
        b.verify(Limits { gas: 1_000_000 }).unwrap();
        let mut bytes = [0; 64];
        bytes[0] = 1;
        b.signature = Some(Signature::from_bytes(bytes).unwrap());
        assert_eq!(a.txid, b.txid);
        assert_ne!(a.witness_id().unwrap(), b.witness_id().unwrap());
        assert!(matches!(
            b.verify(Limits { gas: 1_000_000 }),
            Err(VMError::SpuriousTxBoundSignature)
        ));
        b.signature = Some(Signature::from_bytes([0; 64]).unwrap());
        assert!(matches!(b.to_cell(), Err(CellError::InvalidFormat)));
    }

    #[test]
    fn log_claim_is_pruned_canonical_and_not_part_of_execution_availability() {
        let tx = build(ScriptBuilder::new().nop());
        let body = tx.body().unwrap();
        let log = body.refs()[1].as_resident().unwrap();
        assert!(log.is_pruned());
        assert_eq!(log.level_mask(), 1);
        assert_eq!(log.record_size(), 36);
        let computed = tx
            .verify(Limits { gas: 1_000_000 })
            .unwrap()
            .to_cell()
            .unwrap();
        assert_eq!(computed.hash(0).unwrap(), log.hash(0).unwrap());
        assert_eq!(computed.depth(0).unwrap(), log.depth(0).unwrap());
        assert!(!tx.witnesses.contains(&log.id()));

        for claim in [
            computed,
            Cell::from_pruned(2, vec![log.hash(0).unwrap()], vec![log.depth(0).unwrap()]).unwrap(),
        ] {
            let forged_body = Cell::new(
                body.payload().to_vec(),
                vec![body.refs()[0].clone(), claim.into()],
            )
            .unwrap();
            let root = tx.to_cell().unwrap();
            let forged = Cell::new(root.payload().to_vec(), vec![forged_body.into()]).unwrap();
            assert!(matches!(
                ExternalTx::from_cell(&forged, &mut ()),
                Err(CellError::InvalidFormat)
            ));
        }
        let mut forged = tx.clone();
        forged.log_commitment =
            Cell::from_pruned(1, vec![[7; 32]], vec![log.depth(0).unwrap()]).unwrap();
        forged.txid = TxID(
            body_cell(
                forged.header,
                &forged.script,
                &forged.witnesses,
                &forged.log_commitment,
            )
            .unwrap()
            .id(),
        );
        assert!(matches!(
            forged.verify(Limits { gas: 1_000_000 }),
            Err(VMError::Cell(CellError::InvalidFormat))
        ));
    }

    #[test]
    fn log_level_zero_is_checked_even_when_outputs_contain_pruning() {
        let plain = crate::Dict::from_values(vec![Value::Scalar(Scalar::ONE)])
            .to_cell()
            .unwrap();
        let trie = cells::read_cell(&mut (), &plain.refs()[0]).unwrap();
        let dict = Cell::new(
            plain.payload().to_vec(),
            vec![trie.prune(3).unwrap().into()],
        )
        .unwrap();
        let dict = crate::Dict::from_trusted_cell(&dict, &mut ()).unwrap();
        let tree = PredicateTree::from_scripts(
            None,
            vec![ScriptBuilder::new().push_int(1u64).return_()],
            [7; 32],
        )
        .unwrap();
        let contract = Contract::new(
            Predicate::tree(tree.clone()),
            Anchor([8; 32]),
            Value::Dict(dict),
        )
        .unwrap();
        let tx = build(
            ScriptBuilder::new()
                .push_str(String::contract(contract))
                .input()
                .push_taproot_proof(&tree, 0)
                .unwrap()
                .push_int(100_000u64)
                .push_int(0u64)
                .open()
                .verify()
                .drop_()
                .push_point(*Predicate::unspendable_key().as_bytes())
                .output(),
        );
        let computed = tx
            .verify(Limits { gas: 1_000_000 })
            .unwrap()
            .to_cell()
            .unwrap();
        assert_eq!(computed.level(), 3);
        assert_ne!(computed.id(), computed.hash(0).unwrap());
        assert_eq!(tx.log_commitment.level_mask(), 1);
        assert_eq!(tx.effect_id().0, computed.hash(0).unwrap());
        let bytes = tx.to_bytes().unwrap();
        ExternalTx::from_bytes_bounded(&bytes, 1, tx.script.len(), tx.proof_bytes().len())
            .unwrap()
            .verify(Limits { gas: 1_000_000 })
            .unwrap();
    }

    #[test]
    fn inline_proof_length_is_bounded_and_consumed_exactly() {
        let tx = build(ScriptBuilder::new().nop());
        let root = tx.to_cell().unwrap();
        assert_eq!(&root.payload()[..64], &[0; 64]);
        assert_eq!(
            u16::from_le_bytes(root.payload()[64..66].try_into().unwrap()) as usize,
            tx.proof_bytes().len()
        );
        assert_eq!(&root.payload()[66..], tx.proof_bytes());
        let mut payload = root.payload().to_vec();
        payload[64..66].copy_from_slice(&u16::MAX.to_le_bytes());
        let forged = Cell::new(payload, root.refs().to_vec()).unwrap();
        assert!(matches!(
            ExternalTx::from_cell_bounded(&forged, &mut (), 1, 1, 2497),
            Err(CellError::LimitExceeded)
        ));
        let mut payload = root.payload().to_vec();
        payload.push(0);
        let forged = Cell::new(payload, root.refs().to_vec()).unwrap();
        let bytes = forged.encode().unwrap();
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 1, 1, 2497),
            Err(CellError::TrailingBytes)
        ));
    }

    #[test]
    fn execution_checks_effect_root_depth_not_only_its_hash() {
        let limits = Limits { gas: 100_000 };
        let tx = ScriptBuilder::new()
            .nop()
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap()
            .without_signature()
            .unwrap();
        let mut forged = tx.clone();
        forged.log_commitment = Cell::from_pruned(
            1,
            vec![tx.log_commitment.hash(0).unwrap()],
            vec![tx.log_commitment.depth(0).unwrap() + 1],
        )
        .unwrap();
        forged.txid = TxID(
            body_cell(
                forged.header,
                &forged.script,
                &forged.witnesses,
                &forged.log_commitment,
            )
            .unwrap()
            .id(),
        );
        let bytes = forged.to_bytes().unwrap();
        let forged =
            ExternalTx::from_bytes_bounded(&bytes, 1, tx.script.len(), tx.proof_bytes().len())
                .unwrap();
        assert_eq!(forged.effect_id(), tx.effect_id());
        assert_ne!(forged.txid, tx.txid);
        assert!(matches!(
            forged.verify(limits),
            Err(VMError::Cell(CellError::InvalidFormat))
        ));
    }

    #[test]
    fn embedded_nested_witnesses_roundtrip_from_prover_to_verifier() {
        let leaf = ScriptBuilder::new()
            .drop_()
            .alloc(Some(Scalar::from(7u64)))
            .alloc(Some(Scalar::from(3u64)))
            .add()
            .alloc(Some(Scalar::from(10u64)))
            .eq()
            .verify();
        let inner = PredicateTree::from_scripts(None, vec![leaf], [1; 32]).unwrap();
        let inner_id = inner.root_id();
        let inner_contract = Contract::new(
            Predicate::tree(inner.clone()),
            Anchor([2; 32]),
            Value::Scalar(Scalar::ONE),
        )
        .unwrap();
        let inner_contract_id = inner_contract.id();
        let branch = ScriptBuilder::new()
            .drop_()
            .push_str(String::contract(inner_contract))
            .input()
            .push_taproot_proof(&inner, 0)
            .unwrap()
            .push_int(100_000u64)
            .push_int(0u64)
            .open()
            .verify()
            .drop_();
        let hidden_contract = Contract::new(
            Predicate::opaque(Predicate::unspendable_key()),
            Anchor([3; 32]),
            Value::Scalar(Scalar::ONE),
        )
        .unwrap();
        let hidden_id = hidden_contract.id();
        let unused = ScriptBuilder::new()
            .drop_()
            .push_str(String::contract(hidden_contract))
            .drop_();
        let outer = PredicateTree::from_scripts(None, vec![branch, unused], [4; 32]).unwrap();
        let contract = Contract::new(
            Predicate::tree(outer.clone()),
            Anchor([5; 32]),
            Value::Scalar(Scalar::ONE),
        )
        .unwrap();
        let contract_id = contract.id();
        let limits = Limits { gas: 1_000_000 };
        let program = ScriptBuilder::new()
            .push_str(String::contract(contract))
            .input()
            .push_taproot_proof(&outer, 0)
            .unwrap()
            .push_int(400_000u64)
            .push_int(0u64)
            .open()
            .verify()
            .drop_();
        // No manual `with_cells`, private script overlay, or verifier sidecar.
        let unsigned = program
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap();
        for id in [contract_id, inner_contract_id, outer.root_id(), inner_id] {
            assert!(unsigned.witnesses().contains(&id));
        }
        assert!(
            !unsigned.witnesses().contains(&hidden_id),
            "unused program witnesses stay private"
        );
        let expected_id = unsigned.txid();
        let expected_metrics = unsigned.metrics();
        let expected_bag = unsigned.witnesses().id();
        let tx = unsigned.without_signature().unwrap();
        let bytes = tx.to_bytes().unwrap();
        let decoded =
            ExternalTx::from_bytes_bounded(&bytes, 1, tx.script.len(), tx.proof_bytes().len())
                .unwrap();
        assert_eq!(decoded.witnesses().id(), expected_bag);
        let (log, metrics) = decoded.verify_with_metrics(limits).unwrap();
        assert_eq!(decoded.txid, expected_id);
        assert_eq!(log.effect_id(), decoded.effect_id());
        assert_eq!(metrics, expected_metrics);
        assert_eq!(
            log.iter()
                .filter(|entry| matches!(entry, TxEntry::Input(_)))
                .count(),
            2
        );

        let mut missing = decoded;
        let mut bag = CellIndex::new();
        for (id, cell) in missing.witnesses().iter() {
            if *id != outer.root_id() {
                bag.insert(Arc::clone(cell)).unwrap();
            }
        }
        missing.witnesses = Arc::new(bag);
        assert!(matches!(
            missing.verify(limits),
            Err(VMError::Cell(CellError::InvalidFormat))
        ));
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => panic!("invalid test vector"),
                };
                digit(pair[0]) << 4 | digit(pair[1])
            })
            .collect()
    }

    #[test]
    fn canonical_external_tx_vector_and_bounds() {
        // Fixed R1CS bytes keep this transport/shape vector independent of prover randomness.
        const PROOF: &str = "007e5de4349c5b87f2e1003095aff2e310801e2504b706bc6c062076eee49f90366625b75748908fb2492dd909a6d1428001dfdd201a0a7fae70911cf29112c8319e9d0eba4ca7fe137d5f8026614ab8736204ea46c213d9a20d0d663aa3e8ff1676fcd93dc1cba92d2f820b5b8ae5c99bacce0610dc799f050d1dec5effd5cb6c96950b0ad392e7414252008e6ff97d385437f30c74f106ae586522db4a9d73241ca0ed4f24798b31981e98e96bc121852a567728380ca00d12ee8556c220c13c3ed16d35fca58a3a3773120657b5b49cac1830a472bd083c51f4012ab7de25450a4544cdee6b7577d97a9c3e5a3267da4e13e2ef36a65ce83697cc498f00d005000000000000000000000000000000000000000000000000000000000000000027e4219ec9efc32f50b4b1c8766037a812d135363cbaa38be71527de967eb20839057e9d2324d2932cba8c6a646bb2b9f09661cd1ef8977bbd1df4813803e4040000000000000000000000000000000000000000000000000000000000000000ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010";
        let log_commitment = Cell::from_pruned(1, vec![[0x11; 32]], vec![0]).unwrap();
        let mut tx = ExternalTx {
            header: TxHeader {
                version: 1,
                locktime: 2,
            },
            script: vec![0x42],
            signature: None,
            proof: R1CSProof::from_bytes(&hex_bytes(PROOF)).unwrap(),
            witnesses: Arc::new(CellIndex::new()),
            txid: TxID([0; 32]),
            log_commitment,
            program_root: Cell::new(vec![0x42], vec![]).unwrap(),
        };
        tx.txid = TxID(
            body_cell(tx.header, &tx.script, &tx.witnesses, &tx.log_commitment)
                .unwrap()
                .id(),
        );
        let root = tx.to_cell().unwrap();
        let mut expected_payload = vec![0; 64];
        expected_payload.extend_from_slice(&417u16.to_le_bytes());
        expected_payload.extend_from_slice(&hex_bytes(PROOF));
        assert_eq!(root.payload(), expected_payload);
        assert_eq!(root.refs().len(), 1);
        assert_eq!(root.refs()[0].id(), tx.txid.0);
        assert_eq!(tx.body().unwrap().payload(), [1, 0, 0, 0, 2, 0, 0, 0]);
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<std::string::String>()
        };
        assert_eq!(
            hex(&tx.txid.0),
            "a68e7ba79558ee8405ebe8dfbe6fb86880fd28d9a4e70abe5d02ce997755e15e"
        );
        assert_eq!(
            hex(&root.id()),
            "4aa568dabfb40a5ad6251fbbc67bd021cee5574f4c96553ca60f0e74b0db8ae1"
        );
        let bytes = tx.to_bytes().unwrap();
        let decoded = ExternalTx::from_bytes_bounded(&bytes, 1, 1, 417).unwrap();
        assert_eq!(decoded.to_bytes().unwrap(), bytes);
        assert_eq!(decoded.encoded_size(), bytes.len());

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(ExternalTx::from_bytes_bounded(&trailing, 1, 1, 417).is_err());
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 2, 1, 417),
            Err(CellError::InvalidFormat)
        ));
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 1, 0, 417),
            Err(CellError::LimitExceeded)
        ));
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 1, 1, 416),
            Err(CellError::LimitExceeded)
        ));

        let mut unknown_version = tx;
        let original_txid = unknown_version.txid;
        unknown_version.txid.0[0] ^= 1;
        assert!(matches!(
            unknown_version.to_cell(),
            Err(CellError::InvalidFormat)
        ));
        unknown_version.txid = original_txid;
        unknown_version.header.version = 2;
        unknown_version.txid = TxID(
            body_cell(
                unknown_version.header,
                &unknown_version.script,
                &unknown_version.witnesses,
                &unknown_version.log_commitment,
            )
            .unwrap()
            .id(),
        );
        let bytes = unknown_version.to_bytes().unwrap();
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 2, 1, 417),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn program_subcells_are_committed_and_transport_extras_are_rejected() {
        let limits = Limits { gas: 100_000 };
        let mut witnesses = CellIndex::new();
        witnesses
            .insert(Arc::new(Cell::new(vec![9], vec![]).unwrap()))
            .unwrap();
        let mut tx = ScriptBuilder::new()
            .with_cells(witnesses)
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap()
            .without_signature()
            .unwrap();
        let log = tx.verify(limits).unwrap();
        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.effect_id(), tx.effect_id());
        let bytes = tx.to_bytes().unwrap();
        ExternalTx::from_bytes_bounded(&bytes, 1, 0, tx.proof_bytes().len())
            .unwrap()
            .verify(limits)
            .unwrap();

        let root = tx.to_cell().unwrap();
        let mut references = root.refs().to_vec();
        references.push(Cell::new(vec![99], vec![]).unwrap().into());
        let extra = Cell::new(root.payload().to_vec(), references)
            .unwrap()
            .encode()
            .unwrap();
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&extra, 1, 0, tx.proof_bytes().len()),
            Err(CellError::TrailingReferences)
        ));

        tx.witnesses = Arc::new(CellIndex::new());
        assert!(
            tx.verify(limits).is_err(),
            "stripping an unused witness must still invalidate the proof/TxID"
        );
        tx.txid = TxID(
            body_cell(tx.header, &tx.script, &tx.witnesses, &tx.log_commitment)
                .unwrap()
                .id(),
        );
        assert!(
            matches!(tx.verify(limits), Err(VMError::InvalidR1CSProof)),
            "rehashing the modified body must not make the original proof reusable"
        );
    }

    #[test]
    fn txlog_trie_roundtrip_and_wrong_count() {
        let log = TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Data(vec![42; 20_000]),
            TxEntry::Fee(17),
        ]);
        let cell = log.to_cell().unwrap();
        assert_eq!(cell.hash(0).unwrap(), log.effect_id().0);
        let mut envelope = log.to_envelope().unwrap();
        let root = envelope.cells().get(&envelope.root()).unwrap();
        let decoded = TxLog::from_cell(&root, &mut envelope).unwrap();
        assert_eq!(decoded.effect_id(), log.effect_id());
        assert!(
            matches!(&decoded.entries()[1], TxEntry::Data(bytes) if bytes == &vec![42; 20_000])
        );
        let malformed = Cell::new(5u64.to_le_bytes().to_vec(), cell.refs().to_vec()).unwrap();
        assert!(TxLog::from_cell(&malformed, &mut ()).is_err());
    }

    #[test]
    fn transaction_without_txbound_authorization_omits_signature() {
        let limits = Limits { gas: 10_000 };
        let tx = ScriptBuilder::new()
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap()
            .without_signature()
            .unwrap();
        assert!(tx.signature_bytes().is_none());
        let (_, metrics) = tx.verify_with_metrics(limits).unwrap();
        assert!(metrics.gas_used > 0);
        assert_eq!(metrics.multiplications, 0);
    }

    #[test]
    fn multiplication_metrics_include_randomized_constraints() {
        let limits = Limits { gas: 100_000 };
        let unsigned = ScriptBuilder::new()
            .alloc(Some(Scalar::ONE))
            .alloc(Some(Scalar::from(2u64)))
            .eq()
            .not()
            .verify()
            .build_tx(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                limits,
            )
            .unwrap();
        assert_eq!(unsigned.metrics().multiplications, 3);
        let tx = unsigned.without_signature().unwrap();
        assert_eq!(tx.verify_with_metrics(limits).unwrap().1.multiplications, 3);
    }
}
