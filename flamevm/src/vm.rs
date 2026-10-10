//! FlameVM execution engine: Tx → CallFrame + dispatch loop.

use bulletproofs::r1cs;
use bulletproofs::r1cs::R1CSProof;
#[cfg(test)]
use cells::{read_cell, CellRef};
use cells::{
    Cell, CellBuilder, CellDecode, CellEncode, CellError, CellID, CellIndex, CellReader, CellSlice,
};
use core::convert::TryFrom;
use core::mem;
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use merlin::Transcript;
use std::{collections::BTreeMap, sync::Arc};

use crate::actor::{empty_state, ActorID, ActorRegistry};
use crate::constraints::Commitment;
use crate::contract::{Contract, ContractID, Predicate, TaprootProof};
use crate::errors::VMError;
use crate::fees::CheckedFee;
use crate::message::Message;
use crate::ops::Instruction;
use crate::script::{Program, Script, ScriptBuilder};
use crate::string::array32;
use crate::token::{flavor_from_actor, flavor_from_predicate, FLAME_FLAVOR};
use crate::tx::TxHeader;
use crate::tx::{TxEntry, TxID};
use crate::{
    ClearToken, Constraint, Dict, Expression, Merlin, Point, Scalar, String, Token, Value,
    Variable, WideToken,
};

/// Bitcoin BIP-65 convention threshold for distinguishing
/// `TxHeader::locktime` as a block height vs. a Unix timestamp:
/// values `< 500_000_000` are block heights, `≥` are timestamps
/// (~1985-11-05 epoch). Surfaced to scripts via the `timelock`
/// opcode's `{0|1}` flag.
pub const LOCKTIME_TIMESTAMP_THRESHOLD: u32 = 500_000_000;

/// 32-byte anchor. Chained via `ratchet` to make outputs unique
/// within a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor(pub [u8; 32]);

impl Anchor {
    /// Deterministically splits this anchor into two distinct
    /// children. Used by every consume site that needs a unique
    /// entity-anchor: the `left` half is embedded in the new entity
    /// (contract / message / callee frame), the `right` half replaces
    /// the consumer's current anchor.
    ///
    /// Uniqueness inherits from the parent: if `self` came from a
    /// spend-once source (an input contract's id) and from a chain of
    /// prior splits over that source, both children are unique
    /// within the network.
    pub fn split(&self) -> (Anchor, Anchor) {
        let mut t = Transcript::new(b"flamevm.anchor.split");
        t.append_message(b"parent", &self.0);
        let mut left = [0u8; 32];
        let mut right = [0u8; 32];
        t.challenge_bytes(b"left", &mut left);
        t.challenge_bytes(b"right", &mut right);
        (Anchor(left), Anchor(right))
    }
}

/// 32 raw bytes — fixed size, no tag, no length prefix.
impl CellEncode for Anchor {
    fn encode(&self, w: &mut CellBuilder) -> Result<(), CellError> {
        w.store_bytes(&self.0)?;
        Ok(())
    }
}

impl CellDecode for Anchor {
    fn decode<R: CellReader + ?Sized>(
        r: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Anchor(<[u8; 32]>::decode(r, cells)?))
    }
}

// `Predicate` lives in `contract::predicate`; re-exported via `Predicate`.

/// External signature check deferred to transaction finalization. `TxBound`
/// comes from external-only `signtx`; `Explicit` comes from external
/// `signcall`. Internal `signcall` signatures are checked immediately and are
/// never recorded here.
#[derive(Clone, Debug)]
pub enum DeferredSig {
    TxBound {
        verification_key: CompressedRistretto,
        contract_id: ContractID,
    },
    Explicit {
        verification_key: CompressedRistretto,
        message: Vec<u8>,
        signature: [u8; 64],
    },
}

/// Consensus transcript for a `signcall` signature after the program bytes
/// have been reduced to `message`. Shared by immediate internal verification
/// and deferred external batch verification.
pub(crate) fn signcall_verification_transcript(message: &[u8]) -> Transcript {
    let mut transcript = Transcript::new(b"flamevm.signcall");
    transcript.append_message(b"msg", message);
    transcript
}

/// Block-level immutable context (height, chain stats).
pub struct BlockContext {
    pub height: u64,
}

/// External-context user-task abstraction: prover or verifier. Owns the
/// R1CS constraint system and finalizes proofs and signatures.
pub trait Delegate {
    /// The R1CS constraint system. The
    /// [`r1cs::CheckpointableConstraintSystem`] bound lets the VM
    /// snapshot the CS on call entry and roll back the witness +
    /// constraint vectors + transcript on call failure — so a
    /// failed nested call's CS effects can't pollute the caller's
    /// proof.
    type CS: r1cs::RandomizableConstraintSystem + r1cs::CheckpointableConstraintSystem;
    /// Per-side batched scalar-point check accumulator. The
    /// [`musig::BatchCheckpoint`] bound lets the VM snapshot the
    /// accumulator on call entry and restore it on call failure —
    /// so a failed nested call's MSM contributions can't pollute
    /// the caller's proof.
    type BatchVerifier: musig::BatchVerification + musig::BatchCheckpoint;

    fn cs(&mut self) -> &mut Self::CS;

    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier;

    /// Adds a Commitment to the CS, producing a high-level variable.
    fn commit_variable(
        &mut self,
        commitment: &Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError>;
}

/// No-op [`Delegate`] used by internal-context steps. CS opcodes
/// route through `require_external()` and never reach these methods.
struct InternalDelegate;

impl Delegate for InternalDelegate {
    type CS = r1cs::Verifier<merlin::Transcript>;
    type BatchVerifier = musig::BatchVerifier<rand::rngs::ThreadRng>;

    fn cs(&mut self) -> &mut Self::CS {
        unreachable!("InternalDelegate::cs — CS opcodes are external-only")
    }
    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier {
        unreachable!("InternalDelegate::batch_verifier — sig batching is external-only")
    }
    fn commit_variable(
        &mut self,
        _commitment: &Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError> {
        unreachable!("InternalDelegate::commit_variable — commit/expr/mix are external-only")
    }
}

/// Base cost charged for every fetched instruction, executed or skip-scanned.
/// Expensive handlers add their calibrated work charge below.
const GAS_PER_INSTRUCTION: u64 = 1;

/// Variable-size heap work is charged monotonically in the current frame.
/// Drops do not refund it, so cumulative charged growth is a deterministic
/// upper bound on peak live allocation.
const GAS_PER_ALLOC_BYTE: u64 = 1;
const GAS_PER_ALLOC_ITEM: u64 = 1;

// Calibrated with `cargo bench -p flamevm --bench gas` on 2026-08-23.
// One gas represents roughly 100 ns of verifier work on the reference machine;
// every value is rounded upward. Memory charges remain deliberately more
// conservative because they prove a bound rather than model an allocator.
const GAS_HASH_BASE: u64 = 2;
const GAS_HASH_BLOCK: u64 = 2;
const GAS_POINT_DECOMPRESS: u64 = 25;
const GAS_SIGNATURE_VERIFY: u64 = 350;
const GAS_EXTERNAL_FINALIZE_BASE: u64 = 2_000;
const GAS_R1CS_ITEM: u64 = 120;
const GAS_MSM_VERIFY_BASE: u64 = 220;
const GAS_MSM_VERIFY_TERM: u64 = 12 + GAS_POINT_DECOMPRESS;

fn alloc_byte_gas(n: usize) -> Result<u64, VMError> {
    u64::try_from(n)
        .map(|n| n.saturating_mul(GAS_PER_ALLOC_BYTE))
        .map_err(|_| VMError::OutOfGas)
}

fn alloc_item_gas(n: usize) -> Result<u64, VMError> {
    u64::try_from(n)
        .map(|n| n.saturating_mul(GAS_PER_ALLOC_ITEM))
        .map_err(|_| VMError::OutOfGas)
}

fn linear_gas(base: u64, per_item: u64, n: usize) -> Result<u64, VMError> {
    let n = u64::try_from(n).map_err(|_| VMError::OutOfGas)?;
    base.checked_add(n.checked_mul(per_item).ok_or(VMError::OutOfGas)?)
        .ok_or(VMError::OutOfGas)
}

fn hash_gas(n: usize, block_bytes: usize) -> Result<u64, VMError> {
    // Include one padding/finalization block. The deliberate extra block at an
    // exact boundary keeps the formula simple and conservatively priced.
    let blocks = n
        .checked_div(block_bytes)
        .and_then(|blocks| blocks.checked_add(1))
        .ok_or(VMError::OutOfGas)?;
    linear_gas(GAS_HASH_BASE, GAS_HASH_BLOCK, blocks)
}

// Conservatively bound all sixteen significant-level SHA256 preimages.
fn cell_hash_gas(bytes: usize, refs: usize) -> Result<u64, VMError> {
    hash_gas(bytes + 4 + 34 * refs, 64)?
        .checked_add(15 * hash_gas(36 + 34 * refs, 64)?)
        .ok_or(VMError::OutOfGas)
}

fn factual_hash_gas(value: &Value) -> Result<u64, VMError> {
    match value {
        Value::Slice(slice) => cell_hash_gas(slice.remaining_bytes(), slice.remaining_refs()),
        Value::Builder(builder) => cell_hash_gas(builder.used_bytes(), builder.used_refs()),
        _ => Ok(0),
    }
}

fn shift_bytes(bytes: &mut [u8], count: usize, left: bool) {
    if count >= bytes.len() * 8 {
        bytes.fill(0);
        return;
    }
    let whole = count / 8;
    let bits = count % 8;
    if left {
        for index in 0..bytes.len() {
            let source = index + whole;
            let first = bytes.get(source).copied().unwrap_or(0);
            let second = bytes.get(source + 1).copied().unwrap_or(0);
            bytes[index] = (first << bits) | if bits == 0 { 0 } else { second >> (8 - bits) };
        }
    } else {
        for index in (0..bytes.len()).rev() {
            let first = index
                .checked_sub(whole)
                .and_then(|i| bytes.get(i))
                .copied()
                .unwrap_or(0);
            let second = index
                .checked_sub(whole + 1)
                .and_then(|i| bytes.get(i))
                .copied()
                .unwrap_or(0);
            bytes[index] = (first >> bits) | if bits == 0 { 0 } else { second << (8 - bits) };
        }
    }
}

fn r1cs_gas(items: usize) -> Result<u64, VMError> {
    linear_gas(0, GAS_R1CS_ITEM, items)
}

/// Maximum nested call/open/signcall depth. Re-entrancy is permitted
/// (ADR 0017), so without this a load-free A↔B cycle would be bounded
/// only by gas; the cap restores a structural bound (docs/flamevm.md §Design).
const MAX_CALL_DEPTH: usize = 64;

/// A frame's executable code. The prover holds decoded, witness-bearing
/// instructions; the verifier and internal actor execution hold raw
/// bytecode and decode one instruction at a time — never materializing a
/// `Vec<Instruction>`. See ADR 0015.
impl CallFrame {
    // The frame's code + cursor + lazy label table (fields `code` /
    // `cursor` / `labels`) are walked directly by these methods — exactly
    // Payload fetch and literal-reference consumption are independent streams.

    /// Returns the next instruction; `Ok(None)` at end of program. For
    /// `Script::Opaque` this decodes one instruction at the cursor and
    /// advances by its encoded length.
    pub(crate) fn next_instruction(&mut self) -> Result<Option<Instruction>, VMError> {
        self.prepare_program()?;
        if self.cursor >= self.program.as_ref().unwrap().len {
            return Ok(None);
        }
        let cursor = self.cursor;
        match &self.instructions {
            Some(instrs) => {
                let instr = instrs[self.instruction_cursor].clone();
                self.instruction_cursor += 1;
                self.cursor += instr.encoded_size();
                Ok(Some(instr))
            }
            None => {
                let bytes = self.program.as_ref().unwrap().bytes(cursor);
                let mut reader: &[u8] = bytes;
                let before = reader.len();
                let instr = Instruction::parse(&mut reader)?;
                let consumed = before - reader.len();
                self.cursor = cursor + consumed;
                Ok(Some(instr))
            }
        }
    }

    /// True iff the frame has reached its end. Test-only — production
    /// code drives to completion via the dispatch loop.
    #[cfg(test)]
    pub(crate) fn is_finished(&self) -> bool {
        match &self.program {
            Some(program) => self.cursor >= program.len,
            None => match self.code.as_ref().unwrap() {
                Script::Transparent(instructions) => instructions.is_empty(),
                Script::Opaque(bytes) => bytes.is_empty(),
                Script::Cell(cell) => !cell.is_pruned() && cell.payload().is_empty(),
            },
        }
    }

    /// Debits `n` gas from this frame's budget; `OutOfGas` when the
    /// budget is exhausted. Charged per fetched instruction (executed
    /// or skip-scanned), so prover (`Instrs`) and verifier (`Bytes`)
    /// meter identically — they walk the same instruction sequence.
    fn charge_gas(&mut self, n: u64) -> Result<(), VMError> {
        self.gas_used = self.gas_used.checked_add(n).ok_or(VMError::OutOfGas)?;
        if self.gas_used > self.gas_limit {
            return Err(VMError::OutOfGas);
        }
        Ok(())
    }

    /// Records `label n` at the current cursor (the position just after
    /// the label). `n` must be the next sequential index, except a
    /// re-visit (`n < len`, a loop back-edge) which must match the
    /// stored position. See ADR 0015.
    fn record_label(&mut self, n: usize) -> Result<(), VMError> {
        use core::cmp::Ordering;
        match n.cmp(&self.labels.len()) {
            Ordering::Equal => {
                self.labels.push((self.cursor, self.instruction_cursor));
                Ok(())
            }
            Ordering::Less if self.labels[n] == (self.cursor, self.instruction_cursor) => Ok(()),
            _ => Err(VMError::LabelOutOfOrder),
        }
    }

    /// Moves the cursor to `label n`: immediate if already recorded,
    /// else scans forward (recording labels, not executing) until it is
    /// reached. `LabelNotFound` if the scan hits end-of-program. See
    /// ADR 0015.
    fn jump_to_label(&mut self, n: usize) -> Result<(), VMError> {
        if n < self.labels.len() {
            (self.cursor, self.instruction_cursor) = self.labels[n];
            return Ok(());
        }
        // Scan forward via `next_instruction` (representation-agnostic:
        // decoded Vec or byte stream), recording labels, until `n`.
        // Each scanned instruction is charged like an executed one, so
        // long skips can't be free (ADR 0015 gas-on-scan).
        loop {
            self.charge_gas(GAS_PER_INSTRUCTION)?;
            match self.next_instruction()? {
                None => return Err(VMError::LabelNotFound),
                Some(Instruction::Label(m)) => {
                    let m = m as usize;
                    self.record_label(m)?;
                    if m == n {
                        return Ok(());
                    }
                }
                Some(_) => {}
            }
        }
    }
}

impl cells::GasMeter for CallFrame {
    fn charge(&mut self, amount: u64) -> Result<(), CellError> {
        self.charge_gas(amount)
            .map_err(|_| CellError::ResourceExhausted)
    }
}

/// Identity-bearing scope tag carried by every CallFrame.
pub enum CallKind {
    /// Outer scope of an external transaction.
    ExternalRoot,

    /// Outer scope of an internal tx; dispatched to the target's method.
    InternalRoot {
        actor: ActorID,
        caller: Option<ActorID>,
    },

    /// Synchronous actor-to-actor call inside an internal tx.
    ActorCall { actor: ActorID, caller: ActorID },

    /// `open` / `signcall` of a contract predicate. `external_context`
    /// snapshots the caller's `is_external()` at frame creation so
    /// `require_external` propagates correctly across the isolation
    /// boundary (ADR 0013).
    ContractOpen {
        predicate: Predicate,
        external_context: bool,
        /// Canonical id of the directly invoking actor, for `callerid`
        /// introspection only. This never grants actor authority.
        caller_id: Option<[u8; 32]>,
    },
}

impl CallKind {
    /// Returns the actor identity of this frame, if any. Used by the
    /// re-entrancy guard.
    pub fn actor(&self) -> Option<&ActorID> {
        match self {
            Self::InternalRoot { actor, .. } | Self::ActorCall { actor, .. } => Some(actor),
            Self::ExternalRoot | Self::ContractOpen { .. } => None,
        }
    }

    /// Returns only the caller's canonical id. `ContractOpen` stores this
    /// compact form for read-only attribution without inheriting the actor.
    pub fn caller_id(&self) -> Option<[u8; 32]> {
        match self {
            Self::InternalRoot { caller, .. } => caller.as_ref().map(ActorID::to_hash),
            Self::ActorCall { caller, .. } => Some(caller.to_hash()),
            Self::ContractOpen { caller_id, .. } => *caller_id,
            Self::ExternalRoot => None,
        }
    }
}

/// An isolated execution scope with its own stack, code, and gas budget.
/// Created by `call`, `open`, or the outermost frame of a transaction.
pub struct CallFrame {
    /// Isolated stack visible to scripts in this scope.
    pub(crate) stack: Vec<Value>,

    /// Only this actor's committed storage bodies; never another actor's cache.
    actor_cells: Arc<CellIndex>,

    /// Source consumed on first fetch; private instructions are only an overlay.
    code: Option<Script>,
    program: Option<Program>,
    entry_program: Option<Cell>,
    private_scripts: BTreeMap<CellID, Vec<Instruction>>,
    instructions: Option<Vec<Instruction>>,
    instruction_cursor: usize,
    /// Byte cursor into the program's Cell payloads.
    cursor: usize,
    /// `labels[n]` = (byte offset, private overlay index) after `label n`; filled lazily
    /// in appearance order. See ADR 0015.
    labels: Vec<(usize, usize)>,

    /// Identity / dispatch context for this frame.
    pub(crate) kind: CallKind,

    /// This frame's starting anchor (todo #4 — previously stored in
    /// every `CallKind` variant). Only the root internal frame uses it:
    /// `VM::new` seeds `last_anchor` from it. Child frames (`op_call` /
    /// contract-open) set `last_anchor` directly on entry, so theirs is
    /// informational. `None` for `ExternalRoot` (seeded by `op_input`).
    pub(crate) anchor: Option<Anchor>,

    /// Gas budget for this call.
    pub(crate) gas_limit: u64,
    pub(crate) gas_used: u64,

    /// Anchor that should replace `VM.last_anchor` when control
    /// returns to this frame from a child call. Populated at call
    /// entry as the `right` half of the parent's anchor split (the
    /// `left` half becomes the child frame's starting anchor).
    /// `None` when no call is in flight from this frame.
    pub(crate) post_call_anchor: Option<Anchor>,

    /// Snapshots taken at child-call entry, used to roll back side
    /// effects if the call fails. All `Vec` cursors and the fee
    /// accumulator are restored on failure; on success they're
    /// discarded.
    pub(crate) snap_txlog_len: usize,
    pub(crate) snap_deferred_sigs_len: usize,
    pub(crate) snap_deferred_multiplications: usize,
    pub(crate) snap_total_fee: CheckedFee,

    /// Entry-owned values returned to the caller when the child fails.
    /// Actor calls escrow their arguments; contract calls escrow the original
    /// locked Contract followed by their explicit arguments.
    pub(crate) snap_failure_values: Vec<Value>,
    /// Number of explicit call arguments. For contract calls this intentionally
    /// excludes the separately restored locked Contract.
    pub(crate) snap_failure_arg_count: usize,

    /// Snapshot of the delegate's MSM/signature batch state taken
    /// when this frame's child was pushed. Restored on child
    /// failure so the polluting MSM appended by the failed callee
    /// is dropped from the global batch — keeping the caller's
    /// proof verifiable. `None` when no call is in flight (or in
    /// internal context where the delegate has no real batch).
    pub(crate) snap_batch: Option<musig::BatchSnapshot>,

    /// Snapshot of the delegate's R1CS constraint system taken
    /// when this frame's child was pushed. Restored on child
    /// failure via `bulletproofs::r1cs::CheckpointableConstraintSystem::
    /// rollback` — truncates the witness/constraint vectors and
    /// rewinds the Fiat–Shamir transcript so any allocations and
    /// constraints the failed callee made are dropped. `None` in
    /// internal context (no real CS).
    pub(crate) snap_cs: Option<r1cs::Checkpoint>,
}

impl CallFrame {
    /// Test/compatibility entry: compiles source instructions to Cell code,
    /// retaining their private witness overlay.
    #[cfg(test)]
    pub fn new(instructions: Vec<Instruction>, kind: CallKind, gas_limit: u64) -> Self {
        Self::from_code(Script::Transparent(instructions), kind, gas_limit)
    }

    /// Compatibility source-bytecode entry. Production transaction and actor
    /// entry points use `from_cell`; bounded String/signcall code also adapts here.
    pub(crate) fn from_bytecode(bytecode: Vec<u8>, kind: CallKind, gas_limit: u64) -> Self {
        Self::from_code(Script::Opaque(bytecode), kind, gas_limit)
    }

    /// Sets this frame's starting anchor (todo #4). Chained at the
    /// internal-root / call / contract-open construction sites.
    pub(crate) fn with_anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    pub(crate) fn from_code(code: Script, kind: CallKind, gas_limit: u64) -> Self {
        Self {
            stack: Vec::new(),
            actor_cells: Arc::new(CellIndex::new()),
            code: Some(code),
            program: None,
            entry_program: None,
            private_scripts: BTreeMap::new(),
            instructions: None,
            instruction_cursor: 0,
            cursor: 0,
            labels: Vec::new(),
            kind,
            anchor: None,
            gas_limit,
            gas_used: 0,
            post_call_anchor: None,
            snap_txlog_len: 0,
            snap_deferred_sigs_len: 0,
            snap_deferred_multiplications: 0,
            snap_total_fee: CheckedFee::zero(),
            snap_failure_values: Vec::new(),
            snap_failure_arg_count: 0,
            snap_batch: None,
            snap_cs: None,
        }
    }

    /// Enters authenticated program Cells without flattening their bytecode.
    pub(crate) fn from_cell(root: Cell, kind: CallKind, gas_limit: u64) -> Result<Self, VMError> {
        let program = Program::new(root.clone())?;
        let mut frame = Self::from_bytecode(Vec::new(), kind, gas_limit);
        frame.code = None;
        frame.entry_program = Some(root);
        frame.program = Some(program);
        Ok(frame)
    }

    fn prepare_program(&mut self) -> Result<(), VMError> {
        if self.program.is_none() {
            let code = self.code.as_ref().expect("unprepared frame has source");
            let root = match code {
                Script::Transparent(instructions) => {
                    let (root, scripts) = crate::script::compile_program(instructions)?;
                    self.private_scripts = scripts;
                    root
                }
                _ => code.to_cell()?,
            };
            self.instructions = self.private_scripts.get(&root.id()).cloned();
            let program = Program::new(root.clone())?;
            self.entry_program = Some(root);
            self.code = None;
            self.program = Some(program);
        }
        Ok(())
    }
}

/// Outcome of a successful transaction execution. Returned by both
/// `Prover::prove` and `Verifier::verify`.
pub struct TxResult {
    /// Canonical 32-byte transaction id.
    pub txid: TxID,

    /// Full txlog including the `Header` entry at index 0.
    pub txlog: Vec<TxEntry>,

    /// Aggregate fee in flames recorded by `op_fee`.
    pub total_fee: u64,

    /// Gas spent by instructions, logical allocation, scheduled finalization,
    /// and asynchronous message grants.
    pub gas_used: u64,

    /// Exact number of multiplication gates in the final constraint system.
    pub multiplications: usize,

    /// Compatibility byte-only source when representable, otherwise root
    /// payload only. Never a standalone native graph; verify using `program`.
    pub bytecode: Vec<u8>,
    /// Executed root program, with literal Cell operands and explicit continuations.
    pub program: Cell,

    /// The immutable lookup snapshot committed by the external transaction.
    /// Inline program bodies are also available; use ExternalTx::execution_cells
    /// when delivering induced actor transactions.
    pub cells: Arc<CellIndex>,

    /// R1CS proof. `Some` on the prover side, `None` on the verifier
    /// side (the verifier consumed it during `cs.verify`).
    pub proof: Option<R1CSProof>,

    /// Deferred-signature records — `Explicit` items (already
    /// batch-verified for the verifier; surfaced for caller
    /// inspection) and `TxBound` items (the caller used these to
    /// build the aggregate multi-signature; verifier sees them
    /// after the fact for the same audit shape).
    pub deferred_sigs: Vec<DeferredSig>,
}

/// Manual Debug — the linear `Contract` in `txlog` blocks `#[derive]`.
impl core::fmt::Debug for TxResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TxResult")
            .field("txid", &self.txid)
            .field("txlog.len", &self.txlog.len())
            .field("total_fee", &self.total_fee)
            .field("gas_used", &self.gas_used)
            .field("multiplications", &self.multiplications)
            .field("bytecode.len", &self.bytecode.len())
            .field("proof_present", &self.proof.is_some())
            .field("deferred_sigs.len", &self.deferred_sigs.len())
            .finish()
    }
}

pub(crate) struct VM {
    header: TxHeader,
    block_height: u64,
    cells: Arc<CellIndex>,
    /// Serialized lookup snapshot, separate from the inline program bodies.
    witnesses: Arc<CellIndex>,
    program_scope_ready: bool,
    contract_witnesses: BTreeMap<ContractID, Contract>,
    script_witnesses: BTreeMap<CellID, Vec<Instruction>>,
    value_witnesses: BTreeMap<CellID, Value>,

    /// Per-tx current anchor. `None` for fresh ExternalRoot txs (the
    /// first `op_input` seeds it); `Some(M)` at the start of an
    /// internal tx (where `M` is the delivering Message's anchor —
    /// itself a split-child from the originating tx's `op_send`).
    /// Consumed-and-replaced via split by `contract` / `output` / `send`;
    /// `call` / `open` / `signcall` split it into disjoint callee and
    /// caller-continuation subtrees.
    pub(crate) last_anchor: Option<Anchor>,

    current_call: CallFrame,
    call_stack: Vec<CallFrame>,

    /// Effects committed by the external body or the derived internal ID.
    pub(crate) txlog: Vec<TxEntry>,

    /// Running per-tx fee accumulator (overflow → `FeeTooHigh`).
    total_fee: CheckedFee,

    /// Signature checks deferred to `Delegate::finalize`.
    deferred_sigs: Vec<DeferredSig>,

    /// Multiplications added by deferred randomized constraints. Ordinary
    /// gates are read from the finalized constraint system by the delegate.
    deferred_multiplications: usize,
}

/// Execution-local resolution, priced identically for resident and unloaded refs.
/// A cache hit cannot turn an absent transaction witness into an available one.
struct ExecutionCells<'a> {
    external: &'a CellIndex,
    actor: &'a CellIndex,
    gas_used: &'a mut u64,
    gas_limit: u64,
}

impl ExecutionCells<'_> {
    fn charge(&mut self, gas: u64) -> Result<(), CellError> {
        *self.gas_used = self
            .gas_used
            .checked_add(gas)
            .ok_or(CellError::ResourceExhausted)?;
        if *self.gas_used > self.gas_limit {
            return Err(CellError::ResourceExhausted);
        }
        Ok(())
    }
}

impl CellReader for ExecutionCells<'_> {
    fn read(&mut self, id: CellID, resident: Option<&Arc<Cell>>) -> Result<Arc<Cell>, CellError> {
        self.charge(1)?;
        let cell = match resident {
            Some(cell) => Arc::clone(cell),
            None => self
                .actor
                .get(&id)
                .or_else(|| self.external.get(&id))
                .ok_or(CellError::MissingCell(id))?,
        };
        self.charge((cell.record_size() as u64).saturating_add(cell.refs().len() as u64))?;
        Ok(cell)
    }
}

impl VM {
    fn resolver(&mut self) -> ExecutionCells<'_> {
        ExecutionCells {
            external: &self.cells,
            actor: &self.current_call.actor_cells,
            gas_limit: self.current_call.gas_limit,
            gas_used: &mut self.current_call.gas_used,
        }
    }

    fn with_cells(mut self, cells: Arc<CellIndex>) -> Result<Self, CellError> {
        if matches!(self.current_call.kind, CallKind::ExternalRoot) {
            // Reject unencodable program data before executing private witnesses.
            cells.to_cell()?;
        }
        self.witnesses = Arc::clone(&cells);
        self.cells = cells;
        Ok(self)
    }
    /// Executes an external transaction script with the given delegate,
    /// then calls `delegate.finalize`. Crate-internal: the public path
    /// is `ScriptBuilder::build_tx` / `ExternalTx::verify`.
    #[cfg(test)]
    pub(crate) fn execute_external<D: Delegate>(
        header: TxHeader,
        script: Vec<u8>,
        gas_limit: u64,
        mut delegate: D,
    ) -> Result<TxResult, VMError> {
        let mut frame = CallFrame::from_bytecode(script.clone(), CallKind::ExternalRoot, gas_limit);
        frame.charge_gas(alloc_byte_gas(script.len())?)?;
        frame.charge_gas(GAS_EXTERNAL_FINALIZE_BASE)?;
        let mut vm = Self::new(header, frame);
        while vm.step_external(&mut delegate)? {}
        vm.into_result(script, None)
    }

    /// Runs an external-root program through the VM without finalizing
    /// the delegate. Used by `Prover` / `Verifier` which take a ScriptBuilder
    /// (with witnesses inline on the prover side).
    pub(crate) fn run<D: Delegate>(
        header: TxHeader,
        program: ScriptBuilder,
        gas_limit: u64,
        delegate: &mut D,
    ) -> Result<TxResult, VMError> {
        // Freeze public availability before any private witness is executed.
        let cells = Arc::new(program.cell_witnesses()?);
        let contract_witnesses = program.contract_witnesses();
        let script_witnesses = program.script_witnesses()?;
        let (root, private_scripts) = crate::script::compile_program(program.instructions())?;
        let bytecode = root.payload().to_vec();
        let initial_gas =
            alloc_byte_gas(bytecode.len())?.saturating_add(GAS_EXTERNAL_FINALIZE_BASE);
        if initial_gas > gas_limit {
            return Err(VMError::OutOfGas);
        }
        let mut frame = CallFrame::from_cell(root, CallKind::ExternalRoot, gas_limit)?;
        frame.instructions = private_scripts
            .get(&frame.entry_program.as_ref().unwrap().id())
            .cloned();
        frame.private_scripts = private_scripts;
        frame.charge_gas(initial_gas)?;
        let mut vm = Self::new(header, frame).with_cells(cells)?;
        vm.contract_witnesses = contract_witnesses;
        vm.script_witnesses = script_witnesses;
        while vm.step_external(delegate)? {}
        vm.into_result(bytecode, None)
    }

    /// Verifier entry: fetches instructions from Cell payloads on demand.
    pub(crate) fn run_program_with_cells<D: Delegate>(
        header: TxHeader,
        program: Cell,
        gas_limit: u64,
        delegate: &mut D,
        cells: &CellIndex,
    ) -> Result<TxResult, VMError> {
        let bytecode = program.payload().to_vec();
        let initial_gas =
            alloc_byte_gas(bytecode.len())?.saturating_add(GAS_EXTERNAL_FINALIZE_BASE);
        if initial_gas > gas_limit {
            return Err(VMError::OutOfGas);
        }
        let mut frame = CallFrame::from_cell(program, CallKind::ExternalRoot, gas_limit)?;
        frame.charge_gas(initial_gas)?;
        let mut vm = Self::new(header, frame).with_cells(Arc::new(cells.clone()))?;
        while vm.step_external(delegate)? {}
        vm.into_result(bytecode, None)
    }

    /// Executes an internal transaction. On clean exit runs the tx-end
    /// self-destruct sweep against `registry`.
    #[cfg(test)]
    pub fn execute_internal(
        header: TxHeader,
        message: Message,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
    ) -> Result<TxResult, VMError> {
        Self::execute_internal_with_cells(
            header,
            message,
            registry,
            block,
            Arc::new(CellIndex::new()),
        )
    }

    pub fn execute_internal_with_cells(
        header: TxHeader,
        message: Message,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
        cells: Arc<CellIndex>,
    ) -> Result<TxResult, VMError> {
        // Admission and MessageID/transport construction re-encode arguments.
        // Price that work before cloning or traversing any of their Cell graphs.
        let mut message_work = 0u64;
        for value in message.payload() {
            message_work = message_work
                .checked_add(
                    value
                        .encoding_gas()?
                        .checked_mul(4)
                        .ok_or(VMError::OutOfGas)?,
                )
                .and_then(|gas| gas.checked_add(value.clone_gas().saturating_mul(2)))
                .ok_or(VMError::OutOfGas)?;
        }
        if let ActorID::Constructor(code) = &message.target {
            message_work = message_work
                .checked_add(alloc_byte_gas(code.len())?.saturating_mul(2))
                .ok_or(VMError::OutOfGas)?;
        }
        if message_work > message.gas {
            return Err(VMError::OutOfGas);
        }
        registry.push_checkpoint();
        let target = message.target.clone();
        let message_bytes = message.encoded_size();

        if let ActorID::Constructor(bytes) = &message.target {
            if alloc_byte_gas(bytes.len())? > message.gas {
                registry.pop_checkpoint_rollback();
                return Err(VMError::OutOfGas);
            }
        }

        // Deploy-on-first-delivery (spec §Actors): a Constructor-form
        // target carries the actor's code on the wire and the id
        // commits to it (`id = H(bytes)`), so the first message to a
        // not-yet-deployed actor instantiates it — code = constructor
        // bytes and empty state. The actor remains provisional until it buys
        // enough storage during this transaction.
        let mut deployed_code = None;
        if !registry.exists(&message.target) {
            if let ActorID::Constructor(bytes) = &message.target {
                if let Err(e) =
                    registry.deploy(message.target.clone(), bytes.clone(), empty_state())
                {
                    registry.pop_checkpoint_rollback();
                    return Err(e);
                }
                deployed_code = Some(bytes.clone());
            }
        }
        let code_bytes = match registry.actor_code_bytes(&message.target) {
            Ok(bytes) => bytes,
            Err(e) => {
                registry.pop_checkpoint_rollback();
                return Err(e);
            }
        };
        let initial_gas = message_work
            .saturating_add(alloc_byte_gas(message_bytes)?)
            .saturating_add(code_bytes.saturating_mul(GAS_PER_ALLOC_BYTE))
            .saturating_add(alloc_item_gas(message.payload().len())?);
        if initial_gas > message.gas {
            registry.pop_checkpoint_rollback();
            return Err(VMError::OutOfGas);
        }
        let actor_cells = match registry.actor_cells(&message.target) {
            Ok(cells) => cells,
            Err(error) => {
                registry.pop_checkpoint_rollback();
                return Err(error);
            }
        };
        let mut initial_gas = initial_gas;
        let mut resolver = ExecutionCells {
            external: &cells,
            actor: &actor_cells,
            gas_used: &mut initial_gas,
            gas_limit: message.gas,
        };
        let script = match registry.load_program_with_cells(&message.target, &mut resolver) {
            Ok(script) => script,
            Err(e) => {
                registry.pop_checkpoint_rollback();
                return Err(match e {
                    VMError::Cell(CellError::ResourceExhausted) => VMError::OutOfGas,
                    other => other,
                });
            }
        };
        // MessageID is the canonical hash of the whole send (anchor,
        // target, caller, payload, gas, refund
        // predicate) — analogous to ContractID for Output. Capture
        // before the move below.
        let send_id = *message.id().as_bytes();
        let anchor = message.anchor;
        let caller = message.caller.clone();
        let gas = message.gas;
        let payload = message.into_payload();
        let kind = CallKind::InternalRoot {
            actor: target.clone(),
            caller,
        };
        let mut frame = match CallFrame::from_cell(script, kind, gas) {
            Ok(frame) => frame.with_anchor(anchor),
            Err(error) => {
                registry.pop_checkpoint_rollback();
                return Err(error);
            }
        };
        frame.actor_cells = actor_cells;
        if let Err(e) = frame.charge_gas(initial_gas) {
            registry.pop_checkpoint_rollback();
            return Err(e);
        }
        // Deliver the message payload onto the recv's stack (in payload
        // order) before dispatch runs — symmetric with `op_call`, which
        // pushes its args. The dispatch selector (ADR 0020) rides as the
        // topmost payload arg.
        for v in payload {
            frame.stack.push(v);
        }
        let mut vm = Self::new(header, frame).with_cells(cells)?;
        vm.block_height = block.height;
        // Commit the triggering MessageID into the Internal TxID merkle
        // root. Symmetric with `op_input` for external txs: the first
        // post-Header effect identifies *what consumed-once entity*
        // brought this tx into existence.
        vm.txlog.push(TxEntry::Receive(send_id));
        if let Some(code) = deployed_code {
            vm.txlog.push(TxEntry::ActorDeploy {
                actor: ActorID::Hash(target.to_hash()),
                code,
            });
        }

        let mut run_err: Option<VMError> = None;
        loop {
            match vm.step_internal_with_registry(registry) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => {
                    run_err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = run_err {
            registry.pop_checkpoint_rollback();
            return Err(e);
        }
        for actor in registry.commit_tx_destructions() {
            vm.txlog.push(TxEntry::ActorDestroy { actor });
        }
        if registry.exists(&target) {
            if let Err(e) = registry.validate_actor_storage(&target, block.height) {
                registry.pop_checkpoint_rollback();
                return Err(e);
            }
        }
        registry.pop_checkpoint_commit();
        vm.into_result(Vec::new(), None)
    }

    fn new(header: TxHeader, initial_call: CallFrame) -> Self {
        // Header is the first txlog entry so TxID binds to version + locktime.
        let cells = Arc::new(CellIndex::new());
        let txlog = vec![TxEntry::Header(header)];
        // Seed last_anchor from the root frame's kind: ExternalRoot →
        // None (op_input must seed); InternalRoot → Some(Message.anchor)
        // (already unique from prior tx's op_send split).
        let last_anchor = initial_call.anchor;
        Self {
            header,
            block_height: 0,
            witnesses: Arc::clone(&cells),
            cells,
            program_scope_ready: false,
            contract_witnesses: BTreeMap::new(),
            script_witnesses: BTreeMap::new(),
            value_witnesses: BTreeMap::new(),
            last_anchor,
            current_call: initial_call,
            call_stack: Vec::new(),
            txlog,
            total_fee: CheckedFee::zero(),
            deferred_sigs: Vec::new(),
            deferred_multiplications: 0,
        }
    }

    /// Splits `last_anchor`: returns the `left` half (to embed in
    /// a fresh unique-anchored entity — contract, message), and writes
    /// the `right` half back. Hard-fails `AnchorMissing` if no
    /// anchor has been claimed yet.
    fn consume_anchor(&mut self) -> Result<Anchor, VMError> {
        let parent = self.last_anchor.ok_or(VMError::AnchorMissing)?;
        let (left, right) = parent.split();
        self.last_anchor = Some(right);
        Ok(left)
    }

    /// Splits the current anchor for a call entry: returns the
    /// `left` half (becomes the callee's starting `last_anchor`) and
    /// stores the `right` half in the parent frame's
    /// `post_call_anchor` slot — so when control returns to the
    /// parent (success *or* failure), the parent's anchor chain
    /// resumes deterministically from `right`. Also stashes the
    /// snapshots needed to roll back side effects on failure.
    /// `Anchor` flows uninitialized → `AnchorMissing`.
    fn split_anchor_for_call(&mut self) -> Result<Anchor, VMError> {
        let parent_anchor = self.last_anchor.ok_or(VMError::AnchorMissing)?;
        let (left, right) = parent_anchor.split();
        self.current_call.post_call_anchor = Some(right);
        // Snapshot effect counters for failure rollback.
        self.current_call.snap_txlog_len = self.txlog.len();
        self.current_call.snap_deferred_sigs_len = self.deferred_sigs.len();
        self.current_call.snap_deferred_multiplications = self.deferred_multiplications;
        self.current_call.snap_total_fee = self.total_fee;
        Ok(left)
    }

    /// External IDs bind the body; derived internal IDs retain the effect root.
    fn into_result(
        mut self,
        bytecode: Vec<u8>,
        proof: Option<R1CSProof>,
    ) -> Result<TxResult, VMError> {
        let txlog = mem::take(&mut self.txlog);
        let deferred_sigs = mem::take(&mut self.deferred_sigs);
        self.current_call.prepare_program()?;
        let program = self.current_call.entry_program.as_ref().unwrap().clone();
        let txid = if matches!(self.current_call.kind, CallKind::ExternalRoot) {
            TxID(
                crate::tx::external_program_body(self.header, &program, &self.witnesses, &txlog)?
                    .id(),
            )
        } else {
            TxID::from_log(&txlog)
        };
        let bytecode = if matches!(self.current_call.kind, CallKind::ExternalRoot) {
            crate::code_from_cell(&program, &mut (), usize::MAX)
                .unwrap_or_else(|_| program.payload().to_vec())
        } else {
            bytecode
        };
        Ok(TxResult {
            txid,
            txlog,
            total_fee: self.total_fee.total(),
            // Root frame's instruction and allocation gas (spec §gas).
            gas_used: self.current_call.gas_used,
            multiplications: self.deferred_multiplications,
            bytecode,
            program,
            cells: self.witnesses,
            proof,
            deferred_sigs,
        })
    }

    /// Gate for external-only opcodes (CS-touching handlers).
    fn require_external(&self) -> Result<(), VMError> {
        if self.is_external() {
            Ok(())
        } else {
            Err(VMError::ExternalOnly)
        }
    }

    /// True iff CS-touching opcodes are permitted. `ExternalRoot` is
    /// always external; `ContractOpen` inherits the caller's snapshot.
    fn is_external(&self) -> bool {
        match self.current_call.kind {
            CallKind::ExternalRoot => true,
            CallKind::ContractOpen {
                external_context, ..
            } => external_context,
            _ => false,
        }
    }

    /// External-context step.
    pub(crate) fn step_external<D: Delegate>(&mut self, delegate: &mut D) -> Result<bool, VMError> {
        self.step(delegate, None)
    }

    /// Internal-context step without a registry — registry-touching
    /// opcodes error `RegistryUnavailable`. Test-only.
    #[cfg(test)]
    pub(crate) fn step_internal(&mut self) -> Result<bool, VMError> {
        let mut stub = InternalDelegate;
        self.step(&mut stub, None)
    }

    /// Internal-context step with a live registry handle.
    pub(crate) fn step_internal_with_registry(
        &mut self,
        registry: &mut dyn ActorRegistry,
    ) -> Result<bool, VMError> {
        let mut stub = InternalDelegate;
        self.step(&mut stub, Some(registry))
    }

    /// Executes one instruction. Returns `Ok(true)` to continue,
    /// `Ok(false)` to stop. Errors that occur inside a nested
    /// call frame are caught — the frame is unwound via
    /// `fail_current_call` and its entry escrow plus `count, 0` is
    /// pushed onto the parent's stack. Errors at the outermost frame
    /// propagate to the caller (kill the tx).
    fn step<D: Delegate>(
        &mut self,
        delegate: &mut D,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<bool, VMError> {
        // Depth before the step. If the step pushes a frame
        // (`call`/`open`/`signcall` succeeded in entering), depth
        // grows by 1 and we snapshot the delegate's batch on the
        // *parent* frame — so a future failure can restore the
        // pre-call batch state. Snapshots only happen in external
        // context (`InternalDelegate::batch_verifier` panics, so we
        // can't query it in internal mode — see `is_external`).
        let depth_before = self.call_stack.len();
        // Reborrow for step_inner, then hand the original to post_step
        // (one borrow live at a time — borrow checker requires the split).
        let (result, registry) = match registry {
            None => (self.step_inner(delegate, None), None),
            Some(r) => {
                let prepared = if let Some(actor) = self.current_call.kind.actor() {
                    r.actor_cells(actor)
                        .map(|cells| self.current_call.actor_cells = cells)
                } else {
                    Ok(())
                };
                let result = prepared.and_then(|()| self.step_inner(delegate, Some(&mut *r)));
                (result, Some(r))
            }
        };
        self.post_step(delegate, registry, depth_before, result)
    }

    /// Shared step post-processing: snapshot/checkpoint on frame entry,
    /// checkpoint-commit on clean exit, fail_current_call on error.
    fn post_step<D: Delegate>(
        &mut self,
        delegate: &mut D,
        mut registry: Option<&mut dyn ActorRegistry>,
        depth_before: usize,
        result: Result<bool, VMError>,
    ) -> Result<bool, VMError> {
        match result {
            Ok(cont) => {
                let depth_after = self.call_stack.len();
                if depth_after > depth_before {
                    self.snapshot_parent_on_push(delegate);
                    // Open an undo frame so a future failure of the
                    // newly-pushed frame can roll back any `op_save`/
                    // `op_load` state moves (a failed `load` restores
                    // the checked-out state, so the actor isn't
                    // spuriously self-destructed) — closes F1 of the
                    // op_save audit.
                    if let Some(r) = registry.as_mut() {
                        r.push_checkpoint();
                    }
                } else if depth_after < depth_before {
                    // Clean exit: keep effects, drop the snapshot.
                    if let Some(r) = registry.as_mut() {
                        r.pop_checkpoint_commit();
                    }
                }
                Ok(cont)
            }
            Err(e) => {
                let e = match e {
                    VMError::Cell(CellError::ResourceExhausted) => VMError::OutOfGas,
                    other => other,
                };
                if self.call_stack.is_empty() {
                    return Err(e);
                }
                self.fail_current_call(delegate, registry);
                Ok(true)
            }
        }
    }

    /// On call entry (depth grew), snapshot the delegate's batch + CS
    /// onto the parent frame so a callee failure can roll them back.
    /// No-op outside external context (internal delegates have no batch).
    fn snapshot_parent_on_push<D: Delegate>(&mut self, delegate: &mut D) {
        if !self.is_external() {
            return;
        }
        use musig::BatchCheckpoint;
        use r1cs::CheckpointableConstraintSystem;
        let batch_snap = delegate.batch_verifier().snapshot();
        let cs_snap = delegate.cs().checkpoint();
        if let Some(parent) = self.call_stack.last_mut() {
            parent.snap_batch = Some(batch_snap);
            parent.snap_cs = Some(cs_snap);
        }
    }

    /// Single-step body, separated from `step` so the error path
    /// can be uniformly caught.
    fn step_inner<D: Delegate>(
        &mut self,
        delegate: &mut D,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<bool, VMError> {
        self.current_call.charge_gas(GAS_PER_INSTRUCTION)?;
        if !self.program_scope_ready && matches!(self.current_call.kind, CallKind::ExternalRoot) {
            self.current_call.prepare_program()?;
            let inline = CellIndex::collect(Arc::new(
                self.current_call.entry_program.as_ref().unwrap().clone(),
            ))?;
            let mut available = self.witnesses.as_ref().clone();
            available.extend(&inline)?;
            self.cells = Arc::new(available);
            self.program_scope_ready = true;
        }
        let Some(instr) = self.current_call.next_instruction()? else {
            return self.finish_call();
        };
        use Instruction as I;
        match instr {
            I::PushInt(i) => {
                self.push_value(Value::Scalar(i));
                Ok(())
            }
            I::BytesLiteral(literal) => {
                let reference = self.current_call.program.as_mut().unwrap().load_ref()?;
                let witness = if matches!(literal.as_ref(), String::Witness(_)) {
                    if literal.to_cell()?.id() != reference.id() {
                        return Err(VMError::TaprootProofMismatch);
                    }
                    Some(literal)
                } else {
                    None
                };
                self.push_value(Value::Cell(crate::CellValue { reference, witness }));
                Ok(())
            }
            I::PushPoint(p) => {
                // Witness-bearing variants flow through unchanged;
                // verifier-side decoded form is `Point::Opaque`.
                self.push_value(Value::Point(p));
                Ok(())
            }
            I::PushToken => self.op_pushtoken(),
            I::PushCell(_) => {
                let reference = self.current_call.program.as_mut().unwrap().load_ref()?;
                self.push_value(Value::Cell(reference.into()));
                Ok(())
            }
            I::Exec => self.op_exec(),
            I::Drop => self.op_drop(),
            I::Nop => self.op_nop(),
            I::Dup => self.op_dup(),
            I::Roll => self.op_roll(),
            I::DupK(k) => self.op_dup_k(k as usize),
            I::RollK(k) => self.op_roll_k(k as usize),

            I::Builder => {
                self.push_value(Value::Builder(CellBuilder::new()));
                Ok(())
            }
            I::Slice => self.op_slice(),
            I::EndCell => self.op_endcell(),
            I::ReadUint => {
                let n = self.pop_byte_count(32)?;
                self.op_read_uint(n)
            }
            I::ReadScalar => self.op_read_uint(32),
            I::ReadBytes => self.op_read_bytes(),
            I::ReadPoint => self.op_read_point(),
            I::WriteUint => {
                let n = self.pop_byte_count(32)?;
                self.op_write_uint(n)
            }
            I::WriteScalar => self.op_write_uint(32),
            I::AppendBytes => self.op_append_bytes(),
            I::AppendRefs => self.op_append_refs(),
            I::WriteZeros => self.op_write_zeros(),
            I::BitNot => self.op_bit_not(),
            I::BitOr => self.op_bitwise(|a, b| a | b),
            I::BitAnd => self.op_bitwise(|a, b| a & b),
            I::BitXor => self.op_bitwise(|a, b| a ^ b),
            I::ShiftLeft => self.op_shift(true),
            I::ShiftRight => self.op_shift(false),
            I::Keccak256 => self.op_hash::<sha3::Keccak256>(136),

            I::Abs => self.op_abs(),
            I::Eq => self.op_eq(delegate),
            I::Neg => self.op_neg(delegate),
            I::Add => self.op_add(delegate),
            I::Mul => self.op_mul(delegate),
            I::DivMod => self.op_divmod(),
            I::Mod252 => self.op_mod252(),
            I::Not => self.op_not(delegate),
            I::And => self.op_and(delegate),
            I::Or => self.op_or(delegate),
            I::Size => self.op_size(),

            I::Dict => self.op_dict(),
            I::Put => self.op_put(),
            I::Replace => self.op_replace(),
            I::Get => self.op_get(),
            I::GetOpt => self.op_getopt(),
            I::GetDup => self.op_getdup(),
            I::First => self.op_first(),
            I::Last => self.op_last(),
            I::Next => self.op_next(),

            I::Transcript => self.op_transcript(),
            I::TWrite => self.op_twrite(),
            I::TRead => self.op_tread(),
            I::Sha256 => self.op_hash::<sha2::Sha256>(64),
            I::Sha512 => self.op_hash::<sha2::Sha512>(128),
            I::Sha3 => self.op_hash::<sha3::Sha3_256>(136),
            I::CellHash => self.op_cell_hash(),
            I::Log => self.op_log(),

            I::Amount => self.op_amount(),
            I::IssuePriv => self.op_issuepriv(delegate),
            I::IssuePub => self.op_issuepub(),
            I::Retire => self.op_retire(),
            I::Borrow => self.op_borrow(delegate),
            I::Merge => self.op_merge(),
            I::Split => self.op_split(),
            I::IssuePubFlv => self.op_issuepubflv(),
            I::IssuePrivFlv => self.op_issueprivflv(),

            I::Alloc(w) => self.op_alloc(w, delegate),
            I::Expr => self.op_expr(delegate),
            I::Range => self.op_range(delegate),
            I::Scalar => self.op_scalar(),
            I::Commit => self.op_commit(),
            I::Decrypt => self.op_decrypt(delegate),
            I::Mix => self.op_mix(delegate),
            I::Fee => self.op_fee(delegate),
            I::Verify => self.op_verify(delegate),

            I::Label(n) => self.op_label(n),
            I::Jump(n) => self.op_jump(n),
            I::JumpIf(n) => self.op_jumpif(n),
            I::Return => self.op_return(),
            I::Type => self.op_type(),

            I::Input => self.op_input(),
            I::Contract => self.op_contract(),
            I::Output => self.op_output(),
            I::Open => self.op_open(),
            I::Send => self.op_send(),
            I::Call => self.op_call(registry),
            I::Load => self.op_load(registry),
            I::Save => self.op_save(registry),
            I::Setcode => self.op_setcode(registry),
            I::AddStorage => self.op_addstorage(registry),
            I::QuoteStorage => self.op_quotestorage(registry),
            I::Signtx => self.op_signtx(),
            I::Signcall => self.op_signcall(),

            I::Timelock => self.op_timelock(),
            // version → tx header version (spec §version).
            I::Version => {
                self.push_value(Value::Scalar(Scalar::from(self.header.version as u64)));
                Ok(())
            }
            I::Selfid => self.op_selfid(),
            I::Anchor => self.op_anchor(),
            I::Gas => self.op_gas(),
            I::Usage => self.op_usage(registry),
            I::Callerid => self.op_callerid(),
            // gaslimit → immutable frame budget.
            I::Gaslimit => {
                self.push_value(Value::Scalar(Scalar::from(self.current_call.gas_limit)));
                Ok(())
            }
            I::Capacity => self.op_capacity(registry),
            I::Height => {
                self.push_value(Value::Scalar(Scalar::from(self.block_height)));
                Ok(())
            }

            I::Ext(b) => Err(VMError::UnknownOpcode(b)),
        }?;
        Ok(true)
    }

    /// Pops the current frame back to its caller on clean exit. Stack
    /// must be empty (use `return k` to send values across the boundary).
    /// Leftover gas is refunded to the parent. Implicit clean exits
    /// push `{0, 1}` onto the parent's stack (success with k=0).
    /// Shared clean-exit epilogue: refund leftover gas to the parent
    /// (already swapped in by the caller), apply the parent's post-call
    /// anchor, and discard the entry-time batch/CS snapshots
    /// (failure-path only). Order is load-bearing — identical in
    /// `finish_call` and `op_return`.
    fn clean_exit_to_parent(&mut self, leftover_gas: u64) {
        self.current_call.gas_used = self.current_call.gas_used.saturating_sub(leftover_gas);
        if let Some(post) = self.current_call.post_call_anchor.take() {
            self.last_anchor = Some(post);
        }
        self.current_call.snap_batch = None;
        self.current_call.snap_cs = None;
        self.current_call.snap_failure_values.clear();
        self.current_call.snap_failure_arg_count = 0;
    }

    fn finish_call(&mut self) -> Result<bool, VMError> {
        if !self.current_call.stack.is_empty() {
            return Err(VMError::StackNotClean);
        }
        let leftover_gas = self
            .current_call
            .gas_limit
            .saturating_sub(self.current_call.gas_used);

        if let Some(parent) = self.call_stack.pop() {
            self.current_call = parent;
            self.clean_exit_to_parent(leftover_gas);
            // Success marker with k=0: stack += [count=0, success=1].
            self.current_call
                .stack
                .push(Value::Scalar(Scalar::from(0u64)));
            self.current_call.stack.push(Value::Scalar(Scalar::ONE));
            return Ok(true);
        }
        // Outermost call returned: entire tx complete.
        Ok(false)
    }

    /// Discards the current frame on failure: pops it without
    /// preserving its effects, rolls back side-effects from the
    /// parent's snapshot (txlog tail, deferred-sigs tail, total_fee,
    /// delegate batch state, R1CS constraint system), applies the
    /// parent's `post_call_anchor`, and restores entry-owned values
    /// followed by their count and a zero status. Caller's effects up to the
    /// failed call are preserved; the parent script continues at
    /// the instruction after the call.
    ///
    /// Actor-state rollback is wired via the registry's checkpoint
    /// stack (push on frame entry, restore here). Closes the F1 audit
    /// finding — a failed callee's `op_save`/`op_load` state moves are
    /// undone (a checked-out state is restored to present), so the
    /// registry stays consistent with the truncated txlog.
    fn fail_current_call<D: Delegate>(
        &mut self,
        delegate: &mut D,
        mut registry: Option<&mut dyn ActorRegistry>,
    ) {
        // Cannot fail the outermost frame — caller of this helper
        // must ensure call_stack is non-empty.
        let parent = self
            .call_stack
            .pop()
            .expect("fail_current_call: outermost frame errors must propagate");
        self.current_call = parent;
        let failure_values = mem::take(&mut self.current_call.snap_failure_values);
        let failure_arg_count = self.current_call.snap_failure_arg_count;
        self.current_call.snap_failure_arg_count = 0;
        // Roll back side effects via the snapshots taken at call
        // entry.
        self.txlog.truncate(self.current_call.snap_txlog_len);
        self.deferred_sigs
            .truncate(self.current_call.snap_deferred_sigs_len);
        self.deferred_multiplications = self.current_call.snap_deferred_multiplications;
        self.total_fee = self.current_call.snap_total_fee;
        // Restore actor registry state (F1).
        if let Some(r) = registry.as_mut() {
            r.pop_checkpoint_rollback();
        }
        // Restore the delegate's MSM/sig batch to its pre-call state
        // so any non-identity statements the failed callee appended
        // are discarded. Only in external context — internal-mode
        // delegates have no real batch (snapshot was never taken).
        if let Some(snap) = self.current_call.snap_batch.take() {
            use musig::BatchCheckpoint;
            delegate.batch_verifier().restore(&snap);
        }
        // Restore the delegate's R1CS state too — drops the
        // witness/constraint vectors and rewinds the Fiat–Shamir
        // transcript so the proof binds only to the parent's CS.
        if let Some(snap) = self.current_call.snap_cs.take() {
            use r1cs::CheckpointableConstraintSystem;
            delegate.cs().rollback(snap);
        }
        // Apply parent's post-call anchor (caller's right half of
        // the entry split — independent of whatever the callee did
        // with its left half).
        if let Some(post) = self.current_call.post_call_anchor.take() {
            self.last_anchor = Some(post);
        }
        self.push_failed_values(failure_values, failure_arg_count);
    }

    /// Pushes a value onto the current call's stack.
    fn push_value(&mut self, v: Value) {
        self.current_call.stack.push(v);
    }

    /// Failure shape shared by synchronous calls: restored entry values,
    /// the explicit argument count, then the zero status marker. A restored
    /// Contract is contextual and is not included in `arg_count`.
    fn push_failed_values(&mut self, values: Vec<Value>, arg_count: usize) {
        self.current_call.stack.extend(values);
        self.current_call
            .stack
            .push(Value::Scalar(Scalar::from(arg_count as u64)));
        self.current_call.stack.push(Value::Scalar(Scalar::ZERO));
    }

    fn charge_clone_values(&mut self, values: &[Value]) -> Result<(), VMError> {
        let gas = values
            .iter()
            .fold(0u64, |gas, value| gas.saturating_add(value.clone_gas()));
        self.current_call.charge_gas(gas)
    }

    /// Charge bounded serialization work before an opcode constructs Cell data.
    /// Copies includes subsequent effect/identity encoding of an admitted value.
    fn charge_value_encoding(&mut self, value: &Value, copies: u64) -> Result<(), VMError> {
        self.current_call.charge_gas(
            value
                .encoding_gas()?
                .checked_mul(copies)
                .ok_or(VMError::OutOfGas)?,
        )
    }

    fn charge_top_value_growth(&mut self, n: usize) -> Result<(), VMError> {
        let len = self.current_call.stack.len();
        if len < n {
            return Err(VMError::StackUnderflow);
        }
        let gas = self.current_call.stack[len - n..]
            .iter()
            .fold(0u64, |gas, value| gas.saturating_add(value.clone_gas()));
        self.current_call.charge_gas(gas)
    }

    /// Pops the top value from the current call's stack.
    fn pop_value(&mut self) -> Result<Value, VMError> {
        self.current_call.stack.pop().ok_or(VMError::StackUnderflow)
    }

    /// Converts a stack-popped `Scalar` index into a `usize`.
    fn scalar_to_stack_index(&self, i: Scalar) -> Result<usize, VMError> {
        let v = i.to_u64().ok_or(VMError::IndexOutOfRange)?;
        let idx = usize::try_from(v).map_err(|_| VMError::IndexOutOfRange)?;
        if idx >= self.current_call.stack.len() {
            return Err(VMError::IndexOutOfRange);
        }
        Ok(idx)
    }

    /// `pushtoken` — `flv → token`. Pops a `Scalar` flavor from
    /// the stack and pushes a zero-qty `ClearToken { qty: 0, flv }`. This
    /// is the canonical "empty bearer of a flavor" used as a starting
    /// point for issuance / borrow flows.
    fn op_pushtoken(&mut self) -> Result<(), VMError> {
        let flv = self.pop_value()?.to_scalar()?;
        self.push_value(Value::ClearToken(ClearToken::new(Scalar::ZERO, flv)));
        Ok(())
    }

    /// `drop` — pops the top and discards it if droppable.
    fn op_drop(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?;
        if !v.is_droppable() {
            // Put it back — failed drop must not silently consume the value
            // (matters if the caller catches the error and continues).
            self.push_value(v);
            return Err(VMError::TypeNotDroppable);
        }
        Ok(())
    }

    /// `dup` — pops `k`, then copies the now-`k`-th item from top
    /// onto the top.
    fn op_dup(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_scalar()?;
        self.op_dup_k(self.scalar_to_stack_index(k)?)
    }

    /// `dup:k` — copies `stack[top - k]` onto the top.
    /// Requires the source value to be copyable.
    fn op_dup_k(&mut self, k: usize) -> Result<(), VMError> {
        let len = self.current_call.stack.len();
        if k >= len {
            return Err(VMError::IndexOutOfRange);
        }
        let idx = len - 1 - k;
        let bytes = match &self.current_call.stack[idx] {
            Value::String(s) => s.len(),
            _ => 0,
        };
        self.charge_alloc_bytes(bytes)?;
        let copy = self.current_call.stack[idx].try_clone()?;
        self.push_value(copy);
        Ok(())
    }

    /// `roll` — pops `k`, then moves the `k`-th item from top to
    /// the top.
    fn op_roll(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_scalar()?;
        self.op_roll_k(self.scalar_to_stack_index(k)?)
    }

    /// `roll:k` — removes `stack[top - k]` and pushes it
    /// back as the new top. `roll:0` is a no-op.
    fn op_roll_k(&mut self, k: usize) -> Result<(), VMError> {
        let stack = &mut self.current_call.stack;
        if k >= stack.len() {
            return Err(VMError::IndexOutOfRange);
        }
        let idx = stack.len() - 1 - k;
        let v = stack.remove(idx);
        stack.push(v);
        Ok(())
    }

    /// `transcript` — `label → merlin`. Pops a label string,
    /// creates a fresh Merlin transcript bound to it.
    fn op_transcript(&mut self) -> Result<(), VMError> {
        let label = self.pop_string()?;
        self.charge_alloc_bytes(label.len())?;
        self.push_value(Value::Merlin(Merlin::new(&label.to_bytes())));
        Ok(())
    }

    /// `twrite` — `merlin label str → merlin`. Pops `str` (top),
    /// `label`, and the merlin; absorbs `(label, str)` into the
    /// transcript; pushes merlin back.
    fn op_twrite(&mut self) -> Result<(), VMError> {
        let data = self.pop_string()?;
        let label = self.pop_string()?;
        self.charge_alloc_bytes(
            label
                .len()
                .checked_add(data.len())
                .ok_or(VMError::OutOfGas)?,
        )?;
        let mut m = self.pop_value()?.to_merlin()?;
        m.write_bytes(&label.to_bytes(), &data.to_bytes());
        self.push_value(Value::Merlin(m));
        Ok(())
    }

    /// `tread` — `merlin label n → merlin str`. Squeezes `n`
    /// bytes of challenge from the transcript under `label`; pushes the
    /// merlin back, then the new String.
    fn op_tread(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        String::check_length(n)?;
        self.charge_alloc_bytes(n)?;
        let label = self.pop_string()?;
        self.charge_alloc_bytes(label.len())?;
        let mut m = self.pop_value()?.to_merlin()?;
        let out = m.read_bytes(&label.to_bytes(), n);
        self.push_value(Value::Merlin(m));
        self.push_value(Value::String(String::from(out)));
        Ok(())
    }

    /// Hashes source payload bytes and appends the digest to the provided Builder.
    fn op_hash<H: sha2::Digest>(&mut self, block_bytes: usize) -> Result<(), VMError> {
        let mut builder = self.pop_value()?.into_builder()?;
        let source = self.pop_byte_source()?;
        let length = <H as sha2::Digest>::output_size();
        if length > builder.remaining_bytes() {
            return Err(CellError::PayloadCapacity.into());
        }
        self.current_call
            .charge_gas(hash_gas(source.byte_payload()?.len(), block_bytes)?)?;
        self.charge_alloc_bytes(length)?;
        let digest = H::digest(source.byte_payload()?);
        builder.store_bytes(&digest)?;
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    /// Resolve plain Cell bytes in the current scope; preserve owned Slice/Builder data.
    fn pop_byte_source(&mut self) -> Result<Value, VMError> {
        let source = match self.pop_value()? {
            Value::Cell(reference) => {
                let cell = cells::read_cell(&mut self.resolver(), &reference)?;
                Value::Slice(crate::Slice::new(cell)?)
            }
            value @ (Value::Slice(_) | Value::Builder(_)) => value,
            _ => return Err(VMError::TypeNotByteSource),
        };
        if source.byte_refs()? != 0 {
            return Err(CellError::InvalidFormat.into());
        }
        Ok(source)
    }

    /// Append a cached hash at an explicit level, without loading the Cell body.
    fn op_cell_hash(&mut self) -> Result<(), VMError> {
        let mut builder = self.pop_value()?.into_builder()?;
        let level = self.pop_byte_count(cells::MAX_CELL_LEVEL as usize)? as u8;
        let reference = self.pop_value()?.into_cell_ref()?;
        if builder.remaining_bytes() < 32 {
            return Err(CellError::PayloadCapacity.into());
        }
        self.charge_alloc_bytes(32)?;
        builder.store_bytes(&reference.commitment().hash(level)?)?;
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    /// `log` — `str | cell → ø`. A Cell must be a payload-only byte container.
    /// Emits
    /// `TxEntry::Data(bytes)` into the txlog. Witness-bearing String
    /// variants serialize via `to_bytes` so prover and verifier emit
    /// the same canonical bytes.
    fn op_log(&mut self) -> Result<(), VMError> {
        let bytes = match self.pop_value()? {
            Value::Cell(reference) => {
                let cell = cells::read_cell(&mut self.resolver(), &reference)?;
                if cell.is_pruned() {
                    return Err(CellError::PrunedCell.into());
                }
                if !cell.refs().is_empty() {
                    return Err(CellError::InvalidFormat.into());
                }
                self.charge_alloc_bytes(cell.payload().len())?;
                cell.payload().to_vec()
            }
            value => {
                let string = value.to_string()?;
                self.charge_alloc_bytes(string.len())?;
                string.to_bytes()
            }
        };
        self.txlog.push(TxEntry::Data(bytes));
        Ok(())
    }

    /// `dict` — `... val key val key n → dict`. Pops `n`, then `n`
    /// key/value pairs (key on top of each pair). Duplicate keys error.
    /// Each successful insertion updates the Dict's sticky capability
    /// flags; non-portable values are allowed until a storage or transfer
    /// boundary is crossed.
    fn op_dict(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        self.charge_alloc_items(n)?;
        let mut dict = Dict::new();
        for _ in 0..n {
            let key = self.pop_value()?.to_scalar()?;
            let value = self.pop_value()?;
            self.charge_value_encoding(&value, 1)?;
            if dict
                .insert_strict_resolved(key, value, &mut self.resolver())
                .map_err(|(error, _value)| VMError::from(error))?
                .is_err()
            {
                return Err(VMError::DictKeyOccupied);
            }
        }
        self.push_value(Value::Dict(dict));
        Ok(())
    }

    /// `put` — `dict k v → dict'`. Strict insert; fails on
    /// occupied key.
    fn op_put(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?;
        let k = self.pop_value()?.to_scalar()?;
        let mut dict = self.pop_value()?.to_dict()?;
        self.charge_alloc_items(1)?;
        self.charge_value_encoding(&v, 1)?;
        if dict
            .insert_strict_resolved(k, v, &mut self.resolver())
            .map_err(|(error, _value)| VMError::from(error))?
            .is_err()
        {
            return Err(VMError::DictKeyOccupied);
        }
        self.push_value(Value::Dict(dict));
        Ok(())
    }

    /// `replace` — `dict k v → dict' {prev 1 | 0}`. Overwrites
    /// the slot, returning the prior value (if any) as an optional.
    /// Stack order matches `put`: `v` on top, `k` below.
    fn op_replace(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?;
        let k = self.pop_value()?.to_scalar()?;
        let mut dict = self.pop_value()?.to_dict()?;
        self.charge_alloc_items(1)?;
        self.charge_value_encoding(&v, 1)?;
        let prev = dict
            .insert_resolved(k, v, &mut self.resolver())
            .map_err(|(error, _value)| VMError::from(error))?;
        let prev = prev
            .map(|value| self.restore_private_value(value))
            .transpose()?;
        self.push_value(Value::Dict(dict));
        match prev {
            Some(prev_v) => {
                self.push_value(prev_v);
                self.push_value(Value::Scalar(Scalar::from(1u64)));
            }
            None => {
                self.push_value(Value::Scalar(Scalar::ZERO));
            }
        }
        Ok(())
    }

    /// `get` — `dict k → dict' k v`. Removes and returns the
    /// value at `k`. Fails if the key is missing.
    fn op_get(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_scalar()?;
        let mut dict = self.pop_value()?.to_dict()?;
        let v = dict
            .remove_resolved(&k, &mut self.resolver())?
            .ok_or(VMError::DictKeyNotFound)?;
        let v = self.restore_private_value(v)?;
        self.push_value(Value::Dict(dict));
        self.push_value(Value::Scalar(k));
        self.push_value(v);
        Ok(())
    }

    /// `getopt` — `dict k → dict' {v 1 | 0}`. Like `get`, but
    /// soft-fails (pushes `0`) when the key is missing.
    fn op_getopt(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_scalar()?;
        let mut dict = self.pop_value()?.to_dict()?;
        let v = dict.remove_resolved(&k, &mut self.resolver())?;
        let v = v
            .map(|value| self.restore_private_value(value))
            .transpose()?;
        self.push_value(Value::Dict(dict));
        self.push_optional_value(v);
        Ok(())
    }

    /// `getdup` — `dict k → dict {v 1 | 0}`. Copies the value
    /// without consuming it. Soft-fails with `0` on missing key; hard
    /// errors if the value exists but isn't copyable.
    fn op_getdup(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_scalar()?;
        let mut dict = self.pop_value()?.to_dict()?;
        let copied = match dict.get_resolved(&k, &mut self.resolver())? {
            Some(v) => {
                let bytes = match v {
                    Value::String(s) => s.len(),
                    _ => 0,
                };
                self.charge_alloc_bytes(bytes)?;
                Some(self.restore_private_value(v.try_clone()?)?)
            }
            None => None,
        };
        self.push_value(Value::Dict(dict));
        self.push_optional_value(copied);
        Ok(())
    }

    /// Dict-lookup epilogue: pushes `Some(v)` as `v, 1`; `None` as `0`.
    fn push_optional_value(&mut self, v: Option<Value>) {
        match v {
            Some(v) => {
                self.push_value(v);
                self.push_value(Value::Scalar(Scalar::from(1u64)));
            }
            None => self.push_value(Value::Scalar(Scalar::ZERO)),
        }
    }

    /// `first` — `dict → dict {k 1 | 0}`. Pushes the smallest key
    /// alongside a flag, or `0` if the dict is empty.
    fn op_first(&mut self) -> Result<(), VMError> {
        let dict = self.pop_value()?.to_dict()?;
        let k = dict.first_key_resolved(&mut self.resolver())?;
        self.push_value(Value::Dict(dict));
        self.push_optional_value(k.map(Value::Scalar));
        Ok(())
    }

    /// `last` — `dict → dict {k 1 | 0}`. Mirror of `first`.
    fn op_last(&mut self) -> Result<(), VMError> {
        let dict = self.pop_value()?.to_dict()?;
        let k = dict.last_key_resolved(&mut self.resolver())?;
        self.push_value(Value::Dict(dict));
        self.push_optional_value(k.map(Value::Scalar));
        Ok(())
    }

    /// `next` — `dict k → dict {k' 1 | 0}`. Smallest key strictly
    /// greater than `k`, or `0` if no such key exists.
    fn op_next(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_scalar()?;
        let dict = self.pop_value()?.to_dict()?;
        let next_k = dict.next_key_after_resolved(&k, &mut self.resolver())?;
        self.push_value(Value::Dict(dict));
        self.push_optional_value(next_k.map(Value::Scalar));
        Ok(())
    }

    /// Helper: converts a stack-popped count into a `usize` ≤ `max`.
    /// Returns `IndexOutOfRange` on overflow or above `max`.
    fn pop_byte_count(&mut self, max: usize) -> Result<usize, VMError> {
        let n_int = self.pop_value()?.to_scalar()?;
        let n_u64 = n_int.to_u64().ok_or(VMError::IndexOutOfRange)?;
        let n = usize::try_from(n_u64).map_err(|_| VMError::IndexOutOfRange)?;
        if n > max {
            return Err(VMError::IndexOutOfRange);
        }
        Ok(n)
    }

    /// Pushes a failure marker (`Scalar(0)`) — used by `read*` opcodes
    /// when the source string is too short to satisfy the request. The
    /// original string is restored under the marker.
    fn push_read_failure(&mut self, original: String) {
        self.push_value(Value::String(original));
        self.push_value(Value::Scalar(Scalar::ZERO));
    }

    fn op_slice(&mut self) -> Result<(), VMError> {
        let reference = self.pop_value()?.into_cell_ref()?;
        let cell = cells::read_cell(&mut self.resolver(), &reference)?;
        self.push_value(Value::Slice(crate::Slice::new(cell)?));
        Ok(())
    }

    fn op_endcell(&mut self) -> Result<(), VMError> {
        let builder = self.pop_value()?.into_builder()?;
        self.current_call
            .charge_gas(cell_hash_gas(builder.used_bytes(), builder.used_refs())?)?;
        self.push_value(Value::Cell(builder.build().into()));
        Ok(())
    }

    /// Short/noncanonical input leaves the cursor unchanged and returns zero.
    fn op_read_uint(&mut self, count: usize) -> Result<(), VMError> {
        let mut slice = self.pop_value()?.into_slice()?;
        let mut bytes = [0; 32];
        let value = if slice.remaining_bytes() >= count {
            bytes[..count].copy_from_slice(&slice.bytes()[..count]);
            Scalar::from_bytes(bytes)
        } else {
            None
        };
        if let Some(value) = value {
            slice.advance_bytes(count);
            self.push_value(Value::Slice(slice));
            self.push_value(Value::Scalar(value));
            self.push_value(Value::Scalar(Scalar::ONE));
        } else {
            self.push_value(Value::Slice(slice));
            self.push_value(Value::Scalar(Scalar::ZERO));
        }
        Ok(())
    }

    fn op_read_bytes(&mut self) -> Result<(), VMError> {
        let count = self.pop_byte_count(usize::MAX)?;
        let mut builder = self.pop_value()?.into_builder()?;
        let mut slice = self.pop_value()?.into_slice()?;
        if slice.remaining_bytes() < count {
            self.push_value(Value::Slice(slice));
            self.push_value(Value::Builder(builder));
            self.push_value(Value::Scalar(Scalar::ZERO));
            return Ok(());
        }
        if count > builder.remaining_bytes() {
            return Err(CellError::PayloadCapacity.into());
        }
        self.charge_alloc_bytes(count)?;
        builder.store_bytes(&slice.bytes()[..count])?;
        slice.advance_bytes(count);
        self.push_value(Value::Slice(slice));
        self.push_value(Value::Builder(builder));
        self.push_value(Value::Scalar(Scalar::ONE));
        Ok(())
    }

    /// `readpoint` — `s → s' point 1 | s 0`. Splits off the first
    /// 32 bytes of `s` as a `Point` (decompressability not validated
    /// here; later opcodes that consume the point may reject it).
    fn op_read_point(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        if s.len() < 32 {
            self.push_read_failure(s);
            return Ok(());
        }
        self.charge_alloc_bytes(s.len())?;
        let bytes = s.to_bytes();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes[..32]);
        let point = Point::from_bytes(arr);
        self.push_value(Value::String(String::from(bytes[32..].to_vec())));
        self.push_value(Value::Point(point));
        self.push_value(Value::Scalar(Scalar::from(1u64)));
        Ok(())
    }

    fn op_write_uint(&mut self, count: usize) -> Result<(), VMError> {
        let value = self.pop_value()?.to_scalar()?;
        let mut builder = self.pop_value()?.into_builder()?;
        if count > builder.remaining_bytes() {
            return Err(CellError::PayloadCapacity.into());
        }
        self.charge_alloc_bytes(count)?;
        builder.store_bytes(&value.to_bytes()[..count])?;
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    fn op_append_bytes(&mut self) -> Result<(), VMError> {
        let mut builder = self.pop_value()?.into_builder()?;
        let mut slice = self.pop_value()?.into_slice()?;
        let count = slice.remaining_bytes();
        if count > builder.remaining_bytes() {
            return Err(CellError::PayloadCapacity.into());
        }
        self.charge_alloc_bytes(count)?;
        builder.store_bytes(slice.bytes())?;
        slice.advance_bytes(count);
        self.push_value(Value::Slice(slice));
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    fn op_append_refs(&mut self) -> Result<(), VMError> {
        let mut builder = self.pop_value()?.into_builder()?;
        let mut slice = self.pop_value()?.into_slice()?;
        let count = slice.remaining_refs();
        if count > builder.remaining_refs() {
            return Err(CellError::ReferenceCapacity.into());
        }
        for reference in slice.refs() {
            reference.validate_child()?;
        }
        self.charge_alloc_items(count)?;
        for reference in slice.refs() {
            builder.store_ref(reference.clone())?;
        }
        slice.advance_refs(count);
        self.push_value(Value::Slice(slice));
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    /// Charges variable byte allocation before it occurs. Allocation gas is
    /// monotonic: freeing memory never refunds gas, so total charged growth
    /// bounds the frame's peak live heap without allocator-specific tracking.
    fn charge_alloc_bytes(&mut self, n: usize) -> Result<(), VMError> {
        self.current_call.charge_gas(alloc_byte_gas(n)?)
    }

    /// Charges variable-sized collection growth before reserving its items.
    fn charge_alloc_items(&mut self, n: usize) -> Result<(), VMError> {
        self.current_call.charge_gas(alloc_item_gas(n)?)
    }

    /// `writezeros` — `s n → s'`. Appends `n` zero bytes.
    fn op_write_zeros(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let s = self.pop_string()?;
        self.charge_alloc_bytes(s.appended_len(n)?)?;
        let appended = s.append_zeros(n)?;
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// In-place payload transform; references and payload length never change.
    fn op_bit_not(&mut self) -> Result<(), VMError> {
        let mut builder = self.pop_value()?.into_builder()?;
        self.current_call.charge_gas(builder.used_bytes() as u64)?;
        for byte in builder.payload_mut() {
            *byte = !*byte;
        }
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    fn op_bitwise(&mut self, operation: fn(u8, u8) -> u8) -> Result<(), VMError> {
        if matches!(self.current_call.stack.last(), Some(Value::Builder(_))) {
            return Err(VMError::TypeNotCellOrSlice);
        }
        let source = self.pop_byte_source()?;
        let mut builder = self.pop_value()?.into_builder()?;
        if builder.used_bytes() != source.byte_payload()?.len() {
            return Err(VMError::BitwiseSizeMismatch);
        }
        self.current_call.charge_gas(builder.used_bytes() as u64)?;
        for (byte, other) in builder.payload_mut().iter_mut().zip(source.byte_payload()?) {
            *byte = operation(*byte, *other);
        }
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    /// Fixed-width, big-endian payload shift. Displaced bits are discarded.
    fn op_shift(&mut self, left: bool) -> Result<(), VMError> {
        let count = self.pop_byte_count(256)?;
        let mut builder = self.pop_value()?.into_builder()?;
        self.current_call.charge_gas(builder.used_bytes() as u64)?;
        shift_bytes(builder.payload_mut(), count, left);
        self.push_value(Value::Builder(builder));
        Ok(())
    }

    /// `abs` — interprets a scalar in the centered interval `[-H, H]`,
    /// where `H = (ℓ - 1) / 2`, and pushes its magnitude followed by
    /// `0` for a non-negative value or `1` for a negative value.
    fn op_abs(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?.to_scalar()?;
        let sign = u64::from(v.is_centered_negative());
        self.push_value(Value::Scalar(v.centered_abs()));
        self.push_value(Value::Scalar(Scalar::from(sign)));
        Ok(())
    }

    /// _a b_ **eq** → _a b {0|1}_ (cleartext) | _a b_ **eq** → _constraint_ (CS)
    fn op_eq<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let n = self.current_call.stack.len();
        if n < 2 {
            return Err(VMError::StackUnderflow);
        }
        // CS-lift only when an operand is actually a CS type (Variable /
        // Expression). A "non-Scalar" test would wrongly route String /
        // Point comparisons (e.g. the `anchor … eq verify` binding idiom)
        // into `to_expression()` and hard-fail in external context.
        let cs_involved = matches!(
            self.current_call.stack[n - 1],
            Value::Variable(_) | Value::Expression(_)
        ) || matches!(
            self.current_call.stack[n - 2],
            Value::Variable(_) | Value::Expression(_)
        );
        if self.is_external() && cs_involved {
            self.charge_top_value_growth(2)?;
            let b = self.pop_value()?.to_expression()?;
            let a = self.pop_value()?.to_expression()?;
            self.push_value(Value::Constraint(Constraint::eq(a, b)));
        } else {
            if self.current_call.stack[n - 1].is_cell_container()
                && self.current_call.stack[n - 2].is_cell_container()
            {
                let gas = factual_hash_gas(&self.current_call.stack[n - 1])?
                    .checked_add(factual_hash_gas(&self.current_call.stack[n - 2])?)
                    .ok_or(VMError::OutOfGas)?;
                self.current_call.charge_gas(gas)?;
            } else {
                self.charge_top_value_growth(2)?;
            }
            let eq = self.current_call.stack[n - 1].try_eq(&self.current_call.stack[n - 2])?;
            let bit = if eq { 1u64 } else { 0u64 };
            self.push_value(Value::Scalar(Scalar::from(bit)));
        }
        Ok(())
    }

    /// _x_ **neg** → _-x_  (Scalar cleartext or Expression LC negate)
    fn op_neg<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        self.charge_top_value_growth(1)?;
        let v = self.pop_value()?.neg()?;
        self.push_value(v);
        Ok(())
    }

    /// _x y_ **add** → _z_  (cleartext modulo ℓ, or LC sum)
    fn op_add<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        self.charge_top_value_growth(2)?;
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        let r = a.add(b, self.is_external())?;
        self.push_value(r);
        Ok(())
    }

    /// _x y_ **mul** → _z_  (cleartext modulo ℓ, MSM scalar-point lift,
    /// or CS multiplier)
    fn op_mul<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        use crate::msm::MultiscalarMul;
        self.charge_top_value_growth(2)?;
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        match (a, b) {
            (Value::Scalar(x), Value::Scalar(y)) => {
                self.push_value(Value::Scalar(x * y));
                Ok(())
            }
            // scalar * point / point * scalar → MSM with one term.
            (Value::Scalar(s), Value::Point(p)) | (Value::Point(p), Value::Scalar(s)) => {
                self.push_value(Value::MultiscalarMul(MultiscalarMul::term(
                    s.to_dalek(),
                    p.to_compressed(),
                )));
                Ok(())
            }
            // scalar * MSM / MSM * scalar → scale coefficients.
            (Value::Scalar(s), Value::MultiscalarMul(m))
            | (Value::MultiscalarMul(m), Value::Scalar(s)) => {
                self.push_value(Value::MultiscalarMul(m.scaled(s.to_dalek())));
                Ok(())
            }
            // Quadratic group-element products are not defined in
            // Sigma-protocol semantics.
            (Value::Point(_), Value::Point(_))
            | (Value::Point(_), Value::MultiscalarMul(_))
            | (Value::MultiscalarMul(_), Value::Point(_))
            | (Value::MultiscalarMul(_), Value::MultiscalarMul(_)) => Err(VMError::TypeNotScalar),
            (a, b) if self.is_external() => {
                let aexpr = a.to_expression()?;
                let bexpr = b.to_expression()?;
                if matches!(&aexpr, Expression::LinearCombination(_, _))
                    && matches!(&bexpr, Expression::LinearCombination(_, _))
                {
                    self.current_call.charge_gas(r1cs_gas(1)?)?;
                }
                let product = aexpr.multiply(bexpr, delegate.cs());
                self.push_value(Value::Expression(product));
                Ok(())
            }
            _ => Err(VMError::TypeNotScalar),
        }
    }

    /// `divmod` — `x z → d r`. Centered integer division, truncating
    /// toward zero: `x = d*z + r` over integers, `|r| < |z|`, and a
    /// nonzero remainder has the dividend's sign. Errors on zero divisor.
    fn op_divmod(&mut self) -> Result<(), VMError> {
        let z = self.pop_value()?.to_scalar()?;
        let x = self.pop_value()?.to_scalar()?;
        let (d, r) = x.div_rem(z).ok_or(VMError::DivByZero)?;
        self.push_value(Value::Scalar(d));
        self.push_value(Value::Scalar(r));
        Ok(())
    }

    /// `mod252` — pops a `String` of 0..=64 bytes, interprets it
    /// as a little-endian unsigned integer, reduces it modulo ℓ, and
    /// pushes the canonical `Scalar` residue.
    fn op_mod252(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        let bytes = s.to_bytes();
        if bytes.len() > 64 {
            return Err(VMError::StringTooLongForModReduction);
        }
        let mut buf = [0u8; 64];
        buf[..bytes.len()].copy_from_slice(&bytes);
        let scalar = DalekScalar::from_bytes_mod_order_wide(&buf);
        self.push_value(Value::Scalar(Scalar::from(scalar)));
        Ok(())
    }

    /// _x_ **not** → _y_  (Scalar logical, or Constraint negation)
    fn op_not<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        self.charge_top_value_growth(1)?;
        let v = self.pop_value()?.not()?;
        self.push_value(v);
        Ok(())
    }

    /// _a b_ **and** → _c_  (Scalar logical, or Constraint conjunction)
    fn op_and<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        self.charge_top_value_growth(2)?;
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        let r = a.and(b, self.is_external())?;
        self.push_value(r);
        Ok(())
    }

    /// _a b_ **or** → _c_  (Scalar logical, or Constraint disjunction)
    fn op_or<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        self.charge_top_value_growth(2)?;
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        let r = a.or(b, self.is_external())?;
        self.push_value(r);
        Ok(())
    }

    /// `size` — peeks the top value and pushes its length as an
    /// `Scalar` (Cell/Slice/Builder byte count, Dict entry count). Other types
    /// error with `TypeHasNoLength`.
    fn op_size(&mut self) -> Result<(), VMError> {
        if matches!(self.current_call.stack.last(), Some(Value::Cell(_))) {
            let reference = self
                .current_call
                .stack
                .last()
                .unwrap()
                .clone()
                .into_cell_ref()?;
            let cell = cells::read_cell(&mut self.resolver(), &reference)?;
            if cell.is_pruned() {
                return Err(CellError::PrunedCell.into());
            }
            self.push_value(Value::Scalar(Scalar::from(cell.payload().len() as u64)));
            return Ok(());
        }
        let top = self
            .current_call
            .stack
            .last()
            .ok_or(VMError::StackUnderflow)?;
        let len = match top {
            Value::Dict(d) => d.len(),
            Value::Slice(s) => s.remaining_bytes(),
            Value::Builder(b) => b.used_bytes(),
            _ => return Err(VMError::TypeHasNoLength),
        };
        self.push_value(Value::Scalar(Scalar::from(len as u64)));
        Ok(())
    }

    /// `verify` — pop one and assert truthiness.
    ///
    /// - `Scalar`: errors `VerifyFailed` if zero, else pops.
    /// - `Constraint`: hands the constraint to the CS so the proof
    ///   commits to its truth. Requires external context.
    /// - `MultiscalarMul`: appends `sum(s_i · P_i) == identity` to
    ///   the delegate's `BatchVerifier` (alongside Schnorr/Musig
    ///   sigs). Requires external context.
    fn op_verify<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.charge_top_value_growth(1)?;
        match self.pop_value()? {
            Value::Scalar(v) => {
                if v.is_zero() {
                    return Err(VMError::VerifyFailed);
                }
                Ok(())
            }
            Value::Constraint(c) => {
                self.require_external()?;
                let multiplications = c.multiplier_count();
                self.current_call.charge_gas(r1cs_gas(multiplications)?)?;
                c.verify(delegate.cs())?;
                self.deferred_multiplications = self
                    .deferred_multiplications
                    .saturating_add(multiplications);
                Ok(())
            }
            Value::MultiscalarMul(m) => {
                self.require_external()?;
                let terms = m.into_terms();
                self.current_call.charge_gas(linear_gas(
                    GAS_MSM_VERIFY_BASE,
                    GAS_MSM_VERIFY_TERM,
                    terms.len(),
                )?)?;
                self.charge_alloc_items(terms.len().saturating_mul(2))?;
                let scalars: Vec<curve25519_dalek::scalar::Scalar> =
                    terms.iter().map(|(s, _)| *s).collect();
                let points: Vec<Option<curve25519_dalek::ristretto::RistrettoPoint>> =
                    terms.iter().map(|(_, p)| p.decompress()).collect();
                // basepoint_scalar = 0: no contribution from the
                // basepoint; the entire MSM must sum to identity.
                // The BatchVerifier multiplies the whole statement
                // by a fresh random scalar (RNG-based, owned by the
                // delegate) so unrelated batched statements can't
                // cancel each other. Per-frame rollback is achieved
                // not by batching per-frame, but by snapshotting the
                // delegate's batch on call entry (see `step`) and
                // restoring on call failure (see `fail_current_call`).
                musig::BatchVerification::append(
                    delegate.batch_verifier(),
                    curve25519_dalek::scalar::Scalar::ZERO,
                    scalars,
                    points,
                );
                Ok(())
            }
            other => {
                self.push_value(other);
                Err(VMError::TypeNotScalar)
            }
        }
    }

    /// _prog_ **run** → _results…_
    /// **label:n** → ø — record a jump target. See ADR 0015.
    fn op_label(&mut self, n: u32) -> Result<(), VMError> {
        self.current_call.record_label(n as usize)
    }

    /// **jump:n** → ø — unconditional jump to label `n`.
    fn op_jump(&mut self, n: u32) -> Result<(), VMError> {
        self.current_call.jump_to_label(n as usize)
    }

    /// _x_ **jumpif:n** → ø — pop an Scalar; jump to label `n` iff `x ≠ 0`.
    fn op_jumpif(&mut self, n: u32) -> Result<(), VMError> {
        let x = self.pop_value()?.to_scalar()?;
        if x.is_zero() {
            Ok(())
        } else {
            self.current_call.jump_to_label(n as usize)
        }
    }

    /// _x(k-1) … x(0) k_ **return** → ø
    ///
    /// Pops the frame, refunds leftover gas, pushes the `k` items
    /// then `k` then `1` (success marker) onto the caller's stack.
    /// Stack on parent after the call: `… values… k 1` (top = 1).
    /// Errors `ReturnAtRoot` at the outermost frame.
    fn op_return(&mut self) -> Result<(), VMError> {
        let k_int = self.pop_value()?.to_scalar()?;
        let k_u64 = k_int.to_u64().ok_or(VMError::BadReturnArity)?;
        let k = usize::try_from(k_u64).map_err(|_| VMError::BadReturnArity)?;

        // Outermost frame: `return` has no parent. Fail regardless of `k`.
        if self.call_stack.is_empty() {
            return Err(VMError::ReturnAtRoot);
        }

        // Strict: exactly `k` items must remain. Fewer → arity mismatch,
        // more → leftover state the script forgot about.
        if self.current_call.stack.len() < k {
            return Err(VMError::BadReturnArity);
        }
        if self.current_call.stack.len() > k {
            return Err(VMError::StackNotClean);
        }

        let return_values: Vec<Value> = self.current_call.stack.drain(..).collect();
        let leftover_gas = self
            .current_call
            .gas_limit
            .saturating_sub(self.current_call.gas_used);

        let parent = self.call_stack.pop().expect("checked non-empty above");
        self.current_call = parent;
        self.clean_exit_to_parent(leftover_gas);
        // Pour return values, then count, then success marker (1).
        self.current_call.stack.extend(return_values);
        self.current_call
            .stack
            .push(Value::Scalar(Scalar::from(k as u64)));
        self.current_call.stack.push(Value::Scalar(Scalar::ONE));
        Ok(())
    }

    /// `type` — peeks the top value and pushes its type code as an
    /// `Scalar`. The original value remains on the stack underneath.
    fn op_type(&mut self) -> Result<(), VMError> {
        let code = self
            .current_call
            .stack
            .last()
            .ok_or(VMError::StackUnderflow)?
            .type_code();
        self.push_value(Value::Scalar(Scalar::from(code as u64)));
        Ok(())
    }

    /// Transitional adapter for the byte consumers not migrated in this step.
    fn pop_string(&mut self) -> Result<String, VMError> {
        match self.pop_value()? {
            Value::String(string) => {
                string.check_len()?;
                Ok(string)
            }
            Value::Cell(reference) => {
                let cell = cells::read_cell(&mut self.resolver(), &reference)?;
                if cell.is_pruned() {
                    return Err(CellError::PrunedCell.into());
                }
                if !cell.refs().is_empty() {
                    return Err(CellError::InvalidFormat.into());
                }
                self.charge_alloc_bytes(cell.payload().len())?;
                Ok(reference
                    .witness
                    .as_ref()
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_else(|| String::from(cell.payload().to_vec())))
            }
            _ => Err(VMError::TypeNotString),
        }
    }

    fn pop_string_32(&mut self) -> Result<[u8; 32], VMError> {
        let s = self.pop_string()?;
        array32(&s.to_bytes()).ok_or(VMError::MalformedAddress)
    }

    /// Actor destinations accept a 32-byte hash or a canonical ActorID Cell
    /// envelope, including constructor code for deploy-on-first-delivery.
    fn pop_actor_id(&mut self) -> Result<ActorID, VMError> {
        let bytes = self.pop_string()?.to_bytes_vec();
        if let Some(hash) = array32(&bytes) {
            return Ok(ActorID::Hash(hash));
        }
        let envelope = cells::CellEnvelope::decode(&bytes, bytes.len(), &mut self.current_call)?;
        let root = envelope
            .cells()
            .get(&envelope.root())
            .ok_or(VMError::MalformedAddress)?;
        let mut resolver = ExecutionCells {
            external: envelope.cells(),
            actor: &self.current_call.actor_cells,
            gas_limit: self.current_call.gas_limit,
            gas_used: &mut self.current_call.gas_used,
        };
        ActorID::from_cell(&root, &mut resolver).map_err(VMError::from)
    }

    fn op_nop(&mut self) -> Result<(), VMError> {
        Ok(())
    }

    /// Returns the current call's actor identity, or
    /// `OpcodeRequiresActorContext` if the frame has none.
    fn require_actor(&self) -> Result<&ActorID, VMError> {
        self.current_call
            .kind
            .actor()
            .ok_or(VMError::OpcodeRequiresActorContext)
    }

    /// _token_ **amount** → _token qty flv_
    fn op_amount(&mut self) -> Result<(), VMError> {
        let token = self.pop_value()?;
        match token {
            Value::ClearToken(t) => {
                let qty = t.qty();
                let flv = t.flv();
                self.push_value(Value::ClearToken(t));
                self.push_value(Value::Scalar(qty));
                self.push_value(Value::Scalar(flv));
                Ok(())
            }
            Value::Token(t) => {
                let qty_pt = Point::from_compressed(t.qty.to_point());
                let flv_pt = Point::from_compressed(t.flv.to_point());
                self.push_value(Value::Token(t));
                self.push_value(Value::Point(qty_pt));
                self.push_value(Value::Point(flv_pt));
                Ok(())
            }
            other => {
                self.push_value(other);
                Err(VMError::TypeNotToken)
            }
        }
    }

    /// _qty:Scalar tag_ **issuepub** → _CT_
    ///
    /// Cleartext mint under the enclosing actor's identity. Requires
    /// an internal actor frame; `ExternalRoot` and `ContractOpen` error
    /// `OpcodeRequiresActorContext` (issuance domains are disjoint —
    /// see spec.md §issuepub). Non-`Scalar` qty hard-fails
    /// `TypeNotScalar`; the confidential path lives in [`op_issuepriv`].
    fn op_issuepub(&mut self) -> Result<(), VMError> {
        let actor = self.require_actor()?.clone();
        let tag = self.pop_string()?;
        let qty = match self.pop_value()? {
            Value::Scalar(i) => i,
            _ => return Err(VMError::TypeNotScalar),
        };
        let flv = flavor_from_actor(&actor, &tag);
        // Cleartext qty + flv go straight into the txlog as `Scalar`s —
        // no commitment indirection. The `IssuePub` entry is publicly
        // auditable directly on the wire.
        self.txlog.push(TxEntry::IssuePub(qty, flv));
        self.push_value(Value::ClearToken(ClearToken::new(qty, flv)));
        Ok(())
    }

    /// _qty:Variable tag_ **issuepriv** → _T_
    ///
    /// Confidential mint under the enclosing predicate's identity.
    /// Requires a `CallKind::ContractOpen` frame (errors
    /// `OpcodeRequiresPredicateContext` otherwise) AND external
    /// context (errors `ExternalOnly`; the CS lane is needed for the
    /// range proof and the qty commitment registration).
    fn op_issuepriv<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        // Snapshot the predicate before any pop, so a wrong frame
        // surfaces before we mutate the stack.
        let predicate = match &self.current_call.kind {
            CallKind::ContractOpen { predicate, .. } => predicate.clone(),
            _ => return Err(VMError::OpcodeRequiresPredicateContext),
        };
        self.require_external()?;
        use spacesuit::BitRange;
        // One committed variable plus a 64-bit range gadget.
        self.current_call.charge_gas(r1cs_gas(65)?)?;

        let tag = self.pop_string()?;
        let qty_var = self.pop_value()?.to_variable()?;
        // Register the Pedersen commitment with the CS. On the prover
        // the open assignment travels through; on the verifier only
        // the closed point. Same r1cs::Variable on both sides.
        let (_qty_point, qty_r1cs) = delegate.commit_variable(&qty_var.commitment)?;
        // 64-bit range proof on the qty commitment — `borrow`/`mix`
        // already enforce this for confidential token paths.
        let qty_assignment = match qty_var.commitment.assignment() {
            Some(i) => Some(scalar_to_signed_integer(i)?),
            None => None,
        };
        spacesuit::range_proof(
            delegate.cs(),
            qty_r1cs.into(),
            qty_assignment,
            BitRange::max(),
        )
        .map_err(VMError::R1CSError)?;

        // Flavor binds to the enclosing predicate + tag.
        let flv = flavor_from_predicate(&predicate, &tag);
        let flv_commit = Commitment::unblinded(flv);

        self.txlog.push(TxEntry::IssuePriv(
            qty_var.commitment.to_point(),
            flv_commit.to_point(),
        ));
        self.push_value(Value::Token(Token::new(qty_var.commitment, flv_commit)));
        Ok(())
    }

    /// _token_ **retire** → ø
    fn op_retire(&mut self) -> Result<(), VMError> {
        let val = self.pop_value()?;
        match val {
            Value::ClearToken(t) => {
                if !t.is_portable() {
                    self.push_value(Value::ClearToken(t));
                    return Err(VMError::NegativeTokenRetirement);
                }
                let qty_commit = Commitment::unblinded(t.qty());
                let flv_commit = Commitment::unblinded(t.flv());
                self.txlog.push(TxEntry::Retire(
                    qty_commit.to_point(),
                    flv_commit.to_point(),
                ));
                Ok(())
            }
            Value::Token(t) => {
                self.txlog
                    .push(TxEntry::Retire(t.qty.to_point(), t.flv.to_point()));
                Ok(())
            }
            other => {
                self.push_value(other);
                Err(VMError::TypeNotToken)
            }
        }
    }

    /// _qty flv_ **borrow** → _widetoken token_
    ///
    /// Cleartext branch for `Scalar` operands; encrypted (`Variable`)
    /// branch for CS-bound operands.
    fn op_borrow<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let flv_val = self.pop_value()?;
        let qty_v = self.pop_value()?;
        match (qty_v, flv_val) {
            (Value::Scalar(qty), Value::Scalar(flv)) => {
                let pos = ClearToken::new(qty, flv);
                let neg = pos.negated();
                self.push_value(Value::ClearToken(neg));
                self.push_value(Value::ClearToken(pos));
                Ok(())
            }
            (Value::Variable(qty), Value::Variable(flv)) => {
                self.require_external()?;
                self.op_borrow_encrypted_inner(qty, flv, delegate)
            }
            (Value::Point(_), _) | (_, Value::Point(_)) => Err(VMError::TokenRequiresCS),
            _ => Err(VMError::TypeNotScalar),
        }
    }

    /// _a b_ **merge** → _{c 1 | a b 0}_
    fn op_merge(&mut self) -> Result<(), VMError> {
        let b = self.pop_value()?.to_clear_token()?;
        let a = self.pop_value()?.to_clear_token()?;
        match a.merge_into(b) {
            Ok(c) => {
                self.push_value(Value::ClearToken(c));
                self.push_value(Value::Scalar(Scalar::from(1u64)));
            }
            Err((a, b)) => {
                self.push_value(Value::ClearToken(a));
                self.push_value(Value::ClearToken(b));
                self.push_value(Value::Scalar(Scalar::ZERO));
            }
        }
        Ok(())
    }

    /// _a q_ **split** → _a' b_
    fn op_split(&mut self) -> Result<(), VMError> {
        let q = self.pop_value()?.to_scalar()?;
        let a = self.pop_value()?.to_clear_token()?;
        match a.split(q) {
            Some((remainder, new_token)) => {
                self.push_value(Value::ClearToken(remainder));
                self.push_value(Value::ClearToken(new_token));
                Ok(())
            }
            None => Err(VMError::TokenSplitOutOfRange),
        }
    }

    /// _cid tag_ **issuepubflv** → _int_
    ///
    /// Consumer-side helper for `issuepub`: pops `tag` (String) and
    /// `cid` (String, exactly 32 bytes — an actor id), pushes
    /// `flavor_from_actor(cid, tag)` as `Scalar`. Pure helper: no CS,
    /// no txlog effect, no actor-context requirement. Use to
    /// recompute and verify a cleartext token's flavor without
    /// running the corresponding `issuepub`.
    fn op_issuepubflv(&mut self) -> Result<(), VMError> {
        let tag = self.pop_string()?;
        let cid = self.pop_string()?;
        let bytes = array32(&cid.to_bytes()).ok_or(VMError::IndexOutOfRange)?;
        let flv = flavor_from_actor(&ActorID::Hash(bytes), &tag);
        self.push_value(Value::Scalar(flv));
        Ok(())
    }

    /// _pred tag_ **issueprivflv** → _int_
    ///
    /// Consumer-side helper for `issuepriv`: pops `tag` (String) and
    /// `pred` (String, exactly 32 bytes — a compressed Ristretto
    /// predicate point), pushes `flavor_from_predicate(pred, tag)` as
    /// `Scalar`. Pure helper: no CS, no txlog effect, no
    /// predicate-context requirement. Use to recompute and verify a
    /// confidential token's flavor (the flv commitment is unblinded,
    /// so its point determines the scalar).
    fn op_issueprivflv(&mut self) -> Result<(), VMError> {
        let tag = self.pop_string()?;
        let pred = self.pop_string()?;
        let bytes = array32(&pred.to_bytes()).ok_or(VMError::IndexOutOfRange)?;
        let predicate = Predicate::opaque(curve25519_dalek::ristretto::CompressedRistretto(bytes));
        let flv = flavor_from_predicate(&predicate, &tag);
        self.push_value(Value::Scalar(flv));
        Ok(())
    }

    /// Pops `n` values from the stack and returns them in caller-pushed
    /// order (deepest first). No type or portability check.
    fn pop_n_values(&mut self, n: usize) -> Result<Vec<Value>, VMError> {
        if self.current_call.stack.len() < n {
            return Err(VMError::StackUnderflow);
        }
        self.charge_alloc_items(n)?;
        let start = self.current_call.stack.len() - n;
        Ok(self.current_call.stack.drain(start..).collect())
    }

    /// Values may return upward in any form, but only portable values may be
    /// delegated downward into a child frame. The caller must resolve any
    /// outstanding loan before calling another actor or predicate.
    fn require_portable_call_args(args: &[Value]) -> Result<(), VMError> {
        if args.iter().any(|value| !value.is_portable()) {
            return Err(VMError::NonPortableInCall);
        }
        Ok(())
    }

    /// Merlin message for `signcall`: binds the signature to the
    /// program bytes only. Programs add further context (anchor,
    /// actor identity) via explicit checks inside their script.
    fn signcall_message(program: &[u8]) -> Vec<u8> {
        let mut t = Transcript::new(b"flamevm.signcall");
        t.append_message(b"program", program);
        let mut out = vec![0u8; 32];
        t.challenge_bytes(b"msg", &mut out);
        out
    }

    /// _contract-id:string32_ **input** → _contract_
    ///
    /// The public body must resolve in the frozen transaction Cell hierarchy. Private
    /// witnesses may replace its typed values only after matching this identity.
    fn op_input(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let encoded = self.pop_string()?;
        let id = array32(&encoded.to_bytes()).ok_or(VMError::MalformedContractEncoding)?;
        let cell = cells::resolve_cell(&mut self.resolver(), id)?;
        let public = Contract::from_trusted_cell(&cell, &mut self.resolver())?;
        self.charge_value_encoding(public.payload(), 3)?;
        if public.id() != id {
            return Err(VMError::MalformedContractEncoding);
        }
        if let Some(witness) = self.contract_witnesses.get(&id) {
            if witness.id() != id {
                return Err(VMError::MalformedContractEncoding);
            }
            let mut pending = vec![witness.payload()];
            while let Some(value) = pending.pop() {
                match value {
                    Value::Dict(dict) => pending.extend(dict.cached_values()),
                    other => {
                        self.value_witnesses
                            .insert(other.to_cell()?.id(), other.clone());
                    }
                }
            }
        }
        let predicate = public.predicate.clone();
        let anchor = public.anchor;
        let payload = self.restore_private_value(public.into_payload())?;
        let contract = Contract::new(predicate, anchor, payload)?;
        self.last_anchor = Some(Anchor(id));
        self.txlog.push(TxEntry::Input(id));
        self.push_value(Value::Contract(Box::new(contract)));
        Ok(())
    }

    /// Private leaf witnesses never materialize a Dict or bypass a public path.
    fn restore_private_value(&mut self, public: Value) -> Result<Value, VMError> {
        if matches!(public, Value::Dict(_)) || !public.is_portable() {
            return Ok(public);
        }
        self.charge_value_encoding(&public, 1)?;
        let id = public.to_cell()?.id();
        Ok(self.value_witnesses.get(&id).cloned().unwrap_or(public))
    }

    /// _payload pred_ **contract** → _contract_
    fn op_contract(&mut self) -> Result<(), VMError> {
        let pred = self.pop_value()?.to_point()?.to_predicate()?;
        let payload = self.pop_value()?;
        self.charge_value_encoding(&payload, 2)?;
        let anchor = self.consume_anchor()?;
        let contract = Contract::new(pred, anchor, payload)?;
        self.push_value(Value::Contract(Box::new(contract)));
        Ok(())
    }

    /// _payload pred_ **output** → ø
    fn op_output(&mut self) -> Result<(), VMError> {
        let pred = self.pop_value()?.to_point()?.to_predicate()?;
        let payload = self.pop_value()?;
        self.charge_value_encoding(&payload, 2)?;
        let anchor = self.consume_anchor()?;
        let contract = Contract::new(pred, anchor, payload)?;
        self.txlog.push(TxEntry::Output(contract));
        Ok(())
    }

    /// _contract ik root:string32 index gas portable-args… k_ **open** → _results… k'_
    ///
    /// Loads the selected program through the authenticated Cell Trie. The
    /// isolated frame receives one payload Value, followed by explicit arguments.
    fn op_open(&mut self) -> Result<(), VMError> {
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;
        Self::require_portable_call_args(&args)?;
        let gas = self.pop_gas_limit()?;
        self.current_call.charge_gas(gas)?;
        let index = self
            .pop_value()?
            .to_scalar()?
            .to_u64()
            .ok_or(VMError::MalformedTaprootProof)?;
        let root = array32(&self.pop_string()?.to_bytes()).ok_or(VMError::MalformedTaprootProof)?;
        let internal_key = self.pop_value()?.to_point()?.to_compressed();
        let contract = self.pop_value()?.to_contract()?;
        self.current_call.charge_gas(GAS_POINT_DECOMPRESS)?;
        let proof = TaprootProof {
            internal_key,
            root,
            index,
        };
        let root = contract
            .predicate
            .open_program(&proof, &mut self.resolver())?;
        let id = root.id();
        let program = Program::new(root.clone())?;
        let code_bytes = program.len;
        let instructions = if let Some(instructions) = self.script_witnesses.get(&id) {
            if crate::script::instruction_cell(instructions)?.id() != id {
                return Err(VMError::TaprootProofMismatch);
            }
            Some(instructions.clone())
        } else {
            None
        };
        let child_anchor = self.split_anchor_for_call()?;
        self.enter_contract_open_frame(
            contract,
            Script::Opaque(Vec::new()),
            code_bytes,
            gas,
            args,
            child_anchor,
        )?;
        self.current_call.code = None;
        self.current_call.entry_program = Some(root);
        self.current_call.program = Some(program);
        self.current_call.instructions = instructions;
        Ok(())
    }

    /// _cell_ **exec** is a tail transfer: only code cursors and Cell-local
    /// labels change. Frame identity, escrow, stack and gas remain intact.
    fn op_exec(&mut self) -> Result<(), VMError> {
        let reference = self.pop_value()?.into_cell_ref()?;
        let root = cells::read_cell(&mut self.resolver(), &reference)?;
        let program = Program::new(root.as_ref().clone())?;
        let id = root.id();
        let instructions = self
            .current_call
            .private_scripts
            .get(&id)
            .or_else(|| self.script_witnesses.get(&id))
            .cloned();
        if let Some(instructions) = &instructions {
            if crate::script::instruction_cell(instructions)?.id() != id {
                return Err(VMError::TaprootProofMismatch);
            }
        }
        self.current_call.program = Some(program);
        self.current_call.instructions = instructions;
        self.current_call.cursor = 0;
        self.current_call.instruction_cursor = 0;
        self.current_call.labels.clear();
        Ok(())
    }

    /// _contract script sig gas portable-args… m_ **signcall** → _results… k'_
    ///
    /// Checks an Explicit signature over `script` immediately in internal
    /// execution, or defers it for external batch verification, then enters an
    /// isolated `ContractOpen` frame via [`enter_contract_open_frame`].
    fn op_signcall(&mut self) -> Result<(), VMError> {
        let m = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(m)?;
        Self::require_portable_call_args(&args)?;
        let gas = self.pop_gas_limit()?;
        // Debit the grant from the caller (see op_open).
        self.current_call.charge_gas(gas)?;
        let sig_bytes = self.pop_string()?.to_bytes();
        let prog_str = self.pop_string()?;
        let contract = self.pop_value()?.to_contract()?;
        if sig_bytes.len() != 64 {
            return Err(VMError::BadSignatureBytes);
        }
        // Same price in both contexts: internal execution verifies now;
        // external execution schedules the same work in the final batch.
        self.current_call.charge_gas(GAS_SIGNATURE_VERIFY)?;
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_bytes);
        // Canonical bytecode for the signed message; `prog_str` is kept
        // for `to_instructions()` (witness-preserving) just below.
        self.charge_alloc_bytes(prog_str.len())?;
        let msg = Self::signcall_message(&prog_str.to_bytes_vec());
        let code_bytes = prog_str.len();
        let code = prog_str.into_script()?;
        let verification_key = contract.predicate.verification_key();
        let external = self.is_external();
        if !external {
            let signature =
                musig::Signature::from_bytes(sig).map_err(|_| VMError::BadSignatureBytes)?;
            let key = musig::VerificationKey::from_compressed(verification_key);
            signature
                .verify(&mut signcall_verification_transcript(&msg), key)
                .map_err(|_| VMError::SignatureVerificationFailed)?;
        }
        // Snapshot cursors before recording an external deferred signature,
        // so a failed child removes it with the rest of the child effects.
        let child_anchor = self.split_anchor_for_call()?;
        if external {
            self.deferred_sigs.push(DeferredSig::Explicit {
                verification_key,
                message: msg,
                signature: sig,
            });
        }
        self.enter_contract_open_frame(contract, code, code_bytes, gas, args, child_anchor)?;
        Ok(())
    }

    /// Shared tail of `op_open` / `op_signcall`: build a new
    /// `ContractOpen` frame snapshotting the caller's CS context, pour
    /// `contract.payload` then `args` onto the new stack, swap the
    /// parent out, and switch the active anchor to `child_anchor`
    /// (the `left` half of the parent's call-entry split).
    fn enter_contract_open_frame(
        &mut self,
        contract: Contract,
        code: Script,
        code_bytes: usize,
        gas: u64,
        args: Vec<Value>,
        child_anchor: Anchor,
    ) -> Result<(), VMError> {
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(VMError::CallDepthExceeded);
        }
        let failure_count = args.len().saturating_add(1);
        self.charge_alloc_items(failure_count)?;
        let clone_gas = args
            .iter()
            .fold(1u64.saturating_add(contract.clone_gas()), |gas, value| {
                gas.saturating_add(value.clone_gas())
            });
        self.current_call.charge_gas(clone_gas)?;
        let mut failure_values = Vec::with_capacity(failure_count);
        failure_values.push(Value::Contract(Box::new(contract.clone())));
        failure_values.extend(args.iter().cloned());
        self.current_call.snap_failure_values = failure_values;
        self.current_call.snap_failure_arg_count = args.len();
        let external_context = self.is_external();
        let caller_id = self.current_call.kind.actor().map(ActorID::to_hash);
        let mut frame = CallFrame::from_code(
            code,
            CallKind::ContractOpen {
                predicate: contract.predicate.clone(),
                external_context,
                caller_id,
            },
            gas,
        )
        .with_anchor(child_anchor);
        frame.gas_used = alloc_byte_gas(code_bytes)?
            .saturating_add(alloc_item_gas(1usize.saturating_add(args.len()))?);
        frame.stack.push(contract.into_payload());
        for v in args {
            frame.stack.push(v);
        }
        let parent = core::mem::replace(&mut self.current_call, frame);
        self.call_stack.push(parent);
        self.last_anchor = Some(child_anchor);
        Ok(())
    }

    /// Pops the child gas grant as a non-negative `u64`.
    fn pop_gas_limit(&mut self) -> Result<u64, VMError> {
        self.pop_value()?
            .to_scalar()?
            .to_u64()
            .ok_or(VMError::InvalidBitrange)
    }

    /// _args… k refund gas addr_ **send** → ø
    ///
    /// Queues a [`Message`] for the consensus layer to instantiate as a
    /// future internal tx and emits a `TxEntry::Send`. The anchor is
    /// ratcheted from `last_anchor` before the entry is appended. There
    /// is no VM-level method operand; a selector, when used, is an
    /// ordinary payload argument (ADR 0020).
    fn op_send(&mut self) -> Result<(), VMError> {
        let target = self.pop_actor_id()?;
        let gas = self
            .pop_value()?
            .to_scalar()?
            .to_u64()
            .ok_or(VMError::InvalidBitrange)?;
        let refund_predicate = Predicate::opaque(curve25519_dalek::ristretto::CompressedRistretto(
            self.pop_string_32()?,
        ));
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;

        for value in &args {
            self.charge_value_encoding(value, 4)?;
        }
        self.charge_clone_values(&args)?;
        let anchor = self.consume_anchor()?;
        let caller = self.current_call.kind.actor().cloned();
        // Single source of truth: the full Message lives in the
        // TxLog as `TxEntry::Send(Message)` — symmetric with
        // `TxEntry::Output(Contract)`. The block builder scans these
        // entries to construct internal-tx deliveries; no separate
        // queue.
        let message = Message::new(target, caller, anchor, args, gas, refund_predicate)?;
        // A send reserves future execution from the active frame. The grant is
        // intentionally not refunded: asynchronous execution has no live
        // caller to receive it, and descendants must divide an existing grant
        // rather than minting fresh gas.
        self.current_call.charge_gas(gas)?;
        self.txlog.push(TxEntry::Send(message));
        Ok(())
    }

    /// _portable-args… k gas addr_ **call** → _results… k' 1 | args… k 0_
    ///
    /// Synchronous actor-to-actor call. Re-entry is gated by actor-state
    /// presence: a checked-out callee returns its arguments and `k 0`; otherwise
    /// re-entry is permitted. There is no VM-level method operand
    /// (ADR 0020). Calls emit no txlog entry; a callee state mutation is
    /// recorded later by `op_save`.
    fn op_call(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        // ContractOpen caller attribution is not actor authority: only an actor
        // frame may originate a synchronous actor call.
        let caller = ActorID::Hash(self.require_actor()?.to_hash());
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let callee = self.pop_actor_id()?.to_canonical();
        let gas = self
            .pop_value()?
            .to_scalar()?
            .to_u64()
            .ok_or(VMError::InvalidBitrange)?;
        // Debit the grant from the caller (see op_open). A caller that
        // can't afford the grant hard-fails OutOfGas — its own budget
        // is exhausted, not a soft "callee unavailable" marker.
        self.current_call.charge_gas(gas)?;
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;
        Self::require_portable_call_args(&args)?;

        // Pre-frame setup. Any failure here ("cannot enter callee")
        // converts to the restored arguments plus `k 0` —
        // the call simply "did not happen" from the caller's POV. A
        // re-entrant call into an actor that's mid-update lands here
        // too: its state is checked out, so `resolve_method` returns
        // `ActorEmpty` (ADR 0017 — the state is the re-entrancy lock).
        let pre_frame: Result<(CallFrame, u64, Arc<CellIndex>), VMError> = (|| {
            if self.call_stack.len() >= MAX_CALL_DEPTH {
                return Err(VMError::CallDepthExceeded);
            }
            let code_bytes = registry.actor_code_bytes(&callee)?;
            let mut initial_gas = code_bytes
                .saturating_mul(GAS_PER_ALLOC_BYTE)
                .saturating_add(alloc_item_gas(args.len())?);
            if initial_gas > gas {
                return Err(VMError::OutOfGas);
            }
            let actor_cells = registry.actor_cells(&callee)?;
            let mut resolver = ExecutionCells {
                external: &self.cells,
                actor: &actor_cells,
                gas_used: &mut initial_gas,
                gas_limit: gas,
            };
            let script = registry.load_program_with_cells(&callee, &mut resolver)?;
            let frame = CallFrame::from_cell(
                script,
                CallKind::ActorCall {
                    actor: callee.clone(),
                    caller,
                },
                gas,
            )?;
            Ok((frame, initial_gas, actor_cells))
        })();
        let (mut frame, initial_gas, actor_cells) = match pre_frame {
            Ok(v) => v,
            Err(error) => {
                // Pre-frame failure (reentrancy, missing actor, etc.):
                // restore the moved arguments. Availability failures refund
                // the grant; an insufficient child budget burns it.
                if !matches!(
                    error,
                    VMError::OutOfGas | VMError::Cell(CellError::ResourceExhausted)
                ) {
                    self.current_call.gas_used = self.current_call.gas_used.saturating_sub(gas);
                }
                let arg_count = args.len();
                self.push_failed_values(args, arg_count);
                return Ok(());
            }
        };

        self.charge_alloc_items(args.len())?;
        self.charge_clone_values(&args)?;
        self.current_call.snap_failure_values = args.clone();
        self.current_call.snap_failure_arg_count = args.len();

        // Split the parent's anchor: `left` (callee_anchor) seeds
        // the callee's `last_anchor`; `right` is stashed on the
        // parent frame's `post_call_anchor` for restoration on
        // return (success or failure). Also snapshots side-effect
        // cursors so we can roll back if the child errors out.
        let callee_anchor = self.split_anchor_for_call()?;

        frame.anchor = Some(callee_anchor);
        frame.gas_used = initial_gas;
        frame.actor_cells = actor_cells;
        for v in args {
            frame.stack.push(v);
        }

        let parent = core::mem::replace(&mut self.current_call, frame);
        self.call_stack.push(parent);
        // Switch the active anchor to the callee's half.
        self.last_anchor = Some(callee_anchor);
        Ok(())
    }

    /// **load** → _value_
    ///
    /// **Checks out** the current actor's state: moves its portable
    /// `Value` out of
    /// the registry (the actor goes empty) and pushes it onto the
    /// stack. While checked out, any call/load against this actor fails
    /// `ActorEmpty` — the state's presence is the re-entrancy lock (ADR
    /// 0017). A frame must `save` it back or explicitly dismantle it
    /// before returning; leftover state fails the clean-stack rule and
    /// rolls back. State shape is not VM-enforced. Re-loading an
    /// already-checked-out actor errors `ActorEmpty`.
    fn op_load(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = self.require_actor()?.clone();
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let state_bytes = registry.actor_state_bytes(&actor)?;
        self.current_call
            .charge_gas(state_bytes.saturating_mul(GAS_PER_ALLOC_BYTE))?;
        let state = registry.load_state_with_cells(&actor, &mut self.resolver())?;
        self.push_value(state);
        Ok(())
    }

    /// _value_ **save** → ø
    ///
    /// Pops the state value (any portable `Value`), validates
    /// portability, and **moves it back** into
    /// the current actor (which must be checked out by a prior `load` —
    /// else `SaveWithoutLoad`, since saving would clobber live state).
    /// Portability is the canonical storage gate — every inserted value
    /// must be portable (`Scalar`, `String`, `Point`, `Dict` of
    /// portable, non-negative `ClearToken`, `Token`). Non-portable
    /// values (`Contract`, `Merlin`, `Variable`, `Expression`,
    /// `Constraint`, `MultiscalarMul`, `WideToken`, negative
    /// `ClearToken`) hard-fail `NonPortableInState`.
    fn op_save(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = ActorID::Hash(self.require_actor()?.to_hash());
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let state = self.pop_value()?;
        // Portability is the canonical storage gate — checked here
        // before any registry mutation so a bad state is rejected
        // cleanly. Distinct from encodability (which the encoder may
        // or may not implement for a given variant).
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        self.charge_value_encoding(&state, 2)?;
        self.current_call.charge_gas(state.clone_gas())?;
        // Rust-level deep clone for the txlog entry. The registry
        // takes ownership of one copy; the txlog gets another.
        // `clone` ignores VM stack-copyability rules so portable
        // linear values (Token) survive — those are exactly what
        // actor state is for.
        let state_for_log = state.clone();
        // Moves the state back in; errors `SaveWithoutLoad` if the
        // actor isn't checked out (no matching `load`).
        registry.save_state(&actor, state)?;
        registry.validate_actor_storage(&actor, self.block_height)?;
        // Structural effect. State-machine replay applies these
        // last-write-wins per actor; the merkle leaf hashes
        // `state_root(state)`, while the entry carries the full state
        // value for direct consumers.
        self.txlog.push(TxEntry::ActorSave {
            actor,
            state: state_for_log,
        });
        Ok(())
    }

    /// _code_ **setcode** → ø
    ///
    /// Replaces the current actor's code blob with the popped String's
    /// bytes and records `TxEntry::SetCode`. Upgrade *policy* (who may
    /// call this) is the author's, gated in the actor's own code via
    /// `callerid` — actors authenticate by identity, not signatures.
    /// See ADR 0018.
    fn op_setcode(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = ActorID::Hash(self.require_actor()?.to_hash());
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let code_string = self.pop_string()?;
        self.charge_alloc_bytes(code_string.len())?;
        let code = code_string.to_bytes();
        self.charge_alloc_bytes(code.len())?;
        registry.set_code(&actor, code.clone())?;
        registry.validate_actor_storage(&actor, self.block_height)?;
        self.txlog.push(TxEntry::SetCode { actor, code });
        Ok(())
    }

    /// _q_ **addstorage** → _debt 1 | 0_. Invalid market requests are
    /// soft failures; type, context, and host invariant errors are hard.
    fn op_addstorage(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = ActorID::Hash(self.require_actor()?.to_hash());
        let request = self.pop_value()?.to_scalar()?;
        let Some(bytes) = request.to_u64() else {
            self.push_value(Value::Scalar(Scalar::ZERO));
            return Ok(());
        };
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let Some(purchase) = registry.purchase_storage(&actor, bytes, self.block_height)? else {
            self.push_value(Value::Scalar(Scalar::ZERO));
            return Ok(());
        };
        if purchase.fee_sparks.is_zero() || purchase.fee_sparks.is_centered_negative() {
            return Err(VMError::StorageArithmeticOverflow);
        }
        self.txlog.push(TxEntry::StoragePurchase {
            actor: actor.clone(),
            bytes,
            expiry_height: purchase.expiry_height,
            fee_sparks: purchase.fee_sparks,
        });
        self.push_value(Value::ClearToken(ClearToken::new(
            -purchase.fee_sparks,
            FLAME_FLAVOR,
        )));
        self.push_value(Value::Scalar(Scalar::ONE));

        Ok(())
    }

    /// _q_ **quotestorage** → _fee 1 | 0_. Read-only counterpart to
    /// [`Self::op_addstorage`].
    fn op_quotestorage(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = self.require_actor()?.clone();
        let request = self.pop_value()?.to_scalar()?;
        let Some(bytes) = request.to_u64() else {
            self.push_value(Value::Scalar(Scalar::ZERO));
            return Ok(());
        };
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        match registry.quote_storage(&actor, bytes, self.block_height)? {
            Some(quote)
                if !quote.fee_sparks.is_zero() && !quote.fee_sparks.is_centered_negative() =>
            {
                self.push_value(Value::Scalar(quote.fee_sparks));
                self.push_value(Value::Scalar(Scalar::ONE));
            }
            Some(_) => return Err(VMError::StorageArithmeticOverflow),
            None => self.push_value(Value::Scalar(Scalar::ZERO)),
        }
        Ok(())
    }

    /// _contract_ **signtx** → _payload_
    ///
    /// External-only. Defers a TxID-bound signature for the contract's predicate,
    /// and returns its single payload Value.
    fn op_signtx(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let contract = self.pop_value()?.to_contract()?;
        self.charge_value_encoding(contract.payload(), 1)?;
        // Each authorized contract contributes one key/message term to the final
        // aggregate-signature verification.
        self.current_call.charge_gas(GAS_SIGNATURE_VERIFY)?;
        self.deferred_sigs.push(DeferredSig::TxBound {
            verification_key: contract.predicate.verification_key(),
            contract_id: contract.id(),
        });
        self.push_value(contract.into_payload());
        Ok(())
    }

    /// **selfid** → _string_
    fn op_selfid(&mut self) -> Result<(), VMError> {
        let actor = self.require_actor()?.clone();
        self.push_value(Value::String(String::from(actor.to_hash().to_vec())));
        Ok(())
    }

    /// **anchor** → _string_
    ///
    /// Pushes the tx's *current* anchor (the value the next consume
    /// site would split). Hard-fails `AnchorMissing` if no anchor
    /// has been claimed yet — same rule as `contract` / `output` /
    /// `send`. Available in either context.
    fn op_anchor(&mut self) -> Result<(), VMError> {
        let a = self.last_anchor.ok_or(VMError::AnchorMissing)?;
        self.push_value(Value::String(String::from(a.0.to_vec())));
        Ok(())
    }

    /// **callerid** → _string_
    ///
    /// Pushes the direct caller actor id, or all-zero String when no
    /// authenticated actor directly invoked this frame. Available in actor
    /// and ContractOpen frames, but not ExternalRoot.
    fn op_callerid(&mut self) -> Result<(), VMError> {
        if matches!(self.current_call.kind, CallKind::ExternalRoot) {
            return Err(VMError::OpcodeRequiresActorContext);
        }
        let bytes = self.current_call.kind.caller_id().unwrap_or([0u8; 32]);
        self.push_value(Value::String(String::from(bytes.to_vec())));
        Ok(())
    }

    /// **timelock** → _n {0|1}_
    ///
    /// Pushes the transaction's `locktime` and a unit flag
    /// (0 = block height, 1 = Unix timestamp). Bitcoin BIP-65
    /// convention: `flag = 1` iff `locktime >= LOCKTIME_TIMESTAMP_THRESHOLD`
    /// (i.e. ≥ 500_000_000, approximately 1985-11-05 Unix time).
    fn op_timelock(&mut self) -> Result<(), VMError> {
        let lt = self.header.locktime as u64;
        let flag: u64 = if lt >= LOCKTIME_TIMESTAMP_THRESHOLD as u64 {
            1
        } else {
            0
        };
        self.push_value(Value::Scalar(Scalar::from(lt)));
        self.push_value(Value::Scalar(Scalar::from(flag)));
        Ok(())
    }

    /// **gas** → _n_  Pushes remaining gas for the current call.
    fn op_gas(&mut self) -> Result<(), VMError> {
        let remaining = self
            .current_call
            .gas_limit
            .saturating_sub(self.current_call.gas_used);
        self.push_value(Value::Scalar(Scalar::from(remaining)));
        Ok(())
    }

    /// **usage** → _n_.
    fn op_usage(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = self.require_actor()?;
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let usage = registry.actor_usage(actor)?;
        self.push_value(Value::Scalar(Scalar::from(usage)));
        Ok(())
    }

    /// _height_ **capacity** → _bytes_.
    fn op_capacity(&mut self, registry: Option<&mut dyn ActorRegistry>) -> Result<(), VMError> {
        let actor = self.require_actor()?.clone();
        let height = self
            .pop_value()?
            .to_scalar()?
            .to_u64()
            .ok_or(VMError::InvalidBitrange)?;
        if height < self.block_height {
            return Err(VMError::StorageHeightInPast);
        }
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let capacity = registry.actor_capacity(&actor, height)?;
        self.push_value(Value::Scalar(Scalar::from(capacity)));
        Ok(())
    }

    /// **alloc** → _expr_
    ///
    /// Allocates a low-level R1CS variable. Witness `Some(i)` on the
    /// prover; `None` on the verifier.
    fn op_alloc<D: Delegate>(
        &mut self,
        witness: Option<Scalar>,
        delegate: &mut D,
    ) -> Result<(), VMError> {
        self.require_external()?;
        self.charge_alloc_items(1)?;
        self.current_call.charge_gas(r1cs_gas(1)?)?;
        use bulletproofs::r1cs::ConstraintSystem;
        let witness_scalar = witness.map(|i| i.to_dalek());
        let r1cs_var = delegate
            .cs()
            .allocate(witness_scalar)
            .map_err(VMError::R1CSError)?;
        let expr = Expression::LinearCombination(
            vec![(r1cs_var, curve25519_dalek::scalar::Scalar::ONE)],
            witness,
        );
        self.push_value(Value::Expression(expr));
        Ok(())
    }

    /// `expr` — `var → expr`. Pops a `Variable`, calls
    /// `delegate.commit_variable` to allocate a CS-side variable for
    /// the commitment, pushes a one-term Expression.
    fn op_expr<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        self.charge_alloc_items(1)?;
        self.current_call.charge_gas(r1cs_gas(1)?)?;
        use curve25519_dalek::scalar::Scalar as DalekScalar;
        let var = self.pop_value()?.to_variable()?;
        let (_point, r1cs_var) = delegate.commit_variable(&var.commitment)?;
        let witness = var.commitment.assignment();
        let expr = Expression::LinearCombination(vec![(r1cs_var, DalekScalar::ONE)], witness);
        self.push_value(Value::Expression(expr));
        Ok(())
    }

    /// _x n_ **range** → _x_
    ///
    /// Checks a raw scalar in any context. In external execution only,
    /// an Expression is checked or constrained to the same `[0, 2^n)` range.
    /// The checked value is returned with its original type.
    fn op_range<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        use bulletproofs::r1cs::LinearCombination as LC;
        use spacesuit::BitRange;

        // Pop n (bit-count) — must be a scalar in [1, 64].
        let n_int = self.pop_value()?.to_scalar()?;
        let n_u64 = n_int.to_u64().ok_or(VMError::BitCountOutOfRange)?;
        let n_usize = usize::try_from(n_u64).map_err(|_| VMError::BitCountOutOfRange)?;
        if n_usize == 0 {
            return Err(VMError::BitCountOutOfRange);
        }
        let bit_range = BitRange::new(n_usize).ok_or(VMError::BitCountOutOfRange)?;

        let value = self.pop_value()?;
        if let Value::Scalar(scalar) = value {
            if !scalar_fits_in_n_bits(scalar, n_usize) {
                return Err(VMError::InvalidBitrange);
            }
            self.push_value(Value::Scalar(scalar));
            return Ok(());
        }

        self.require_external()?;
        self.charge_alloc_items(n_usize)?;

        let expr = value.to_expression()?;

        match &expr {
            Expression::Constant(value) => {
                // Public constants are checked without touching the CS.
                if !scalar_fits_in_n_bits(*value, n_usize) {
                    return Err(VMError::InvalidBitrange);
                }
                self.push_value(Value::Expression(expr));
                Ok(())
            }
            Expression::LinearCombination(terms, assignment) => {
                self.current_call.charge_gas(r1cs_gas(n_usize)?)?;
                let lc: LC = terms.iter().cloned().collect();
                // Convert the witness (if present) to spacesuit's
                // SignedInteger. Scalars in the u64 range map cleanly;
                // other assignments fail the prover at
                // gadget time via `to_u64() → None` inside
                // spacesuit::range_proof.
                let assignment_si = match assignment {
                    Some(i) => Some(scalar_to_signed_integer(*i)?),
                    None => None,
                };
                spacesuit::range_proof(delegate.cs(), lc, assignment_si, bit_range)
                    .map_err(VMError::R1CSError)?;
                self.push_value(Value::Expression(expr));
                Ok(())
            }
        }
    }

    /// `scalar` — `string → expr`. Pops a String, downcasts to
    /// `Scalar` via `String::to_scalar`, pushes `Expression::Constant`.
    /// For `String::Opaque(bytes)`, the bytes are parsed as a
    /// canonical scalar. For `StringWitness::Scalar(i)`, the
    /// witness is extracted directly.
    fn op_scalar(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_string()?;
        let int = s.to_scalar()?;
        self.push_value(Value::Expression(Expression::constant(int)));
        Ok(())
    }

    /// _s_ **commit** → _var_
    fn op_commit(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_string()?;
        let commitment = s.to_commitment()?;
        let var = Variable { commitment };
        self.push_value(Value::Variable(var));
        Ok(())
    }

    /// Encrypted branch of `borrow`. The caller has already
    /// popped `(qty, flv)` and verified both are `Variable`; this
    /// just runs the CS plumbing — range-proof + additive-inverse
    /// allocation — and pushes the `WideToken` / `Token` pair.
    fn op_borrow_encrypted_inner<D: Delegate>(
        &mut self,
        qty: Variable,
        flv: Variable,
        delegate: &mut D,
    ) -> Result<(), VMError> {
        use bulletproofs::r1cs::ConstraintSystem;
        use spacesuit::BitRange;
        // Two committed variables, a 64-bit range gadget, and the allocated
        // additive inverse.
        self.current_call.charge_gas(r1cs_gas(67)?)?;
        // Commit both to the CS. Prover uses the open witness; verifier
        // sees only the closed point. Either way, the returned r1cs vars
        // are bound to the same commitment point on both sides.
        let (_flv_point, flv_var) = delegate.commit_variable(&flv.commitment)?;
        let (_qty_point, qty_var) = delegate.commit_variable(&qty.commitment)?;
        // Witness assignments (prover only). Negative qty is a
        // protocol error here — borrow's +T is range-proven non-negative.
        let qty_assignment = match qty.commitment.assignment() {
            Some(i) => Some(scalar_to_signed_integer(i)?),
            None => None,
        };
        let flv_assignment = flv.commitment.assignment().map(|i| i.to_dalek());
        // 64-bit range proof on the positive qty.
        spacesuit::range_proof(
            delegate.cs(),
            qty_var.into(),
            qty_assignment,
            BitRange::max(),
        )
        .map_err(VMError::R1CSError)?;
        // Allocate -qty in the CS, witness = -qty_assignment.
        let neg_qty_assignment = qty_assignment.map(|q| -q);
        let neg_qty_var = delegate
            .cs()
            .allocate(neg_qty_assignment.map(|q| q.to_scalar()))
            .map_err(VMError::R1CSError)?;
        // Constrain qty + (-qty) = 0.
        delegate.cs().constrain(qty_var + neg_qty_var);
        // Build the WideToken (negative half) and the Token (positive
        // half). The Token carries the prover's open commitments
        // unchanged so downstream `mix` / `cloak` can re-commit them.
        let wide = WideToken(spacesuit::AllocatedValue {
            q: neg_qty_var,
            f: flv_var,
            assignment: match (neg_qty_assignment, flv_assignment) {
                (Some(q), Some(f)) => Some(Box::new(spacesuit::Value { q, f })),
                _ => None,
            },
        });
        let token = Token::new(qty.commitment, flv.commitment);
        self.push_value(Value::WideToken(wide));
        self.push_value(Value::Token(token));
        Ok(())
    }

    /// _qty_ **fee** → _widetoken_
    ///
    /// External-only. Records `TxEntry::Fee(qty)`, allocates a WideToken
    /// debt with `q = -qty` and the native Flame flavor, then pushes it.
    fn op_fee<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::r1cs::ConstraintSystem;
        self.current_call.charge_gas(r1cs_gas(2)?)?;
        let qty = self.pop_value()?.to_scalar()?;
        if qty.is_centered_negative() {
            return Err(VMError::FeeQtyNegative);
        }
        let qty_u64 = qty.to_u64().ok_or(VMError::FeeTooHigh)?;
        self.total_fee.add(qty_u64)?;
        let qty_scalar: curve25519_dalek::scalar::Scalar = qty.into();
        let flv_scalar: curve25519_dalek::scalar::Scalar = FLAME_FLAVOR.into();
        let q_var = delegate
            .cs()
            .allocate(Some(-qty_scalar))
            .map_err(VMError::R1CSError)?;
        delegate.cs().constrain(q_var + qty_scalar);
        let f_var = delegate
            .cs()
            .allocate(Some(flv_scalar))
            .map_err(VMError::R1CSError)?;
        delegate.cs().constrain(f_var - flv_scalar);
        let assignment = Some(Box::new(spacesuit::Value {
            q: -spacesuit::SignedInteger::from(qty_u64),
            f: flv_scalar,
        }));
        let wide = WideToken(spacesuit::AllocatedValue {
            q: q_var,
            f: f_var,
            assignment,
        });
        self.push_value(Value::WideToken(wide));
        self.txlog.push(TxEntry::Fee(qty_u64));
        Ok(())
    }

    /// Converts a stack value into a `spacesuit::AllocatedValue` for
    /// the cloak gadget. Token/ClearToken commit to the CS; WideToken
    /// unwraps in place.
    fn value_to_allocated<D: Delegate>(
        value: Value,
        delegate: &mut D,
    ) -> Result<spacesuit::AllocatedValue, VMError> {
        let (qty, flv) = match value {
            Value::WideToken(w) => return Ok(w.0),
            Value::Token(t) => (t.qty, t.flv),
            Value::ClearToken(c) => (
                Commitment::unblinded(c.qty()),
                Commitment::unblinded(c.flv()),
            ),
            _ => return Err(VMError::TypeNotToken),
        };
        let (_, qty_var) = delegate.commit_variable(&qty)?;
        let (_, flv_var) = delegate.commit_variable(&flv)?;
        let qty_assg = match qty.assignment() {
            Some(i) => Some(scalar_to_signed_integer(i)?),
            None => None,
        };
        let flv_assg = flv.assignment().map(|i| i.to_dalek());
        Ok(spacesuit::AllocatedValue {
            q: qty_var,
            f: flv_var,
            assignment: match (qty_assg, flv_assg) {
                (Some(q), Some(f)) => Some(Box::new(spacesuit::Value { q, f })),
                _ => None,
            },
        })
    }

    /// _anytokens… commitments… m n_ **mix** → _tokens_
    ///
    /// Invokes the spacesuit cloak gadget to balance `m` input tokens
    /// against `n` range-proven output tokens per flavor.
    fn op_mix<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        // Pop n (output count) and m (input count).
        let n = self.pop_byte_count(usize::MAX)?;
        let m = self.pop_byte_count(usize::MAX)?;
        // Degenerate shapes (m=0 or n=0) make `spacesuit::cloak`'s
        // `k_mix` underflow (`0..k-1` with usize `k=0`). Reject at
        // the opcode boundary so the error is deterministic.
        if m == 0 || n == 0 {
            return Err(VMError::MixDegenerate);
        }
        let mix_items = m.checked_add(n).ok_or(VMError::OutOfGas)?;
        let quadratic_work = mix_items.checked_mul(mix_items).ok_or(VMError::OutOfGas)?;
        self.charge_alloc_items(quadratic_work)?;
        self.current_call.charge_gas(r1cs_gas(quadratic_work)?)?;
        // Stack depth check: we'll pop 2n commitment Strings + m token values.
        let needed = m.saturating_add(n.saturating_mul(2));
        if needed > self.current_call.stack.len() {
            return Err(VMError::StackUnderflow);
        }
        // Build outputs (closest to top): each output pops (flv, qty)
        // Strings → builds Token (with Closed commitments since the
        // String→Commitment downcast retains witness only for
        // String::Commitment variants).
        let mut output_tokens: Vec<Token> = Vec::with_capacity(n);
        let mut cloak_outs: Vec<spacesuit::AllocatedValue> = Vec::with_capacity(n);
        for _ in 0..n {
            let flv_str = self.pop_string()?;
            let qty_str = self.pop_string()?;
            let flv_commit = flv_str.to_commitment()?;
            let qty_commit = qty_str.to_commitment()?;
            let token = Token::new(qty_commit, flv_commit);
            // Build the AllocatedValue against the CS.
            let allocated = Self::value_to_allocated(Value::Token(token.clone()), delegate)?;
            // Insert at front so the deepest output ends up at cloak_outs[0].
            output_tokens.insert(0, token);
            cloak_outs.insert(0, allocated);
        }
        // Build inputs.
        let mut cloak_ins: Vec<spacesuit::AllocatedValue> = Vec::with_capacity(m);
        for _ in 0..m {
            let item = self.pop_value()?;
            let allocated = Self::value_to_allocated(item, delegate)?;
            cloak_ins.insert(0, allocated);
        }
        // Run the cloak gadget. On constraint-system error, surface
        // as R1CSError; the verifier will reject the proof.
        spacesuit::cloak(delegate.cs(), cloak_ins, cloak_outs).map_err(VMError::R1CSError)?;
        // Push the output Tokens in the same order (deepest first).
        for token in output_tokens {
            self.push_value(Value::Token(token));
        }
        Ok(())
    }

    /// `decrypt` — `token f f' q q' → cleartoken`. Reveals a
    /// cleartext quantity / flavor pair for an encrypted Token by
    /// supplying their cleartext values (`f`, `q`) and Pedersen
    /// blinding factors (`f'`, `q'`).
    ///
    /// External execution appends both opening equations to the delegate's
    /// batch and checks them at transaction finalization. Internal execution
    /// checks both equations immediately because it has no deferred batch.
    ///
    /// All four scalar operands (`f`, `f'`, `q`, `q'`) are popped as
    /// `Scalar`. The Token is popped last (deepest on stack). The
    /// `ClearToken(q, f)` is pushed only after the applicable check is queued
    /// or completed.
    fn op_decrypt<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        use bulletproofs::PedersenGens;
        let q_blind = self.pop_value()?.to_scalar()?;
        let q_value = self.pop_value()?.to_scalar()?;
        let f_blind = self.pop_value()?.to_scalar()?;
        let f_value = self.pop_value()?.to_scalar()?;
        let token = match self.pop_value()? {
            Value::Token(t) => t,
            _ => return Err(VMError::TypeNotToken),
        };
        // Two point decompressions and two two-term opening equations. The
        // charge is identical whether they are checked immediately or batched.
        let opening_gas = linear_gas(GAS_MSM_VERIFY_BASE, GAS_MSM_VERIFY_TERM, 2)?;
        self.current_call
            .charge_gas(opening_gas.checked_mul(2).ok_or(VMError::OutOfGas)?)?;
        let gens = PedersenGens::default();
        let qty_point = token.qty.to_point().decompress();
        let flv_point = token.flv.to_point().decompress();
        if self.is_external() {
            // Each equation gets its own random batch factor, so it cannot
            // cancel another opening or an unrelated signature statement.
            let neg_one = -DalekScalar::ONE;
            musig::BatchVerification::append(
                delegate.batch_verifier(),
                q_value.to_dalek(),
                [q_blind.to_dalek(), neg_one],
                [Some(gens.B_blinding), qty_point],
            );
            musig::BatchVerification::append(
                delegate.batch_verifier(),
                f_value.to_dalek(),
                [f_blind.to_dalek(), neg_one],
                [Some(gens.B_blinding), flv_point],
            );
        } else {
            let qty_point = qty_point.ok_or(VMError::InvalidPoint)?;
            let flv_point = flv_point.ok_or(VMError::InvalidPoint)?;
            let expected_qty = gens.B * q_value.to_dalek() + gens.B_blinding * q_blind.to_dalek();
            let expected_flv = gens.B * f_value.to_dalek() + gens.B_blinding * f_blind.to_dalek();
            if expected_qty != qty_point || expected_flv != flv_point {
                return Err(VMError::CommitmentOpeningMismatch);
            }
        }
        self.push_value(Value::ClearToken(ClearToken::new(q_value, f_value)));
        Ok(())
    }
}

/// Checks a canonical residue against `[0, 2^n)` for `n` in `[1, 64]`.
/// Used for raw scalars and public Expression constants, without R1CS.
fn scalar_fits_in_n_bits(value: Scalar, n: usize) -> bool {
    if n == 64 {
        return value.to_u64().is_some();
    }
    match value.to_u64() {
        Some(v) => v < (1u64 << n),
        None => false,
    }
}

/// Converts a scalar's centered value to a `spacesuit::SignedInteger`
/// witness when its magnitude fits u64. Other values error `InvalidBitrange`.
///
/// **Prover/verifier asymmetry (fail-closed, liveness-only).** Callers
/// invoke this only on the *prover* side (`commitment.assignment()` is
/// `Some` for the prover, `None` for the verifier). An out-of-`u64`
/// witness makes this error on the prover while the verifier — lacking
/// the assignment — does not, so a caught sub-call could push a `0`
/// marker on the prover and `1` on the verifier. That divergence is
/// **fail-closed**: the proof binds the whole CS via Fiat–Shamir, so
/// any prover/verifier control-flow divergence makes the proof fail to
/// verify (verifier rejects) — it can never make the verifier *accept*
/// an invalid tx. The effect is a self-inflicted liveness edge (a
/// prover that commits to an out-of-range qty produces an unverifiable
/// tx), not a soundness break. Hardening (making such witness-gated
/// failures tx-level/uncatchable so they never reach a marker) is a
/// deliberate ZK-review item, not a drive-by change.
fn scalar_to_signed_integer(value: Scalar) -> Result<spacesuit::SignedInteger, VMError> {
    if value.is_centered_negative() {
        let mag = value.centered_abs();
        let mag_u64 = mag.to_u64().ok_or(VMError::InvalidBitrange)?;
        Ok(-spacesuit::SignedInteger::from(mag_u64))
    } else {
        let v = value.to_u64().ok_or(VMError::InvalidBitrange)?;
        Ok(spacesuit::SignedInteger::from(v))
    }
}

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;

#[cfg(test)]
mod cell_execution_tests {
    use super::*;
    use crate::{PredicateTree, Prover, Verifier};
    use bulletproofs::PedersenGens;

    #[test]
    fn wrapping_existing_dictionaries_prepays_growing_encoding_work() {
        let frame = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 15_000);
        let mut vm = VM::new(
            TxHeader {
                version: 1,
                locktime: 0,
            },
            frame,
        );
        vm.push_value(Value::String(String::from(vec![7; 1000])));
        let mut costs = Vec::new();
        for _ in 0..20 {
            vm.push_value(Value::Scalar(Scalar::ZERO));
            vm.push_value(Value::Scalar(Scalar::ONE));
            let before = vm.current_call.gas_used;
            match vm.op_dict() {
                Ok(()) => costs.push(vm.current_call.gas_used - before),
                Err(VMError::OutOfGas) => {
                    assert!(costs.len() >= 2);
                    assert!(costs.windows(2).all(|pair| pair[1] > pair[0]));
                    return;
                }
                Err(error) => panic!("unexpected error: {:?}", error),
            }
        }
        panic!("nested serialization must exhaust the finite budget");
    }

    #[test]
    fn dictionary_insertion_rejects_unpaid_large_value_encoding() {
        let frame = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 100);
        let mut vm = VM::new(
            TxHeader {
                version: 1,
                locktime: 0,
            },
            frame,
        );
        vm.push_value(Value::Dict(Dict::new()));
        vm.push_value(Value::Scalar(Scalar::ZERO));
        vm.push_value(Value::String(String::from(vec![7; String::MAX_LEN])));
        assert!(matches!(vm.op_put(), Err(VMError::OutOfGas)));
    }

    #[test]
    fn input_dict_loads_only_the_requested_path_even_with_private_witnesses() {
        struct Recording {
            source: CellIndex,
            used: CellIndex,
        }
        impl CellReader for Recording {
            fn read(
                &mut self,
                id: CellID,
                resident: Option<&Arc<Cell>>,
            ) -> Result<Arc<Cell>, CellError> {
                let cell = self.source.read(id, resident)?;
                self.used.insert(Arc::clone(&cell))?;
                Ok(cell)
            }
        }
        let mut dict = Dict::new();
        dict.insert(Scalar::ONE, Value::Scalar(Scalar::from(11u64)));
        dict.insert(Scalar::from(2u64), Value::Scalar(Scalar::from(22u64)));
        let contract = Contract::new(
            Predicate::opaque(Predicate::unspendable_key()),
            Anchor([3; 32]),
            Value::Dict(dict),
        )
        .unwrap();
        let id = contract.id();
        let mut recording = Recording {
            source: CellIndex::collect(Arc::new(contract.to_cell().unwrap())).unwrap(),
            used: CellIndex::new(),
        };
        let root = cells::resolve_cell(&mut recording, id).unwrap();
        let loaded = Contract::from_trusted_cell(&root, &mut recording).unwrap();
        let mut dict = loaded.into_payload().to_dict().unwrap();
        dict.get_resolved(&Scalar::ONE, &mut recording)
            .unwrap()
            .unwrap();
        assert!(recording.used.len() < recording.source.len());
        let cells = Arc::new(recording.used);

        let mut costs = Vec::new();
        for private_overlay in [false, true] {
            let frame = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 1_000_000);
            let mut vm = VM::new(
                TxHeader {
                    version: 1,
                    locktime: 0,
                },
                frame,
            )
            .with_cells(Arc::clone(&cells))
            .unwrap();
            if private_overlay {
                vm.contract_witnesses.insert(id, contract.clone());
            }
            vm.push_value(Value::String(String::from(id.to_vec())));
            vm.op_input()
                .expect("input does not eagerly hydrate hidden branches");
            vm.op_signtx().unwrap();
            let Value::Dict(dict) = &vm.current_call.stack[0] else {
                panic!("expected Dict")
            };
            assert_eq!(dict.cached_values().count(), 0);
            for _ in 0..2 {
                vm.push_value(Value::Scalar(Scalar::ONE));
                let before = vm.current_call.gas_used;
                vm.op_getdup().unwrap();
                costs.push(vm.current_call.gas_used - before);
                assert!(matches!(vm.pop_value().unwrap(), Value::Scalar(n) if n == Scalar::ONE));
                assert!(
                    matches!(vm.pop_value().unwrap(), Value::Scalar(n) if n == Scalar::from(11u64))
                );
            }
            vm.push_value(Value::Scalar(Scalar::from(2u64)));
            assert!(matches!(
                vm.op_getdup(),
                Err(VMError::Cell(CellError::MissingCell(_)))
            ));
        }
        assert!(costs.iter().all(|cost| *cost == costs[0]));
    }

    #[test]
    fn contract_and_branch_bodies_use_the_committed_bag_and_private_overlays() {
        let branch = ScriptBuilder::new()
            .drop_()
            .alloc(Some(Scalar::from(9u64)))
            .drop_()
            .push_int(0u64)
            .return_();
        let tree = PredicateTree::scripts_only(vec![branch.to_bytecode()], [7; 32]).unwrap();
        let (proof, cells) = tree.witness_for(0).unwrap();
        let contract = Contract::new(
            Predicate::tree(tree),
            Anchor([1; 32]),
            Value::Scalar(Scalar::ONE),
        )
        .unwrap();
        let program = ScriptBuilder::new()
            .with_cells(cells)
            .with_script_witness(branch)
            .unwrap()
            .push_str(String::contract(contract))
            .input()
            .push_point(*proof.internal_key.as_bytes())
            .push_str(String::from(proof.root.to_vec()))
            .push_int(proof.index)
            .push_int(100_000u64)
            .push_int(0u64)
            .open()
            .verify()
            .drop_();
        let header = TxHeader {
            version: 1,
            locktime: 0,
        };
        let pc = PedersenGens::default();
        let result = Prover::prove(&pc, program, header, 1_000_000).unwrap();
        let proof = result.proof.as_ref().unwrap();
        let verified = Verifier::verify_with_cells(
            &pc,
            result.bytecode.clone(),
            proof,
            header,
            1_000_000,
            None,
            &result.cells,
        )
        .unwrap();
        assert_eq!(result.txid, verified.txid);
        assert_eq!(result.gas_used, verified.gas_used);
        assert!(matches!(result.txlog[1], TxEntry::Input(_)));
        assert!(
            Verifier::verify(&pc, result.bytecode.clone(), proof, header, 1_000_000, None).is_err()
        );
        let mut extended = result.cells.as_ref().clone();
        extended
            .insert(Arc::new(Cell::new(vec![99], vec![]).unwrap()))
            .unwrap();
        assert!(Verifier::verify_with_cells(
            &pc,
            result.bytecode,
            proof,
            header,
            1_000_000,
            None,
            &extended
        )
        .is_err());
    }

    #[test]
    fn cell_resolution_has_identical_gas_for_ram_actor_storage_and_witnesses() {
        let cell = Arc::new(Cell::new(vec![7; 128], vec![]).unwrap());
        let bag = CellIndex::collect(cell.clone()).unwrap();
        let empty = CellIndex::new();
        let mut costs = Vec::new();
        for (reference, actor, external) in [
            (Some(CellRef::Resident(cell.clone())), &empty, &empty),
            (
                Some(CellRef::resident(cell.clone()).to_unloaded()),
                &bag,
                &empty,
            ),
            (
                Some(CellRef::resident(cell.clone()).to_unloaded()),
                &empty,
                &bag,
            ),
            (None, &bag, &empty),
            (None, &empty, &bag),
        ] {
            let mut gas = 0;
            let mut resolver = ExecutionCells {
                external,
                actor,
                gas_used: &mut gas,
                gas_limit: 1000,
            };
            let read = match reference {
                Some(reference) => read_cell(&mut resolver, &reference),
                None => cells::resolve_cell(&mut resolver, cell.id()),
            };
            assert_eq!(read.unwrap().id(), cell.id());
            costs.push(gas);
        }
        assert_eq!(costs, vec![131; 5]);
    }

    #[test]
    fn cell_read_misses_are_scoped_and_charge_attempts_before_lookup() {
        let empty = CellIndex::new();
        let id = [9; 32];
        let mut gas = 0;
        let mut reader = ExecutionCells {
            external: &empty,
            actor: &empty,
            gas_used: &mut gas,
            gas_limit: 1000,
        };
        assert_eq!(
            cells::resolve_cell(&mut reader, id).unwrap_err(),
            CellError::MissingCell(id)
        );
        assert_eq!(gas, 1);

        let cell = Arc::new(Cell::new(vec![7], vec![]).unwrap());
        for reference in [Some(CellRef::resident(cell.clone())), None] {
            let mut gas = 0;
            let mut reader = ExecutionCells {
                external: &empty,
                actor: &empty,
                gas_used: &mut gas,
                gas_limit: 0,
            };
            let result = match reference {
                Some(r) => read_cell(&mut reader, &r),
                None => cells::resolve_cell(&mut reader, cell.id()),
            };
            assert_eq!(result.unwrap_err(), CellError::ResourceExhausted);
            assert_eq!(gas, 1);
        }
    }
}
