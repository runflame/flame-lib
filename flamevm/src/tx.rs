use bulletproofs::r1cs::R1CSProof;
use bulletproofs::PedersenGens;
use cells::{
    Cell, CellBuilder, CellDecode, CellEncode, CellEnvelope, CellError, CellIndex, CellRef,
    CellResolver, CellSlice, Trie,
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
    /// Claimed effect root, checked against execution; not the envelope root.
    pub txid: TxID,
    /// Full unloaded commitment of the claimed effect root. Hash alone cannot
    /// encode a child reference because parent hashing also commits to depth.
    effect_root: CellRef,
}

impl CellEncode for TxHeader {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        b.store_u32(self.version)?.store_u32(self.locktime)?;
        Ok(())
    }
}
impl CellDecode for TxHeader {
    fn decode<R: CellResolver + ?Sized>(
        s: &mut CellSlice<'_>,
        _r: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self {
            version: s.load_u32()?,
            locktime: s.load_u32()?,
        })
    }
}

/// Script bytes and the committed witness Cell hierarchy share a container.
fn transaction_script(script: &[u8], witnesses: &CellIndex) -> Result<Cell, CellError> {
    let mut b = CellBuilder::new();
    b.store_snake(script)?;
    b.store_ref(CellRef::resident(witnesses.to_cell()?))?;
    Ok(b.build())
}

impl ExternalTx {
    pub fn header(&self) -> TxHeader {
        self.header
    }
    pub fn script(&self) -> &[u8] {
        &self.script
    }
    pub fn witnesses(&self) -> &CellIndex {
        &self.witnesses
    }
    pub fn signature_bytes(&self) -> Option<[u8; 64]> {
        self.signature.map(|s| s.to_bytes())
    }
    pub fn proof_bytes(&self) -> Vec<u8> {
        self.proof.to_bytes()
    }

    pub fn from_bytes_bounded(
        bytes: &[u8],
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, CellError> {
        // Outer network admission bounds bytes before this function. Decode work
        // is additionally linear-bounded here, including the nested witness bag.
        let mut gas = (bytes.len() as u64)
            .saturating_mul(1024)
            .saturating_add(1024);
        let mut envelope = CellEnvelope::decode(bytes, bytes.len(), &mut gas)?;
        let root = envelope
            .cells()
            .get(&envelope.root())
            .ok_or(CellError::InvalidFormat)?;
        let mut slice = CellSlice::new(&root);
        let tx = Self::decode_bounded(
            &mut slice,
            &mut envelope,
            expected_version,
            max_script_bytes,
            max_proof_bytes,
        )?;
        slice.finish()?;
        // Extra transport bodies must not create a second encoding of this tx.
        // Extras inside the execution Cell hierarchy are allowed: their presence is signed.
        let canonical = tx.to_envelope()?;
        if canonical.root() != envelope.root() || canonical.cells().id() != envelope.cells().id() {
            return Err(CellError::InvalidFormat);
        }
        Ok(tx)
    }

    pub fn verify(&self, limits: Limits) -> Result<TxLog, VMError> {
        self.verify_with_metrics(limits).map(|(log, _)| log)
    }
    pub fn verify_with_metrics(&self, limits: Limits) -> Result<(TxLog, TxMetrics), VMError> {
        let result = Verifier::verify_with_cells(
            &PedersenGens::default(),
            self.script.clone(),
            &self.proof,
            self.header,
            limits.gas,
            self.signature,
            &self.witnesses,
        )?;
        let effect_root = log_cell(&result.txlog)?;
        if result.txid != self.txid || effect_root.commitment() != self.effect_root.commitment()? {
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
        self.to_envelope()
            .expect("constructed transaction is encodable")
            .encode()
            .len()
    }
}

impl CellEncode for ExternalTx {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        b.store(&self.header)?.store_bytes(&self.witnesses.id()?)?;
        b.store_ref(CellRef::resident(transaction_script(
            &self.script,
            &self.witnesses,
        )?))?;
        let signature = self
            .signature_bytes()
            .map(|s| s.to_vec())
            .unwrap_or_default();
        b.store_ref(CellRef::resident(Cell::new(signature, vec![])?))?;
        b.store_ref(CellRef::resident(blob_cell(&self.proof.to_bytes())?))?;
        if self.effect_root.id() != self.txid.0 {
            return Err(CellError::InvalidFormat);
        }
        b.store_ref(self.effect_root.clone())?;
        Ok(())
    }
}

impl ExternalTx {
    /// Reads one transaction root with limits checked before snake allocation.
    /// The surrounding transport owns the total byte bound and canonical Cell hierarchy.
    pub fn from_cell_bounded<R: CellResolver + ?Sized>(
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

    fn decode_bounded<R: CellResolver + ?Sized>(
        s: &mut CellSlice<'_>,
        r: &mut R,
        expected_version: u32,
        max_script_bytes: usize,
        max_proof_bytes: usize,
    ) -> Result<Self, CellError> {
        let header = TxHeader::decode(s, r)?;
        if expected_version != 1 || header.version != expected_version {
            return Err(CellError::InvalidFormat);
        }
        let witness_id = <[u8; 32]>::decode(s, r)?;
        let script_cell = cells::resolve_cell(r, &s.load_ref()?)?;
        let mut script_slice = CellSlice::new(&script_cell);
        let script = script_slice.load_snake(r, max_script_bytes)?;
        let witness_root = cells::resolve_cell(r, &script_slice.load_ref()?)?;
        script_slice.finish()?;
        let witnesses = CellIndex::from_cell(&witness_root, r)?;
        if witnesses.id()? != witness_id {
            return Err(CellError::InvalidFormat);
        }
        let signature_cell = cells::resolve_cell(r, &s.load_ref()?)?;
        if !signature_cell.refs().is_empty() {
            return Err(CellError::InvalidFormat);
        }
        let signature = match signature_cell.payload() {
            [] => None,
            bytes if bytes.len() == 64 => {
                Some(Signature::from_bytes(bytes).map_err(|_| CellError::InvalidFormat)?)
            }
            _ => return Err(CellError::InvalidFormat),
        };
        let proof = R1CSProof::from_bytes(&read_blob(&s.load_ref()?, r, max_proof_bytes)?)
            .map_err(|_| CellError::InvalidFormat)?;
        let effect_root = s.load_ref()?.to_unloaded()?;
        let txid = TxID(effect_root.id());
        Ok(Self {
            header,
            script,
            signature,
            proof,
            witnesses: Arc::new(witnesses),
            txid,
            effect_root,
        })
    }
}

impl CellDecode for ExternalTx {
    fn decode<R: CellResolver + ?Sized>(
        s: &mut CellSlice<'_>,
        r: &mut R,
    ) -> Result<Self, CellError> {
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
/// applies to its state. The [`TxID`] is the Cell ID of this ordered Trie.
pub struct TxLog(Vec<TxEntry>);

/// For the node layer (and tests): wrap a re-derived effect list.
/// The crate itself only ever produces TxLogs by execution.
impl From<Vec<TxEntry>> for TxLog {
    fn from(entries: Vec<TxEntry>) -> Self {
        TxLog(entries)
    }
}

impl TxLog {
    /// Canonical transaction id (Cell ID of the ordered effect Trie envelope).
    pub fn txid(&self) -> TxID {
        TxID::from_log(&self.0)
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
    /// The keys + txid the sender signs over (lifecycle step 2 input).
    pub fn signing_instructions(&self) -> SigningInstructions {
        SigningInstructions {
            txid: self.log.txid(),
            items: self.txbound_items.clone(),
        }
    }
    /// Attaches the aggregate signature → broadcastable [`ExternalTx`].
    pub fn sign(self, signature: Signature) -> ExternalTx {
        let effect_root = CellRef::resident(
            self.log
                .to_cell()
                .expect("admitted effects have Cell encodings"),
        )
        .to_unloaded()
        .expect("resident effect root has a commitment");
        ExternalTx {
            header: self.header,
            script: self.script,
            signature: Some(signature),
            proof: self.proof,
            txid: TxID(effect_root.id()),
            effect_root,
            witnesses: self.witnesses,
        }
    }

    /// Finalizes a transaction that recorded no `signtx` authorizations.
    pub fn without_signature(self) -> Result<ExternalTx, VMError> {
        if !self.txbound_items.is_empty() {
            return Err(VMError::MissingTxBoundSignature);
        }
        let effect_root = CellRef::resident(self.log.to_cell()?).to_unloaded()?;
        Ok(ExternalTx {
            header: self.header,
            script: self.script,
            signature: None,
            proof: self.proof,
            txid: TxID(effect_root.id()),
            effect_root,
            witnesses: self.witnesses,
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
            header,
            script: result.bytecode,
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

/// Transaction ID is a unique 32-byte identifier of a transaction effects represented by `TxLog`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxID(pub [u8; 32]);

/// Entry in a transaction log. All entries are hashed into a [transaction ID](TxID).
///
/// `Clone`/`Serialize`/`Deserialize` are still withheld (the linear
/// `Contract`/`Token` payloads don't participate); downstream code wanting
/// those should hash entries to bytes first.
#[derive(Debug)]
pub enum TxEntry {
    /// Tx header — bound at run start as the first txlog entry so
    /// `version` and `locktime` participate in `TxID::from_log`.
    Header(TxHeader),

    /// Exact execution-body availability, fixed before proving or signing.
    CellWitness(cells::CellID),

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
    /// actor hash and a reference to snake-encoded code. Symmetric with
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
    pub const TAG_CELL_WITNESS: u8 = 15;
}

impl CellEncode for TxEntry {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        match self {
            Self::Header(h) => {
                b.store_u8(Self::TAG_HEADER)?.store(h)?;
            }
            Self::CellWitness(id) => {
                b.store_u8(Self::TAG_CELL_WITNESS)?.store_bytes(id)?;
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
                .store_ref(CellRef::resident(blob_cell(code)?))?;
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
    fn decode<R: CellResolver + ?Sized>(
        s: &mut CellSlice<'_>,
        r: &mut R,
    ) -> Result<Self, CellError> {
        Ok(match s.load_u8()? {
            Self::TAG_HEADER => Self::Header(TxHeader::decode(s, r)?),
            Self::TAG_CELL_WITNESS => Self::CellWitness(<[u8; 32]>::decode(s, r)?),
            Self::TAG_DATA => Self::Data(read_blob(&s.load_ref()?, r, u32::MAX as usize)?),
            Self::TAG_INPUT => Self::Input(<[u8; 32]>::decode(s, r)?),
            Self::TAG_RECEIVE => Self::Receive(<[u8; 32]>::decode(s, r)?),
            Self::TAG_OUTPUT => {
                let cell = cells::resolve_cell(r, &s.load_ref()?)?;
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
                let cell = cells::resolve_cell(r, &s.load_ref()?)?;
                Self::ActorSave {
                    actor,
                    state: Value::from_cell(&cell, r)?,
                }
            }
            tag @ (Self::TAG_ACTOR_DEPLOY | Self::TAG_SET_CODE) => {
                let actor = ActorID::Hash(<[u8; 32]>::decode(s, r)?);
                let code = read_blob(&s.load_ref()?, r, u32::MAX as usize)?;
                if tag == Self::TAG_ACTOR_DEPLOY {
                    Self::ActorDeploy { actor, code }
                } else {
                    Self::SetCode { actor, code }
                }
            }
            Self::TAG_SEND => {
                let cell = cells::resolve_cell(r, &s.load_ref()?)?;
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
    fn decode<R: CellResolver + ?Sized>(
        s: &mut CellSlice<'_>,
        r: &mut R,
    ) -> Result<Self, CellError> {
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
            let cell = cells::resolve_cell(r, &reference)?;
            entries.push(TxEntry::from_cell(&cell, r)?);
        }
        Ok(Self(entries))
    }
}

impl CellEncode for UnsignedTx {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        b.store(&self.header)?.store_bytes(&self.witnesses.id()?)?;
        b.store_ref(CellRef::resident(transaction_script(
            &self.script,
            &self.witnesses,
        )?))?;
        b.store_ref(CellRef::resident(self.log.to_cell()?).to_unloaded()?)?;
        Ok(())
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;
    use crate::{Anchor, Predicate, PredicateTree, String};

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
        let mut envelope = tx.to_envelope().unwrap();
        let mut bytes = tx.to_cell().unwrap().encode_record();
        // The last child is the level-zero effect root; its final two bytes
        // contain depth. Keep the claimed TxID but falsify that commitment.
        let end = bytes.len();
        let depth = u16::from_le_bytes([bytes[end - 2], bytes[end - 1]]);
        bytes[end - 2..].copy_from_slice(&(depth + 1).to_le_bytes());
        let root = Cell::decode_record_exact(&bytes).unwrap();
        let forged = ExternalTx::from_cell(&root, &mut envelope).unwrap();
        assert_eq!(forged.txid, tx.txid);
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
        let expected_id = unsigned.log().txid();
        let expected_metrics = unsigned.metrics();
        let expected_bag = unsigned.witnesses().id();
        let tx = unsigned.without_signature().unwrap();
        let bytes = tx.to_envelope().unwrap().encode();
        let decoded =
            ExternalTx::from_bytes_bounded(&bytes, 1, tx.script.len(), tx.proof_bytes().len())
                .unwrap();
        assert_eq!(decoded.witnesses().id(), expected_bag);
        let (log, metrics) = decoded.verify_with_metrics(limits).unwrap();
        assert_eq!(log.txid(), expected_id);
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
        assert!(
            matches!(missing.verify(limits), Err(VMError::Cell(CellError::MissingCell(id))) if id == outer.root_id())
        );
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
        let effect_root = CellRef::resident(Cell::new(vec![0x11], vec![]).unwrap())
            .to_unloaded()
            .unwrap();
        let tx = ExternalTx {
            header: TxHeader {
                version: 1,
                locktime: 2,
            },
            script: vec![0x42],
            signature: None,
            proof: R1CSProof::from_bytes(&hex_bytes(PROOF)).unwrap(),
            witnesses: Arc::new(CellIndex::new()),
            txid: TxID(effect_root.id()),
            effect_root,
        };
        let root = tx.to_cell().unwrap();
        let mut expected_header = vec![1, 0, 0, 0, 2, 0, 0, 0];
        expected_header.extend_from_slice(&tx.witnesses.id().unwrap());
        assert_eq!(root.payload(), expected_header);
        assert_eq!(root.refs().len(), 4);
        assert_eq!(root.refs()[3].id(), tx.txid.0);
        let bytes = tx.to_envelope().unwrap().encode();
        let decoded = ExternalTx::from_bytes_bounded(&bytes, 1, 1, 417).unwrap();
        assert_eq!(decoded.to_envelope().unwrap().encode(), bytes);
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
        let bytes = unknown_version.to_envelope().unwrap().encode();
        assert!(matches!(
            ExternalTx::from_bytes_bounded(&bytes, 2, 1, 417),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn execution_bag_is_committed_and_transport_extras_are_rejected() {
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
        assert!(
            matches!(log.entries()[1], TxEntry::CellWitness(id) if id == tx.witnesses.id().unwrap())
        );
        let bytes = tx.to_envelope().unwrap().encode();
        ExternalTx::from_bytes_bounded(&bytes, 1, 0, tx.proof_bytes().len())
            .unwrap()
            .verify(limits)
            .unwrap();

        let (root, mut transport) = tx.to_envelope().unwrap().into_parts();
        transport
            .insert(Arc::new(Cell::new(vec![99], vec![]).unwrap()))
            .unwrap();
        assert!(matches!(
            CellEnvelope::new(root, transport),
            Err(CellError::InvalidFormat)
        ));

        tx.witnesses = Arc::new(CellIndex::new());
        assert!(
            tx.verify(limits).is_err(),
            "stripping an unused witness must still invalidate the proof/TxID"
        );
    }

    #[test]
    fn txlog_trie_roundtrip_and_wrong_count() {
        let log = TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::CellWitness(CellIndex::new().id().unwrap()),
            TxEntry::Data(vec![42; 20_000]),
            TxEntry::Fee(17),
        ]);
        let cell = log.to_cell().unwrap();
        assert_eq!(cell.id(), log.txid().0);
        let mut envelope = log.to_envelope().unwrap();
        let root = envelope.cells().get(&envelope.root()).unwrap();
        let decoded = TxLog::from_cell(&root, &mut envelope).unwrap();
        assert_eq!(decoded.txid(), log.txid());
        assert!(
            matches!(&decoded.entries()[2], TxEntry::Data(bytes) if bytes == &vec![42; 20_000])
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
