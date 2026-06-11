//! FlameVM execution engine: Tx → CallFrame + dispatch loop.

use bulletproofs::r1cs;
use bulletproofs::r1cs::R1CSProof;
use core::convert::TryFrom;
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use core::mem;
use merlin::Transcript;
use readerwriter::{Decodable, Encodable, ExactSizeEncodable, ReadError, Reader, WriteError, Writer};

use crate::errors::VMError;
use crate::tx::TxHeader;
use crate::cell::{CallProof, Cell, Predicate};
use crate::constraints::Commitment;
use crate::token::{flavor_from_actor, flavor_from_predicate};
use crate::{ClearToken, Dict, Int253, Merlin, Point, String, Value};

// Re-export from canonical homes so vm.rs callers (notably the
// test helpers, which inherit `super::super::*`) keep their
// existing import shape. The types themselves live in `actor.rs`
// and `send.rs`; vm.rs just plumbs them.
pub use crate::actor::{ActorID, ActorRegistry};
pub use crate::send::Message;

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
    /// (cell / message / callee frame), the `right` half replaces
    /// the consumer's current anchor.
    ///
    /// Uniqueness inherits from the parent: if `self` came from a
    /// spend-once source (an input cell's id) and from a chain of
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
impl Encodable for Anchor {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        w.write(b"anchor", &self.0)
    }
}

impl ExactSizeEncodable for Anchor {
    fn encoded_size(&self) -> usize { 32 }
}

impl Decodable for Anchor {
    fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        Ok(Anchor(r.read_u8x32()?))
    }
}

// `Predicate` lives in `cell::predicate`; re-exported via `crate::Predicate`.

/// Signature check deferred to `Delegate::finalize`. `TxBound` comes
/// from `signtx` (aggregated MuSig verified against TxID); `Explicit`
/// comes from `signcall` (single signature over a program transcript).
#[derive(Clone, Debug)]
pub enum DeferredSig {
    TxBound {
        verification_key: CompressedRistretto,
        cell_id: crate::cell::CellID,
    },
    Explicit {
        verification_key: CompressedRistretto,
        message: Vec<u8>,
        signature: [u8; 64],
    },
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
        commitment: &crate::Commitment,
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
        _commitment: &crate::Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError> {
        unreachable!("InternalDelegate::commit_variable — commit/expr/mix are external-only")
    }
}

/// Flat per-instruction gas cost — placeholder until per-opcode
/// calibration lands (ADR 0009). Charged for every fetched instruction,
/// executed or skip-scanned.
const GAS_PER_INSTRUCTION: u64 = 1;

/// Maximum nested call/open/signcall depth. Re-entrancy is permitted
/// (ADR 0017), so without this a load-free A↔B cycle would be bounded
/// only by gas; the cap restores a structural bound (design.md §Calls).
const MAX_CALL_DEPTH: usize = 64;

/// A frame's executable code. The prover holds decoded, witness-bearing
/// instructions; the verifier and internal actor execution hold raw
/// bytecode and decode one instruction at a time — never materializing a
/// `Vec<Instruction>`. See ADR 0015.
pub(crate) enum Code {
    /// Pre-decoded instructions (prover witnesses inline; `String::Script`).
    Instrs(Vec<crate::ops::Instruction>),
    /// Raw bytecode, decoded on demand (verifier; `String::Opaque`).
    Bytes(Vec<u8>),
}

impl CallFrame {
    // The frame's code + cursor + lazy label table (fields `code` /
    // `cursor` / `labels`) are walked directly by these methods — exactly
    // one stream per frame, no Run nesting. `Code::Bytes` decodes on
    // demand and never builds a `Vec<Instruction>`. See ADR 0015.

    /// Returns the next instruction; `Ok(None)` at end of program. For
    /// `Code::Bytes` this decodes one instruction at the cursor and
    /// advances by its encoded length.
    pub(crate) fn next_instruction(
        &mut self,
    ) -> Result<Option<crate::ops::Instruction>, VMError> {
        let cursor = self.cursor;
        match &self.code {
            Code::Instrs(instrs) => {
                if cursor >= instrs.len() {
                    return Ok(None);
                }
                let instr = instrs[cursor].clone();
                self.cursor = cursor + 1;
                Ok(Some(instr))
            }
            Code::Bytes(bytes) => {
                if cursor >= bytes.len() {
                    return Ok(None);
                }
                let mut reader: &[u8] = &bytes[cursor..];
                let before = reader.len();
                let instr = crate::ops::Instruction::parse(&mut reader)?;
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
        match &self.code {
            Code::Instrs(instrs) => self.cursor >= instrs.len(),
            Code::Bytes(bytes) => self.cursor >= bytes.len(),
        }
    }

    /// Debits `n` gas from this frame's budget; `OutOfGas` when the
    /// budget is exhausted. Charged per fetched instruction (executed
    /// or skip-scanned), so prover (`Instrs`) and verifier (`Bytes`)
    /// meter identically — they walk the same instruction sequence.
    fn charge_gas(&mut self, n: u64) -> Result<(), VMError> {
        self.gas_used = self.gas_used.saturating_add(n);
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
                self.labels.push(self.cursor);
                Ok(())
            }
            Ordering::Less if self.labels[n] == self.cursor => Ok(()),
            _ => Err(VMError::LabelOutOfOrder),
        }
    }

    /// Moves the cursor to `label n`: immediate if already recorded,
    /// else scans forward (recording labels, not executing) until it is
    /// reached. `LabelNotFound` if the scan hits end-of-program. See
    /// ADR 0015.
    fn jump_to_label(&mut self, n: usize) -> Result<(), VMError> {
        if n < self.labels.len() {
            self.cursor = self.labels[n];
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
                Some(crate::ops::Instruction::Label(m)) => {
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

/// Identity-bearing scope tag carried by every CallFrame.
pub enum CallKind {
    /// Outer scope of an external transaction.
    ExternalRoot,

    /// Outer scope of an internal tx; dispatched to the target's method.
    InternalRoot {
        actor: ActorID,
        method: Int253,
        caller: Option<ActorID>,
        anchor: Anchor,
    },

    /// Synchronous actor-to-actor call inside an internal tx.
    ActorCall {
        actor: ActorID,
        method: Int253,
        caller: ActorID,
        anchor: Anchor,
    },

    /// `open` / `signcall` of a cell predicate. `external_context`
    /// snapshots the caller's `is_external()` at frame creation so
    /// `require_external` propagates correctly across the isolation
    /// boundary (ADR 0013).
    CellOpen {
        anchor: Anchor,
        predicate: Predicate,
        external_context: bool,
    },
}

impl CallKind {
    /// Returns the actor identity of this frame, if any. Used by the
    /// re-entrancy guard.
    pub fn actor(&self) -> Option<&ActorID> {
        match self {
            Self::InternalRoot { actor, .. } | Self::ActorCall { actor, .. } => Some(actor),
            Self::ExternalRoot | Self::CellOpen { .. } => None,
        }
    }

    /// Returns the dispatched method key, if the frame has one.
    /// `op_method` reads this; pure read.
    pub fn method(&self) -> Option<Int253> {
        match self {
            Self::InternalRoot { method, .. } | Self::ActorCall { method, .. } => Some(*method),
            Self::ExternalRoot | Self::CellOpen { .. } => None,
        }
    }

    /// Returns the caller's actor id, if any. `op_callerid` reads
    /// this — for `InternalRoot` with a `None` caller (the
    /// originating tx came from an external sender) we surface
    /// `Some(&zero_id)` via the dedicated method [`Self::caller_or_zero`]
    /// so the opcode can push all-zeros instead of erroring.
    pub fn caller(&self) -> Option<&ActorID> {
        match self {
            Self::InternalRoot { caller, .. } => caller.as_ref(),
            Self::ActorCall { caller, .. } => Some(caller),
            Self::ExternalRoot | Self::CellOpen { .. } => None,
        }
    }

    /// Returns the frame's anchor (cell-open anchor or call-entry
    /// anchor). `op_anchor` reads this. `ExternalRoot` and
    /// `ActorCall`-by-error have no anchor concept.
    pub fn anchor(&self) -> Option<Anchor> {
        match self {
            Self::InternalRoot { anchor, .. }
            | Self::ActorCall { anchor, .. }
            | Self::CellOpen { anchor, .. } => Some(*anchor),
            Self::ExternalRoot => None,
        }
    }
}

/// An isolated execution scope. Holds its own stack, run, gas budget, and
/// transient-memory cap. Created by `call`, `open`, or the outermost frame
/// of a tx.
pub struct CallFrame {
    /// Isolated stack visible to scripts in this scope.
    pub(crate) stack: Vec<Value>,

    /// The frame's executable code (decoded instructions or raw bytecode).
    code: Code,
    /// Cursor into `code`: instruction index for `Instrs`, byte offset for
    /// `Bytes`.
    cursor: usize,
    /// `labels[n]` = cursor position just after `label n`; filled lazily
    /// in appearance order. See ADR 0015.
    labels: Vec<usize>,

    /// Identity / dispatch context for this frame.
    pub(crate) kind: CallKind,

    /// Gas budget for this call.
    pub(crate) gas_limit: u64,
    pub(crate) gas_used: u64,

    /// Transient-memory cap for this call.
    pub(crate) mem_limit: u64,
    pub(crate) mem_used: u64,

    /// Vbytes delivered with this call (queryable by `newbytes` opcode).
    pub(crate) newbytes: u64,

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
    pub(crate) snap_total_fee: crate::fees::CheckedFee,

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

    /// VM `code_epoch` at frame creation. A frame whose epoch is stale
    /// (a `setcode` ran after it was created) must not harvest its
    /// label table into the cache — the positions may describe
    /// replaced code.
    pub(crate) code_epoch: u64,
}

impl CallFrame {
    /// Builds a fresh CallFrame over pre-decoded `instructions` (prover
    /// and pre-parsed paths). The verifier / internal execution uses
    /// [`CallFrame::from_bytecode`] to stream raw bytecode instead.
    pub fn new(
        instructions: Vec<crate::ops::Instruction>,
        kind: CallKind,
        gas_limit: u64,
        mem_limit: u64,
        newbytes: u64,
    ) -> Self {
        Self::from_code(Code::Instrs(instructions), kind, gas_limit, mem_limit, newbytes)
    }

    /// Builds a CallFrame that decodes raw `bytecode` on demand — no
    /// `Vec<Instruction>` is materialized. See ADR 0015.
    pub(crate) fn from_bytecode(
        bytecode: Vec<u8>,
        kind: CallKind,
        gas_limit: u64,
        mem_limit: u64,
        newbytes: u64,
    ) -> Self {
        Self::from_code(Code::Bytes(bytecode), kind, gas_limit, mem_limit, newbytes)
    }

    pub(crate) fn from_code(
        code: Code,
        kind: CallKind,
        gas_limit: u64,
        mem_limit: u64,
        newbytes: u64,
    ) -> Self {
        Self {
            stack: Vec::new(),
            code,
            cursor: 0,
            labels: Vec::new(),
            kind,
            gas_limit,
            gas_used: 0,
            mem_limit,
            mem_used: 0,
            newbytes,
            post_call_anchor: None,
            snap_txlog_len: 0,
            snap_deferred_sigs_len: 0,
            snap_total_fee: crate::fees::CheckedFee::zero(),
            snap_batch: None,
            snap_cs: None,
            code_epoch: 0,
        }
    }
}

/// Outcome of a successful transaction execution. Returned by both
/// `Prover::prove` and `Verifier::verify`.
pub struct TxResult {
    /// Canonical 32-byte transaction id.
    pub txid: crate::tx::TxID,

    /// Full txlog including the `Header` entry at index 0.
    pub txlog: Vec<crate::tx::TxEntry>,

    /// Aggregate fee in flames recorded by `op_fee`.
    pub total_fee: u64,

    /// Gas spent by all opcodes. Always zero until per-opcode gas
    /// charging is wired in.
    pub gas_used: u64,

    /// Bytes allocated against the persistent vbyte cap. Always zero
    /// until the memory-cap allocator is wired in.
    pub vbytes_used: u64,

    /// Canonical bytecode of the executed script. The prover supplies
    /// this from the `Program`; the verifier echoes back the bytecode
    /// it received. Useful when downstream code wants to re-hash or
    /// re-broadcast without re-encoding.
    pub bytecode: Vec<u8>,

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

/// Manual Debug — the linear `Cell` in `txlog` blocks `#[derive]`.
impl core::fmt::Debug for TxResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TxResult")
            .field("txid", &self.txid)
            .field("txlog.len", &self.txlog.len())
            .field("total_fee", &self.total_fee)
            .field("gas_used", &self.gas_used)
            .field("vbytes_used", &self.vbytes_used)
            .field("bytecode.len", &self.bytecode.len())
            .field("proof_present", &self.proof.is_some())
            .field("deferred_sigs.len", &self.deferred_sigs.len())
            .finish()
    }
}

pub(crate) struct VM {
    header: TxHeader,

    /// Per-tx current anchor. `None` for fresh ExternalRoot txs (the
    /// first `op_input` seeds it); `Some(M)` at the start of an
    /// internal tx (where `M` is the delivering Message's anchor —
    /// itself a split-child from the originating tx's `op_send`).
    /// Consumed-and-replaced via split by `cell` / `output` / `send`;
    /// `call` / `open` / `signcall` don't touch it (intra-tx calls
    /// don't mint new cross-tx entities).
    pub(crate) last_anchor: Option<Anchor>,

    current_call: CallFrame,
    call_stack: Vec<CallFrame>,

    /// Effects emitted during execution; used to compute TxID.
    pub(crate) txlog: Vec<crate::tx::TxEntry>,

    /// Running per-tx fee accumulator (overflow → `FeeTooHigh`).
    total_fee: crate::fees::CheckedFee,

    /// Signature checks deferred to `Delegate::finalize`.
    deferred_sigs: Vec<DeferredSig>,

    /// Bumped on every `setcode`; frames created before the bump are
    /// barred from harvesting labels (their code snapshot may be stale).
    code_epoch: u64,

    /// Per-tx label-table cache for actor code, keyed by actor id hash.
    /// An actor's code is immutable between `setcode`s, so label
    /// positions discovered by one call seed the next call's frame —
    /// repeated dispatch into the same actor skips the forward scan
    /// (ADR 0018 follow-up). Invalidated by `setcode`; dies with the tx.
    label_cache: std::collections::BTreeMap<[u8; 32], Vec<usize>>,
}

impl VM {
    /// Executes an external transaction script with the given delegate,
    /// then calls `delegate.finalize`. Crate-internal: the public path
    /// is `Program::build_tx` / `ExternalTx::verify`.
    #[cfg(test)]
    pub(crate) fn execute_external<D: Delegate>(
        header: TxHeader,
        script: Vec<u8>,
        gas_limit: u64,
        mem_limit: u64,
        mut delegate: D,
    ) -> Result<TxResult, VMError> {
        let mut vm = Self::new(
            header,
            CallFrame::from_bytecode(
                script.clone(),
                CallKind::ExternalRoot,
                gas_limit,
                mem_limit,
                0,
            ),
        );
        while vm.step_external(&mut delegate)? {}
        Ok(vm.into_result(script, None))
    }

    /// Runs an external-root program through the VM without finalizing
    /// the delegate. Used by `Prover` / `Verifier` which take a Program
    /// (with witnesses inline on the prover side).
    pub(crate) fn run<D: Delegate>(
        header: TxHeader,
        program: crate::program::Program,
        gas_limit: u64,
        mem_limit: u64,
        delegate: &mut D,
    ) -> Result<TxResult, VMError> {
        let bytecode = program.to_bytecode();
        let mut vm = Self::new(
            header,
            CallFrame::new(
                program.into_instructions(),
                CallKind::ExternalRoot,
                gas_limit,
                mem_limit,
                0,
            ),
        );
        while vm.step_external(delegate)? {}
        Ok(vm.into_result(bytecode, None))
    }

    /// Verifier entry: runs external-root **bytecode**, decoding on demand
    /// without materializing a `Vec<Instruction>`. The witness-free
    /// counterpart of [`run`]. See ADR 0015.
    pub(crate) fn run_bytecode<D: Delegate>(
        header: TxHeader,
        bytecode: Vec<u8>,
        gas_limit: u64,
        mem_limit: u64,
        delegate: &mut D,
    ) -> Result<TxResult, VMError> {
        let mut vm = Self::new(
            header,
            CallFrame::from_bytecode(
                bytecode.clone(),
                CallKind::ExternalRoot,
                gas_limit,
                mem_limit,
                0,
            ),
        );
        while vm.step_external(delegate)? {}
        Ok(vm.into_result(bytecode, None))
    }

    /// Executes an internal transaction. On clean exit runs the tx-end
    /// self-destruct sweep against `registry`.
    pub fn execute_internal(
        header: TxHeader,
        message: Message,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
    ) -> Result<TxResult, VMError> {
        // Deploy-on-first-delivery (spec §Actors): a Constructor-form
        // target carries the actor's code on the wire and the id
        // commits to it (`id = H(bytes)`), so the first message to a
        // not-yet-deployed actor instantiates it — code = constructor
        // bytes, empty state, funded by the message's vbytes.
        if !registry.exists(&message.target) {
            if let ActorID::Constructor(bytes) = &message.target {
                registry.deploy(
                    message.target.clone(),
                    bytes.clone(),
                    crate::actor::empty_state(),
                    message.vbytes,
                    block.height,
                )?;
            }
        }
        let script = registry.load_code(&message.target)?;
        let mem_limit = registry.actor_vbytes(&message.target)?.saturating_mul(4);
        // SendID is the canonical hash of the whole send (anchor,
        // target, caller, method, payload, gas, vbytes, refund
        // predicate) — analogous to CellID for Output. Capture
        // before the move below.
        let send_id = *message.id().as_bytes();
        let kind = CallKind::InternalRoot {
            actor: message.target,
            method: message.method,
            caller: message.caller,
            anchor: message.anchor,
        };
        let mut vm = Self::new(
            header,
            CallFrame::from_bytecode(
                script,
                kind,
                message.gas,
                mem_limit,
                message.vbytes,
            ),
        );
        // Commit the triggering SendID into the Internal TxID merkle
        // root. Symmetric with `op_input` for external txs: the first
        // post-Header effect identifies *what consumed-once entity*
        // brought this tx into existence.
        vm.txlog.push(crate::tx::TxEntry::Receive(send_id));

        // Tx-level checkpoint: if the script aborts at the root
        // frame (no caller to swallow into a failure marker), the
        // registry must roll back any mutations the failed run
        // wrote. Symmetric with per-frame checkpointing inside
        // `step`. Without this, an op_save that succeeded then a
        // later opcode that errored at root would persist the save
        // even though the tx aborts.
        registry.push_checkpoint();
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
        registry.pop_checkpoint_commit();
        let _cleared = registry.commit_tx_destructions(block.height);
        Ok(vm.into_result(Vec::new(), None))
    }

    fn new(header: TxHeader, initial_call: CallFrame) -> Self {
        // Header is the first txlog entry so TxID binds to version + locktime.
        let mut txlog = Vec::new();
        txlog.push(crate::tx::TxEntry::Header(header));
        // Seed last_anchor from the root frame's kind: ExternalRoot →
        // None (op_input must seed); InternalRoot → Some(Message.anchor)
        // (already unique from prior tx's op_send split).
        let last_anchor = initial_call.kind.anchor();
        Self {
            header,
            last_anchor,
            current_call: initial_call,
            call_stack: Vec::new(),
            txlog,
            total_fee: crate::fees::CheckedFee::zero(),
            deferred_sigs: Vec::new(),
            code_epoch: 0,
            label_cache: std::collections::BTreeMap::new(),
        }
    }

    /// Stores an exiting actor frame's discovered labels for reuse by a
    /// later call to the same actor in this tx. Label positions are a
    /// deterministic property of the code, so partial tables from failed
    /// frames are equally valid; keep the longest discovered prefix.
    fn harvest_labels(&mut self, exiting: &CallFrame) {
        // A setcode since this frame's creation may have replaced the
        // very code these positions describe — never re-cache them.
        if exiting.code_epoch != self.code_epoch {
            return;
        }
        if let CallKind::ActorCall { actor, .. } = &exiting.kind {
            let entry = self.label_cache.entry(actor.to_hash()).or_default();
            if exiting.labels.len() > entry.len() {
                *entry = exiting.labels.clone();
            }
        }
    }

    /// Splits `last_anchor`: returns the `left` half (to embed in
    /// a fresh unique-anchored entity — cell, message), and writes
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
        self.current_call.snap_total_fee = self.total_fee;
        Ok(left)
    }

    /// Drains the VM into a `TxResult`, computing TxID from the txlog.
    fn into_result(
        mut self,
        bytecode: Vec<u8>,
        proof: Option<R1CSProof>,
    ) -> TxResult {
        let txlog = mem::take(&mut self.txlog);
        let deferred_sigs = mem::take(&mut self.deferred_sigs);
        let txid = crate::tx::TxID::from_log(&txlog);
        TxResult {
            txid,
            txlog,
            total_fee: self.total_fee.total(),
            // Root frame's metered gas (per-instruction; spec §gas).
            gas_used: self.current_call.gas_used,
            // Vbytes flow is deferred (design.md note); not yet metered.
            vbytes_used: 0,
            bytecode,
            proof,
            deferred_sigs,
        }
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
    /// always external; `CellOpen` inherits the caller's snapshot.
    fn is_external(&self) -> bool {
        match self.current_call.kind {
            CallKind::ExternalRoot => true,
            CallKind::CellOpen { external_context, .. } => external_context,
            _ => false,
        }
    }

    /// External-context step.
    pub(crate) fn step_external<D: Delegate>(
        &mut self,
        delegate: &mut D,
    ) -> Result<bool, VMError> {
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
    /// `fail_current_call` and a `0` failure marker is pushed
    /// onto the parent's stack. Errors at the outermost frame
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
            Some(r) => (self.step_inner(delegate, Some(&mut *r)), Some(r)),
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
        let Some(instr) = self.current_call.next_instruction()? else {
            return self.finish_call();
        };
        use crate::ops::Instruction as I;
        match instr {

            I::PushInt(i) => {
                self.push_value(Value::Int253(i));
                Ok(())
            }
            I::PushStr(s) => {
                self.charge_mem(s.len())?;
                self.push_value(Value::String(s));
                Ok(())
            }
            I::PushPoint(p) => {
                // Witness-bearing variants flow through unchanged;
                // verifier-side decoded form is `Point::Opaque`.
                self.push_value(Value::Point(p));
                Ok(())
            }
            I::PushToken => self.op_pushtoken(),
            I::Drop => self.op_drop(),
            I::Nop => self.op_nop(),
            I::Dup => self.op_dup(),
            I::Roll => self.op_roll(),
            I::DupK(k) => self.op_dup_k(k as usize),
            I::RollK(k) => self.op_roll_k(k as usize),

            I::ReadBits => self.op_read_bits(),
            I::ReadInt => self.op_read_int(),
            I::ReadStr => self.op_read_str(),
            I::ReadPoint => self.op_read_point(),
            I::WriteBits => self.op_write_bits(),
            I::WriteInt => self.op_write_int(),
            I::Append => self.op_append(),
            I::WriteZeros => self.op_write_zeros(),
            I::BitNot => self.op_bit_not(),
            I::BitOr => self.op_bit_or(),
            I::BitAnd => self.op_bit_and(),
            I::BitXor => self.op_bit_xor(),
            I::ShiftLeft => self.op_shift_left(),
            I::ShiftRight => self.op_shift_right(),
            I::Keccak256 => self.op_hash::<sha3::Keccak256>(),

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
            I::Sha256 => self.op_hash::<sha2::Sha256>(),
            I::Sha512 => self.op_hash::<sha2::Sha512>(),
            I::Sha3 => self.op_hash::<sha3::Sha3_256>(),
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
            I::Cell => self.op_cell(),
            I::Output => self.op_output(),
            I::Open => self.op_open(),
            I::Send => self.op_send(),
            I::Call => self.op_call(registry),
            I::Load => self.op_load(registry),
            I::Save => self.op_save(registry),
            I::Setcode => self.op_setcode(registry),
            I::Signtx => self.op_signtx(),
            I::Signcall => self.op_signcall(),

            I::Timelock => self.op_timelock(),
            // version → tx header version (spec §version).
            I::Version => { self.push_value(Value::Int253(Int253::from(self.header.version as u64))); Ok(()) }
            I::Selfid => self.op_selfid(),
            I::Anchor => self.op_anchor(),
            I::Gas => self.op_gas(),
            I::Bytes => self.op_bytes(registry),
            I::Callerid => self.op_callerid(),
            I::Method => self.op_method(),
            // gaslimit / memlimit / newbytes → frame budget fields.
            I::Gaslimit => { self.push_value(Value::Int253(Int253::from(self.current_call.gas_limit))); Ok(()) }
            I::Memlimit => { self.push_value(Value::Int253(Int253::from(self.current_call.mem_limit))); Ok(()) }
            I::Newbytes => { self.push_value(Value::Int253(Int253::from(self.current_call.newbytes))); Ok(()) }

            I::Ext(b) => Err(VMError::UnknownOpcode(b)),
        }?;
        Ok(true)
    }

    /// Pops the current frame back to its caller on clean exit. Stack
    /// must be empty (use `return k` to send values across the boundary).
    /// Leftover gas is refunded to the parent. Implicit clean exits
    /// push `{0, 1}` onto the parent's stack (success with k=0).
    /// Shared clean-exit epilogue: pop to `parent` (already swapped in
    /// by the caller), harvest the exiting frame's labels, refund
    /// leftover gas, apply the parent's post-call anchor, and discard
    /// the entry-time batch/CS snapshots (failure-path only). Order is
    /// load-bearing — identical in `finish_call` and `op_return`.
    fn clean_exit_to_parent(&mut self, exiting: &CallFrame, leftover_gas: u64) {
        self.harvest_labels(exiting);
        self.current_call.gas_limit = self
            .current_call
            .gas_limit
            .saturating_add(leftover_gas);
        if let Some(post) = self.current_call.post_call_anchor.take() {
            self.last_anchor = Some(post);
        }
        self.current_call.snap_batch = None;
        self.current_call.snap_cs = None;
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
            let exiting = mem::replace(&mut self.current_call, parent);
            self.clean_exit_to_parent(&exiting, leftover_gas);
            // Success marker with k=0: stack += [count=0, success=1].
            self.current_call.stack.push(Value::Int253(Int253::from(0u64)));
            self.current_call.stack.push(Value::Int253(Int253::ONE));
            return Ok(true);
        }
        // Outermost call returned: entire tx complete.
        Ok(false)
    }

    /// Discards the current frame on failure: pops it without
    /// preserving its effects, rolls back side-effects from the
    /// parent's snapshot (txlog tail, deferred-sigs tail, total_fee,
    /// delegate batch state, R1CS constraint system), applies the
    /// parent's `post_call_anchor`, and pushes `0` onto the parent's
    /// stack as the failure marker. Caller's effects up to the
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
        let exiting = mem::replace(&mut self.current_call, parent);
        self.harvest_labels(&exiting);
        // Roll back side effects via the snapshots taken at call
        // entry.
        self.txlog.truncate(self.current_call.snap_txlog_len);
        self.deferred_sigs.truncate(self.current_call.snap_deferred_sigs_len);
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
        // Push failure marker.
        self.current_call.stack.push(Value::Int253(Int253::from(0u64)));
    }

    /// Pushes a value onto the current call's stack.
    fn push_value(&mut self, v: Value) {
        self.current_call.stack.push(v);
    }

    /// Pops the top value from the current call's stack.
    fn pop_value(&mut self) -> Result<Value, VMError> {
        self.current_call.stack.pop().ok_or(VMError::StackUnderflow)
    }

    /// Converts a stack-popped `Int253` index into a `usize`.
    fn int253_to_stack_index(&self, i: Int253) -> Result<usize, VMError> {
        let v = i.to_u64().ok_or(VMError::IndexOutOfRange)?;
        let idx = usize::try_from(v).map_err(|_| VMError::IndexOutOfRange)?;
        if idx >= self.current_call.stack.len() {
            return Err(VMError::IndexOutOfRange);
        }
        Ok(idx)
    }

    /// `0x1b` `pushtoken` — `flv → token`. Pops an `Int253` flavor from
    /// the stack and pushes a zero-qty `ClearToken { qty: 0, flv }`. This
    /// is the canonical "empty bearer of a flavor" used as a starting
    /// point for issuance / borrow flows.
    fn op_pushtoken(&mut self) -> Result<(), VMError> {
        let flv = self.pop_value()?.to_int253()?;
        self.push_value(Value::ClearToken(ClearToken::new(Int253::ZERO, flv)));
        Ok(())
    }

    /// `0x1c` `drop` — pops the top and discards it if droppable.
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

    /// `0x1e` `dup` — pops `k`, then copies the now-`k`-th item from top
    /// onto the top.
    fn op_dup(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_int253()?;
        self.op_dup_k(self.int253_to_stack_index(k)?)
    }

    /// `0x20..=0x2f` `dup:k` — copies `stack[top - k]` onto the top.
    /// Requires the source value to be copyable.
    fn op_dup_k(&mut self, k: usize) -> Result<(), VMError> {
        let stack = &self.current_call.stack;
        if k >= stack.len() {
            return Err(VMError::IndexOutOfRange);
        }
        let idx = stack.len() - 1 - k;
        let copy = stack[idx].try_clone()?;
        self.push_value(copy);
        Ok(())
    }

    /// `0x1f` `roll` — pops `k`, then moves the `k`-th item from top to
    /// the top.
    fn op_roll(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_int253()?;
        self.op_roll_k(self.int253_to_stack_index(k)?)
    }

    /// `0x30..=0x3f` `roll:k` — removes `stack[top - k]` and pushes it
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

    /// `0x80` `transcript` — `label → merlin`. Pops a label string,
    /// creates a fresh Merlin transcript bound to it.
    fn op_transcript(&mut self) -> Result<(), VMError> {
        let label = self.pop_value()?.to_string()?;
        self.push_value(Value::Merlin(Merlin::new(&label.to_bytes())));
        Ok(())
    }

    /// `0x81` `twrite` — `merlin label str → merlin`. Pops `str` (top),
    /// `label`, and the merlin; absorbs `(label, str)` into the
    /// transcript; pushes merlin back.
    fn op_twrite(&mut self) -> Result<(), VMError> {
        let data = self.pop_value()?.to_string()?;
        let label = self.pop_value()?.to_string()?;
        let mut m = self.pop_value()?.to_merlin()?;
        m.write_bytes(&label.to_bytes(), &data.to_bytes());
        self.push_value(Value::Merlin(m));
        Ok(())
    }

    /// `0x82` `tread` — `merlin label n → merlin str`. Squeezes `n`
    /// bytes of challenge from the transcript under `label`; pushes the
    /// merlin back, then the new String.
    fn op_tread(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        self.charge_mem(n)?;
        let label = self.pop_value()?.to_string()?;
        let mut m = self.pop_value()?.to_merlin()?;
        let out = m.read_bytes(&label.to_bytes(), n);
        self.push_value(Value::Merlin(m));
        self.push_value(Value::String(String::from(out)));
        Ok(())
    }

    /// Pops a String, pushes its hash digest. Generic over the digest
    /// `H`; dispatch fixes the concrete hash per opcode: `sha256` (`0x83`),
    /// `sha512` (`0x6d`), `sha3` (`0x6e`, FIPS-202), `keccak256` (`0x4e`,
    /// pre-FIPS Keccak, Ethereum-compatible).
    fn op_hash<H: sha2::Digest>(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        let digest = H::digest(s.to_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x6f` `log` — `str → ø`. Pops a String, emits
    /// `TxEntry::Data(bytes)` into the txlog. Witness-bearing String
    /// variants serialize via `to_bytes` so prover and verifier emit
    /// the same canonical bytes.
    fn op_log(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        self.txlog.push(crate::tx::TxEntry::Data(s.to_bytes()));
        Ok(())
    }

    /// `0x60` `dict` — `... val key val key n → dict`. Pops `n`, then `n`
    /// key/value pairs (key on top of each pair). Duplicate keys error.
    fn op_dict(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let mut dict = Dict::new();
        for _ in 0..n {
            let key = self.pop_value()?.to_int253()?;
            let value = self.pop_value()?;
            if dict.insert_strict(key, value).is_err() {
                return Err(VMError::DictKeyOccupied);
            }
        }
        self.push_value(Value::Dict(dict));
        Ok(())
    }

    /// `0x61` `put` — `dict k v → dict'`. Strict insert; fails on
    /// occupied key.
    fn op_put(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?;
        let k = self.pop_value()?.to_int253()?;
        let mut dict = self.pop_value()?.to_dict()?;
        if dict.insert_strict(k, v).is_err() {
            return Err(VMError::DictKeyOccupied);
        }
        self.push_value(Value::Dict(dict));
        Ok(())
    }

    /// `0x62` `replace` — `dict k v → dict' {prev 1 | 0}`. Overwrites
    /// the slot, returning the prior value (if any) as an optional.
    /// Stack order matches `put`: `v` on top, `k` below.
    fn op_replace(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?;
        let k = self.pop_value()?.to_int253()?;
        let mut dict = self.pop_value()?.to_dict()?;
        let prev = dict.insert(k, v);
        self.push_value(Value::Dict(dict));
        match prev {
            Some(prev_v) => {
                self.push_value(prev_v);
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::ZERO));
            }
        }
        Ok(())
    }

    /// `0x63` `get` — `dict k → dict' k v`. Removes and returns the
    /// value at `k`. Fails if the key is missing.
    fn op_get(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_int253()?;
        let mut dict = self.pop_value()?.to_dict()?;
        let v = dict.remove(&k).ok_or(VMError::DictKeyNotFound)?;
        self.push_value(Value::Dict(dict));
        self.push_value(Value::Int253(k));
        self.push_value(v);
        Ok(())
    }

    /// `0x64` `getopt` — `dict k → dict' {v 1 | 0}`. Like `get`, but
    /// soft-fails (pushes `0`) when the key is missing.
    fn op_getopt(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_int253()?;
        let mut dict = self.pop_value()?.to_dict()?;
        let v = dict.remove(&k);
        self.push_value(Value::Dict(dict));
        self.push_optional_value(v);
        Ok(())
    }

    /// `0x65` `getdup` — `dict k → dict {v 1 | 0}`. Copies the value
    /// without consuming it. Soft-fails with `0` on missing key; hard
    /// errors if the value exists but isn't copyable.
    fn op_getdup(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_int253()?;
        let dict = self.pop_value()?.to_dict()?;
        let copied = match dict.get(&k) {
            Some(v) => Some(v.try_clone()?),
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
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => self.push_value(Value::Int253(Int253::ZERO)),
        }
    }

    /// `0x66` `first` — `dict → dict {k 1 | 0}`. Pushes the smallest key
    /// alongside a flag, or `0` if the dict is empty.
    fn op_first(&mut self) -> Result<(), VMError> {
        let dict = self.pop_value()?.to_dict()?;
        let k = dict.first_key();
        self.push_value(Value::Dict(dict));
        self.push_optional_value(k.map(Value::Int253));
        Ok(())
    }

    /// `0x67` `last` — `dict → dict {k 1 | 0}`. Mirror of `first`.
    fn op_last(&mut self) -> Result<(), VMError> {
        let dict = self.pop_value()?.to_dict()?;
        let k = dict.last_key();
        self.push_value(Value::Dict(dict));
        self.push_optional_value(k.map(Value::Int253));
        Ok(())
    }

    /// `0x68` `next` — `dict k → dict {k' 1 | 0}`. Smallest key strictly
    /// greater than `k`, or `0` if no such key exists.
    fn op_next(&mut self) -> Result<(), VMError> {
        let k = self.pop_value()?.to_int253()?;
        let dict = self.pop_value()?.to_dict()?;
        let next_k = dict.next_key_after(&k);
        self.push_value(Value::Dict(dict));
        self.push_optional_value(next_k.map(Value::Int253));
        Ok(())
    }

    /// Helper: converts a stack-popped count into a `usize` ≤ `max`.
    /// Returns `IndexOutOfRange` on overflow or above `max`.
    fn pop_byte_count(&mut self, max: usize) -> Result<usize, VMError> {
        let n_int = self.pop_value()?.to_int253()?;
        let n_u64 = n_int.to_u64().ok_or(VMError::IndexOutOfRange)?;
        let n = usize::try_from(n_u64).map_err(|_| VMError::IndexOutOfRange)?;
        if n > max {
            return Err(VMError::IndexOutOfRange);
        }
        Ok(n)
    }

    /// Pushes a failure marker (`Int253(0)`) — used by `read*` opcodes
    /// when the source string is too short to satisfy the request. The
    /// original string is restored under the marker.
    fn push_read_failure(&mut self, original: String) {
        self.push_value(Value::String(original));
        self.push_value(Value::Int253(Int253::ZERO));
    }

    /// _s n_ **readbits** → _s' x 1_ | _s 0_
    ///
    /// Reads `n ≤ 256` bits LSB-first into a fresh `Int253`. Hard-fails
    /// when `n > 256` (programmer error); soft-fails on short input,
    /// non-canonical magnitude, or negative zero.
    fn op_read_bits(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(256)?;
        let s = self.pop_value()?.to_string()?;
        let n_bytes = (n + 7) / 8;
        if s.len() < n_bytes {
            self.push_read_failure(s); // restore original (witness-preserving)
            return Ok(());
        }
        let bytes = s.to_bytes(); // canonical, owned
        let mut int_bytes = [0u8; 32];
        if n_bytes > 0 {
            int_bytes[..n_bytes].copy_from_slice(&bytes[..n_bytes]);
            // Mask off bits above position n-1 within the final byte so
            // that bits `n..n_bytes*8` are forced to zero.
            let tail_bits = n % 8;
            if tail_bits != 0 {
                int_bytes[n_bytes - 1] &= (1u8 << tail_bits) - 1;
            }
        }
        // `Int253::from_bytes` enforces both canonicality (magnitude < ℓ)
        // and the negative-zero invariant. Either violation soft-fails.
        let value = match Int253::from_bytes(int_bytes) {
            Some(v) => v,
            None => {
                self.push_read_failure(String::from(bytes)); // bytes untouched
                return Ok(());
            }
        };
        self.push_value(Value::String(String::from(bytes[n_bytes..].to_vec())));
        self.push_value(Value::Int253(value));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x41` `readint` — `s → s' x 1 | s 0`. Reads the canonical
    /// 32-byte `Int253` (bit 255 = sign, bits 0..254 = magnitude) from
    /// the front of `s`. Equivalent to `readbits(s, 256)`. Soft-fails
    /// on insufficient bytes, magnitude ≥ ℓ, or negative zero.
    fn op_read_int(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        if s.len() < 32 {
            self.push_read_failure(s);
            return Ok(());
        }
        let bytes = s.to_bytes();
        let mut int_bytes = [0u8; 32];
        int_bytes.copy_from_slice(&bytes[..32]);
        let value = match Int253::from_bytes(int_bytes) {
            Some(v) => v,
            None => {
                self.push_read_failure(String::from(bytes));
                return Ok(());
            }
        };
        self.push_value(Value::String(String::from(bytes[32..].to_vec())));
        self.push_value(Value::Int253(value));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x42` `readstr` — `s n → s' s'' 1 | s 0`. Splits off the first
    /// `n` bytes of `s` as a new String.
    fn op_read_str(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let s = self.pop_value()?.to_string()?;
        if s.len() < n {
            self.push_read_failure(s);
            return Ok(());
        }
        let (remainder, consumed) = s.split_at(n).expect("length checked");
        self.push_value(Value::String(remainder));
        self.push_value(Value::String(consumed));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x43` `readpoint` — `s → s' point 1 | s 0`. Splits off the first
    /// 32 bytes of `s` as a `Point` (decompressability not validated
    /// here; later opcodes that consume the point may reject it).
    fn op_read_point(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        if s.len() < 32 {
            self.push_read_failure(s);
            return Ok(());
        }
        let bytes = s.to_bytes();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes[..32]);
        let point = Point::from_bytes(arr);
        self.push_value(Value::String(String::from(bytes[32..].to_vec())));
        self.push_value(Value::Point(point));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// _s x n_ **writebits** → _s'_
    ///
    /// Appends the low `n` bits of `x` (LSB-first) to `s`. `n` must be
    /// a multiple of 8 and ≤ 256. Sign bit included iff `n = 256`.
    fn op_write_bits(&mut self) -> Result<(), VMError> {
        // Hard-fail at n > 256 (script abort) via the `pop_byte_count` cap.
        let n = self.pop_byte_count(256)?;
        if n % 8 != 0 {
            return Err(VMError::BitCountOutOfRange);
        }
        let n_bytes = n / 8;
        let x = self.pop_value()?.to_int253()?;
        let s = self.pop_value()?.to_string()?;
        // Raw 32-byte sign-magnitude form; low `n` bits = first `n_bytes`.
        let raw = x.to_bytes();
        let appended = s.append_bytes(&raw[..n_bytes]);
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// `0x45` `writeint` — `s x → s'`. Appends the canonical 32-byte
    /// `Int253` representation of `x` to `s` (bit 255 carries the sign;
    /// bits 0..254 carry the magnitude). Equivalent to
    /// `writebits(s, x, 256)`.
    fn op_write_int(&mut self) -> Result<(), VMError> {
        let x = self.pop_value()?.to_int253()?;
        let s = self.pop_value()?.to_string()?;
        let appended = s.append_bytes(&x.to_bytes());
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// `0x46` `append` — `s s' → s''`. Concatenates two strings.
    fn op_append(&mut self) -> Result<(), VMError> {
        let s2 = self.pop_value()?.to_string()?;
        let s1 = self.pop_value()?.to_string()?;
        self.charge_mem(s2.len())?;
        self.push_value(Value::String(s1.append_bytes(&s2.to_bytes())));
        Ok(())
    }

    /// Debits `n` bytes of transient memory from the current frame
    /// (ADR 0002 arena cap). Monotonic high-water accounting — drops
    /// don't release; the cap bounds the frame's total growth, and the
    /// counter dies with the frame (failure rollback is automatic).
    /// `mem_limit == 0` means unmetered (test frames); production
    /// frames always carry a cap (4× vbytes, the `bytes` operand, or
    /// the root `Limits`).
    fn charge_mem(&mut self, n: usize) -> Result<(), VMError> {
        let f = &mut self.current_call;
        f.mem_used = f.mem_used.saturating_add(n as u64);
        if f.mem_limit > 0 && f.mem_used > f.mem_limit {
            return Err(VMError::MemLimitExceeded);
        }
        Ok(())
    }

    /// `0x47` `writezeros` — `s n → s'`. Appends `n` zero bytes.
    fn op_write_zeros(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        self.charge_mem(n)?;
        let s = self.pop_value()?.to_string()?;
        let appended = s.append_bytes(&vec![0u8; n]);
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// `0x48` `bitnot` — `s → s'`. Inverts every bit.
    fn op_bit_not(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        self.push_value(Value::String(s.bit_not()));
        Ok(())
    }

    /// `0x49` `bitor` — `a b → c`. Bitwise OR. Fails if sizes differ.
    fn op_bit_or(&mut self) -> Result<(), VMError> {
        let b = self.pop_value()?.to_string()?;
        let a = self.pop_value()?.to_string()?;
        let c = a.bit_or(&b).ok_or(VMError::BitwiseSizeMismatch)?;
        self.push_value(Value::String(c));
        Ok(())
    }

    /// `0x4a` `bitand` — `a b → c`. Bitwise AND. Fails on size mismatch.
    fn op_bit_and(&mut self) -> Result<(), VMError> {
        let b = self.pop_value()?.to_string()?;
        let a = self.pop_value()?.to_string()?;
        let c = a.bit_and(&b).ok_or(VMError::BitwiseSizeMismatch)?;
        self.push_value(Value::String(c));
        Ok(())
    }

    /// `0x4b` `bitxor` — `a b → c`. Bitwise XOR. Fails on size mismatch.
    fn op_bit_xor(&mut self) -> Result<(), VMError> {
        let b = self.pop_value()?.to_string()?;
        let a = self.pop_value()?.to_string()?;
        let c = a.bit_xor(&b).ok_or(VMError::BitwiseSizeMismatch)?;
        self.push_value(Value::String(c));
        Ok(())
    }

    /// `0x4c` `shiftleft` — `a n → b c`. Shifts `a` left by `n ≤ 256`
    /// bits; pushes the shifted string and the removed bits (zero-padded
    /// on the left).
    fn op_shift_left(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(256)?;
        let a = self.pop_value()?.to_string()?;
        let (shifted, removed) = a.shift_left(n);
        self.push_value(Value::String(shifted));
        self.push_value(Value::String(removed));
        Ok(())
    }

    /// `0x4d` `shiftright` — `a n → b c`. Mirror of `shiftleft`; removed
    /// bits are zero-padded on the right.
    fn op_shift_right(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(256)?;
        let a = self.pop_value()?.to_string()?;
        let (shifted, removed) = a.shift_right(n);
        self.push_value(Value::String(shifted));
        self.push_value(Value::String(removed));
        Ok(())
    }

    /// `0x50` `abs` — pops an `Int253`, pushes its magnitude (positive
    /// `Int253`), then pushes the original sign as `Int253` (`0` for
    /// non-negative, `1` for negative). Top of stack ends up holding
    /// the sign bit.
    fn op_abs(&mut self) -> Result<(), VMError> {
        let v = self.pop_value()?.to_int253()?;
        let sign_bit = if v.is_negative() { 1u64 } else { 0u64 };
        self.push_value(Value::Int253(v.abs()));
        self.push_value(Value::Int253(Int253::from(sign_bit)));
        Ok(())
    }

    /// _a b_ **eq** → _a b {0|1}_ (cleartext) | _a b_ **eq** → _constraint_ (CS)
    fn op_eq<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let n = self.current_call.stack.len();
        if n < 2 {
            return Err(VMError::StackUnderflow);
        }
        // CS-lift only when an operand is actually a CS type (Variable /
        // Expression). A "non-Int253" test would wrongly route String /
        // Point comparisons (e.g. the `anchor … eq verify` binding idiom)
        // into `to_expression()` and hard-fail in external context.
        let cs_involved = matches!(self.current_call.stack[n - 1], Value::Variable(_) | Value::Expression(_))
            || matches!(self.current_call.stack[n - 2], Value::Variable(_) | Value::Expression(_));
        if self.is_external() && cs_involved {
            let b = self.pop_value()?.to_expression()?;
            let a = self.pop_value()?.to_expression()?;
            self.push_value(Value::Constraint(crate::Constraint::eq(a, b)));
        } else {
            let eq = self.current_call.stack[n - 1]
                .try_eq(&self.current_call.stack[n - 2])?;
            let bit = if eq { 1u64 } else { 0u64 };
            self.push_value(Value::Int253(Int253::from(bit)));
        }
        Ok(())
    }

    /// _x_ **neg** → _-x_  (Int253 cleartext or Expression LC negate)
    fn op_neg<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let v = self.pop_value()?.neg()?;
        self.push_value(v);
        Ok(())
    }

    /// _x y_ **add** → _z_  (cleartext modulo ℓ, or LC sum)
    fn op_add<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        let r = a.add(b, self.is_external())?;
        self.push_value(r);
        Ok(())
    }

    /// _x y_ **mul** → _z_  (cleartext modulo ℓ, MSM scalar-point lift,
    /// or CS multiplier)
    fn op_mul<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        use crate::msm::{int_to_scalar, MultiscalarMul};
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        match (a, b) {
            (Value::Int253(x), Value::Int253(y)) => {
                self.push_value(Value::Int253(x * y));
                Ok(())
            }
            // scalar * point / point * scalar → MSM with one term.
            (Value::Int253(s), Value::Point(p)) | (Value::Point(p), Value::Int253(s)) => {
                self.push_value(Value::MultiscalarMul(
                    MultiscalarMul::term(int_to_scalar(s), p.to_compressed()),
                ));
                Ok(())
            }
            // scalar * MSM / MSM * scalar → scale coefficients.
            (Value::Int253(s), Value::MultiscalarMul(m))
            | (Value::MultiscalarMul(m), Value::Int253(s)) => {
                self.push_value(Value::MultiscalarMul(m.scaled(int_to_scalar(s))));
                Ok(())
            }
            // Quadratic group-element products are not defined in
            // Sigma-protocol semantics.
            (Value::Point(_), Value::Point(_))
            | (Value::Point(_), Value::MultiscalarMul(_))
            | (Value::MultiscalarMul(_), Value::Point(_))
            | (Value::MultiscalarMul(_), Value::MultiscalarMul(_)) => {
                Err(VMError::TypeNotInt253)
            }
            (a, b) if self.is_external() => {
                let aexpr = a.to_expression()?;
                let bexpr = b.to_expression()?;
                let product = aexpr.multiply(bexpr, delegate.cs());
                self.push_value(Value::Expression(product));
                Ok(())
            }
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// `0x55` `divmod` — `x z → d r`. Truncated division: `sign(d) =
    /// sign(x) XOR sign(z)`, `sign(r) = sign(x)`. Errors on zero divisor.
    fn op_divmod(&mut self) -> Result<(), VMError> {
        let z = self.pop_value()?.to_int253()?;
        let x = self.pop_value()?.to_int253()?;
        let (d, r) = x.div_rem(z).ok_or(VMError::DivByZero)?;
        self.push_value(Value::Int253(d));
        self.push_value(Value::Int253(r));
        Ok(())
    }

    /// `0x56` `mod252` — pops a `String` of 0..=64 bytes, interprets it
    /// as a little-endian unsigned integer, reduces it modulo ℓ, and
    /// pushes the result as a non-negative `Int253`.
    fn op_mod252(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        let bytes = s.to_bytes();
        if bytes.len() > 64 {
            return Err(VMError::StringTooLongForModReduction);
        }
        let mut buf = [0u8; 64];
        buf[..bytes.len()].copy_from_slice(&bytes);
        let scalar = Scalar::from_bytes_mod_order_wide(&buf);
        self.push_value(Value::Int253(Int253::from(scalar)));
        Ok(())
    }

    /// _x_ **not** → _y_  (Int253 logical, or Constraint negation)
    fn op_not<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let v = self.pop_value()?.not()?;
        self.push_value(v);
        Ok(())
    }

    /// _a b_ **and** → _c_  (Int253 logical, or Constraint conjunction)
    fn op_and<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        let r = a.and(b, self.is_external())?;
        self.push_value(r);
        Ok(())
    }

    /// _a b_ **or** → _c_  (Int253 logical, or Constraint disjunction)
    fn op_or<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        let r = a.or(b, self.is_external())?;
        self.push_value(r);
        Ok(())
    }

    /// `0x5f` `size` — peeks the top value and pushes its length as an
    /// `Int253` (String byte count, Dict entry count). Other types
    /// error with `TypeHasNoLength`.
    fn op_size(&mut self) -> Result<(), VMError> {
        let top = self
            .current_call
            .stack
            .last()
            .ok_or(VMError::StackUnderflow)?;
        let len = match top {
            Value::String(s) => s.len(),
            Value::Dict(d) => d.len(),
            _ => return Err(VMError::TypeHasNoLength),
        };
        self.push_value(Value::Int253(Int253::from(len as u64)));
        Ok(())
    }

    /// `0x79 verify` — pop one and assert truthiness.
    ///
    /// - `Int253`: errors `VerifyFailed` if zero, else pops.
    /// - `Constraint`: hands the constraint to the CS so the proof
    ///   commits to its truth. Requires external context.
    /// - `MultiscalarMul`: appends `sum(s_i · P_i) == identity` to
    ///   the delegate's `BatchVerifier` (alongside Schnorr/Musig
    ///   sigs). Requires external context.
    fn op_verify<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        match self.pop_value()? {
            Value::Int253(v) => {
                if v.is_zero() {
                    return Err(VMError::VerifyFailed);
                }
                Ok(())
            }
            Value::Constraint(c) => {
                self.require_external()?;
                c.verify(delegate.cs())?;
                Ok(())
            }
            Value::MultiscalarMul(m) => {
                self.require_external()?;
                let terms = m.into_terms();
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
                Err(VMError::TypeNotInt253)
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

    /// _x_ **jumpif:n** → ø — pop an Int253; jump to label `n` iff `x ≠ 0`.
    fn op_jumpif(&mut self, n: u32) -> Result<(), VMError> {
        let x = self.pop_value()?.to_int253()?;
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
        let k_int = self.pop_value()?.to_int253()?;
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
        let exiting = mem::replace(&mut self.current_call, parent);
        self.clean_exit_to_parent(&exiting, leftover_gas);
        // Pour return values, then count, then success marker (1).
        self.current_call.stack.extend(return_values);
        self.current_call.stack.push(Value::Int253(Int253::from(k as u64)));
        self.current_call.stack.push(Value::Int253(Int253::ONE));
        Ok(())
    }

    /// `0x7f` `type` — peeks the top value and pushes its type code as an
    /// `Int253`. The original value remains on the stack underneath.
    fn op_type(&mut self) -> Result<(), VMError> {
        let code = self
            .current_call
            .stack
            .last()
            .ok_or(VMError::StackUnderflow)?
            .type_code();
        self.push_value(Value::Int253(Int253::from(code as u64)));
        Ok(())
    }

    fn pop_string_32(&mut self) -> Result<[u8; 32], VMError> {
        let s = self.pop_value()?.to_string()?;
        crate::string::array32(&s.to_bytes()).ok_or(VMError::MalformedAddress)
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
                self.push_value(Value::Int253(qty));
                self.push_value(Value::Int253(flv));
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

    /// _qty:Int253 tag_ **issuepub** → _CT_
    ///
    /// Cleartext mint under the enclosing actor's identity. Requires
    /// `CallKind::ActorCall`; from `ExternalRoot` errors
    /// `OpcodeRequiresActorContext`, from `CellOpen` the same (issuance
    /// domains are disjoint — see spec.md §issuepub). Non-`Int253` qty
    /// hard-fails `TypeNotInt253`; the confidential path lives in
    /// [`op_issuepriv`].
    fn op_issuepub(&mut self) -> Result<(), VMError> {
        let tag = self.pop_value()?.to_string()?;
        let qty = match self.pop_value()? {
            Value::Int253(i) => i,
            _ => return Err(VMError::TypeNotInt253),
        };
        let actor = self.require_actor()?.clone();
        let flv = flavor_from_actor(&actor, &tag);
        // Cleartext qty + flv go straight into the txlog as `Int253`s —
        // no commitment indirection. The `IssuePub` entry is publicly
        // auditable directly on the wire.
        self.txlog.push(crate::tx::TxEntry::IssuePub(qty, flv));
        self.push_value(Value::ClearToken(ClearToken::new(qty, flv)));
        Ok(())
    }

    /// _qty:Variable tag_ **issuepriv** → _T_
    ///
    /// Confidential mint under the enclosing predicate's identity.
    /// Requires a `CallKind::CellOpen` frame (errors
    /// `OpcodeRequiresPredicateContext` otherwise) AND external
    /// context (errors `ExternalOnly`; the CS lane is needed for the
    /// range proof and the qty commitment registration).
    fn op_issuepriv<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        // Snapshot the predicate before any pop, so a wrong frame
        // surfaces before we mutate the stack.
        let predicate = match &self.current_call.kind {
            CallKind::CellOpen { predicate, .. } => predicate.clone(),
            _ => return Err(VMError::OpcodeRequiresPredicateContext),
        };
        self.require_external()?;
        use spacesuit::BitRange;

        let tag = self.pop_value()?.to_string()?;
        let qty_var = self.pop_value()?.to_variable()?;
        // Register the Pedersen commitment with the CS. On the prover
        // the open assignment travels through; on the verifier only
        // the closed point. Same r1cs::Variable on both sides.
        let (_qty_point, qty_r1cs) = delegate.commit_variable(&qty_var.commitment)?;
        // 64-bit range proof on the qty commitment — `borrow`/`mix`
        // already enforce this for confidential token paths.
        let qty_assignment = match qty_var.commitment.assignment() {
            Some(i) => Some(int253_to_signed_integer(i)?),
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

        self.txlog.push(crate::tx::TxEntry::IssuePriv(
            qty_var.commitment.to_point(),
            flv_commit.to_point(),
        ));
        self.push_value(Value::Token(crate::Token::new(
            qty_var.commitment,
            flv_commit,
        )));
        Ok(())
    }

    /// _token_ **retire** → ø
    fn op_retire(&mut self) -> Result<(), VMError> {
        let val = self.pop_value()?;
        match val {
            Value::ClearToken(t) => {
                let qty_commit = Commitment::unblinded(t.qty());
                let flv_commit = Commitment::unblinded(t.flv());
                self.txlog.push(crate::tx::TxEntry::Retire(
                    qty_commit.to_point(),
                    flv_commit.to_point(),
                ));
                Ok(())
            }
            Value::Token(t) => {
                self.txlog.push(crate::tx::TxEntry::Retire(
                    t.qty.to_point(),
                    t.flv.to_point(),
                ));
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
    /// Cleartext branch for `Int253` operands; encrypted (`Variable`)
    /// branch for CS-bound operands.
    fn op_borrow<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let flv_val = self.pop_value()?;
        let qty_v = self.pop_value()?;
        match (qty_v, flv_val) {
            (Value::Int253(qty), Value::Int253(flv)) => {
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
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// _a b_ **merge** → _{c 1 | a b 0}_
    fn op_merge(&mut self) -> Result<(), VMError> {
        let b = self.pop_value()?.to_clear_token()?;
        let a = self.pop_value()?.to_clear_token()?;
        match a.merge_into(b) {
            Ok(c) => {
                self.push_value(Value::ClearToken(c));
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            Err((a, b)) => {
                self.push_value(Value::ClearToken(a));
                self.push_value(Value::ClearToken(b));
                self.push_value(Value::Int253(Int253::ZERO));
            }
        }
        Ok(())
    }

    /// _a q_ **split** → _a' b_
    fn op_split(&mut self) -> Result<(), VMError> {
        let q = self.pop_value()?.to_int253()?;
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
    /// `flavor_from_actor(cid, tag)` as `Int253`. Pure helper: no CS,
    /// no txlog effect, no actor-context requirement. Use to
    /// recompute and verify a cleartext token's flavor without
    /// running the corresponding `issuepub`.
    fn op_issuepubflv(&mut self) -> Result<(), VMError> {
        let tag = self.pop_value()?.to_string()?;
        let cid = self.pop_value()?.to_string()?;
        let bytes = crate::string::array32(&cid.to_bytes()).ok_or(VMError::IndexOutOfRange)?;
        let flv = flavor_from_actor(&ActorID::Hash(bytes), &tag);
        self.push_value(Value::Int253(flv));
        Ok(())
    }

    /// _pred tag_ **issueprivflv** → _int_
    ///
    /// Consumer-side helper for `issuepriv`: pops `tag` (String) and
    /// `pred` (String, exactly 32 bytes — a compressed Ristretto
    /// predicate point), pushes `flavor_from_predicate(pred, tag)` as
    /// `Int253`. Pure helper: no CS, no txlog effect, no
    /// predicate-context requirement. Use to recompute and verify a
    /// confidential token's flavor (the flv commitment is unblinded,
    /// so its point determines the scalar).
    fn op_issueprivflv(&mut self) -> Result<(), VMError> {
        let tag = self.pop_value()?.to_string()?;
        let pred = self.pop_value()?.to_string()?;
        let bytes = crate::string::array32(&pred.to_bytes()).ok_or(VMError::IndexOutOfRange)?;
        let predicate = crate::Predicate::opaque(
            curve25519_dalek::ristretto::CompressedRistretto(bytes),
        );
        let flv = flavor_from_predicate(&predicate, &tag);
        self.push_value(Value::Int253(flv));
        Ok(())
    }

    /// Pops `n` values from the stack and returns them in caller-pushed
    /// order (deepest first). No type or portability check.
    fn pop_n_values(&mut self, n: usize) -> Result<Vec<Value>, VMError> {
        if self.current_call.stack.len() < n {
            return Err(VMError::StackUnderflow);
        }
        let start = self.current_call.stack.len() - n;
        Ok(self.current_call.stack.drain(start..).collect())
    }

    /// Pops `n` values, asserting each is portable. Used by `cell` /
    /// `output` to enforce the cell-payload invariant.
    fn pop_n_portable(&mut self, n: usize) -> Result<Vec<Value>, VMError> {
        let values = self.pop_n_values(n)?;
        for v in &values {
            if !v.is_portable() {
                return Err(VMError::NonPortableInOutput);
            }
        }
        Ok(values)
    }

    // (CallProof is now constructed from distinct stack pieces; see
    // `callproof_from_stack_pieces` below `op_open`. The earlier packed
    // bag-of-bytes layout was replaced per Architect's response on todo
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

    /// _string_ **input** → _cell_
    ///
    /// External-only. The prover pushes a `String::Cell(c)` carrying
    /// open commitments on Token payloads; the verifier pushes
    /// `String::Opaque(cell_bytes)` and `to_cell()` decodes to closed
    /// commitments. No separate witness operand — witnesses ride the
    /// stack with the value.
    fn op_input(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let cell = self.pop_value()?.to_string()?.to_cell()?;
        // Seed the per-tx anchor from the input cell's id — the cell
        // is a spend-once source on the wire, so its id is unique. Any
        // prior `last_anchor` (e.g. unused residue from a previous
        // input + outputs sequence) is replaced. See spec §Anchors.
        self.last_anchor = Some(Anchor(cell.id()));
        self.txlog.push(crate::tx::TxEntry::Input(cell.id()));
        self.push_value(Value::Cell(cell));
        Ok(())
    }

    /// _args… k pred_ **cell** → _cell_
    fn op_cell(&mut self) -> Result<(), VMError> {
        let pred = self.pop_value()?.to_point()?.to_predicate()?;
        let k = self.pop_byte_count(usize::MAX)?;
        let payload = self.pop_n_portable(k)?;
        let anchor = self.consume_anchor()?;
        let cell = Cell::new(pred, anchor, payload);
        self.push_value(Value::Cell(cell));
        Ok(())
    }

    /// _args… k pred_ **output** → ø
    fn op_output(&mut self) -> Result<(), VMError> {
        let pred = self.pop_value()?.to_point()?.to_predicate()?;
        let k = self.pop_byte_count(usize::MAX)?;
        let payload = self.pop_n_portable(k)?;
        let anchor = self.consume_anchor()?;
        let cell = Cell::new(pred, anchor, payload);
        self.txlog.push(crate::tx::TxEntry::Output(cell));
        Ok(())
    }

    /// _cell ik nbrs pos script gas bytes args… k_ **open** → _results… k'_
    ///
    /// Verifies the call-proof, then enters the unlocked script in an
    /// isolated `CellOpen` frame via [`enter_cell_open_frame`].
    fn op_open(&mut self) -> Result<(), VMError> {
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;
        let (gas, bytes) = self.pop_gas_bytes()?;
        // The callee's budget comes out of the caller's: debit the full
        // grant now; leftover is refunded on clean return, burned on
        // failure.
        self.current_call.charge_gas(gas)?;
        let prog = self.pop_value()?.to_string()?;
        let position = self.pop_value()?.to_string()?;
        let neighbors = self.pop_value()?.to_dict()?;
        let internal_key = self.pop_value()?.to_point()?;
        let cell = self.pop_value()?.to_cell()?;

        let cp = Self::callproof_from_stack_pieces(
            internal_key,
            &neighbors,
            &position,
            &prog,
        )?;
        let _ = cell.predicate.verify_callproof(&cp)?;
        // `Script` keeps prover witnesses inline; `Opaque` streams bytes
        // (no parse) on the verifier. See ADR 0015.
        let code = prog.into_code()?;
        // Split parent's anchor for the callee + stash post-call.
        let child_anchor = self.split_anchor_for_call()?;
        self.enter_cell_open_frame(cell, code, gas, bytes, args, child_anchor)?;
        Ok(())
    }

    /// _cell script sig gas bytes args… m_ **signcall** → _results… k'_
    ///
    /// Defers an Explicit signature over `script` and enters it in an
    /// isolated `CellOpen` frame via [`enter_cell_open_frame`].
    fn op_signcall(&mut self) -> Result<(), VMError> {
        let m = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(m)?;
        let (gas, bytes) = self.pop_gas_bytes()?;
        // Debit the grant from the caller (see op_open).
        self.current_call.charge_gas(gas)?;
        let sig_bytes = self.pop_value()?.to_string()?.to_bytes();
        let prog_str = self.pop_value()?.to_string()?;
        let cell = self.pop_value()?.to_cell()?;
        if sig_bytes.len() != 64 {
            return Err(VMError::BadSignatureBytes);
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_bytes);
        // Canonical bytecode for the signed message; `prog_str` is kept
        // for `to_instructions()` (witness-preserving) just below.
        let msg = Self::signcall_message(&prog_str.to_bytes_vec());
        self.deferred_sigs.push(DeferredSig::Explicit {
            verification_key: cell.predicate.verification_key(),
            message: msg,
            signature: sig,
        });

        let code = prog_str.into_code()?;
        let child_anchor = self.split_anchor_for_call()?;
        self.enter_cell_open_frame(cell, code, gas, bytes, args, child_anchor)?;
        Ok(())
    }

    /// Shared tail of `op_open` / `op_signcall`: build a new
    /// `CellOpen` frame snapshotting the caller's CS context, pour
    /// `cell.payload` then `args` onto the new stack, swap the
    /// parent out, and switch the active anchor to `child_anchor`
    /// (the `left` half of the parent's call-entry split). Memory
    /// cap equals `bytes` (no actor → no `4 × vbytes` rule).
    fn enter_cell_open_frame(
        &mut self,
        cell: Cell,
        code: Code,
        gas: u64,
        bytes: u64,
        args: Vec<Value>,
        child_anchor: Anchor,
    ) -> Result<(), VMError> {
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(VMError::CallDepthExceeded);
        }
        let external_context = self.is_external();
        let mut frame = CallFrame::from_code(
            code,
            CallKind::CellOpen {
                anchor: child_anchor,
                predicate: cell.predicate.clone(),
                external_context,
            },
            gas,
            /*mem_limit=*/ bytes,
            /*newbytes=*/ bytes,
        );
        for v in cell.payload {
            frame.stack.push(v);
        }
        for v in args {
            frame.stack.push(v);
        }
        let parent = core::mem::replace(&mut self.current_call, frame);
        self.call_stack.push(parent);
        self.last_anchor = Some(child_anchor);
        Ok(())
    }

    /// Pops `bytes` then `gas` (in that order — `gas` is deeper) as
    /// non-negative `u64`. Shared by `op_open`, `op_signcall`,
    /// `op_call`, `op_send`.
    fn pop_gas_bytes(&mut self) -> Result<(u64, u64), VMError> {
        let bytes = self
            .pop_value()?
            .to_int253()?
            .to_u64()
            .ok_or(VMError::InvalidBitrange)?;
        let gas = self
            .pop_value()?
            .to_int253()?
            .to_u64()
            .ok_or(VMError::InvalidBitrange)?;
        Ok((gas, bytes))
    }

    /// Builds a `CallProof` from the four stack-popped pieces. `neighbors`
    /// must be a list-style Dict of 32-byte Strings.
    fn callproof_from_stack_pieces(
        internal_key: Point,
        neighbors: &Dict,
        position: &String,
        program: &String,
    ) -> Result<CallProof, VMError> {
        let mut n_vec = Vec::with_capacity(neighbors.len());
        for (i, (k, v)) in neighbors.entries().enumerate() {
            if *k != Int253::from(i as u64) {
                return Err(VMError::MalformedCallProof);
            }
            match v {
                Value::String(s) => {
                    if s.len() != 32 {
                        return Err(VMError::MalformedCallProof);
                    }
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&s.to_bytes_vec());
                    n_vec.push(h);
                }
                _ => return Err(VMError::MalformedCallProof),
            }
        }
        Ok(CallProof {
            internal_key: internal_key.to_compressed(),
            neighbors: n_vec,
            position: position.to_bytes_vec(),
            program: program.to_bytes_vec(),
        })
    }

    /// _args… k refund gas bytes method addr_ **send** → ø
    ///
    /// Queues a [`Message`] for the consensus layer to instantiate as a
    /// future internal tx and emits a `TxEntry::Send`. The anchor is
    /// ratcheted from `last_anchor` before the entry is appended.
    fn op_send(&mut self) -> Result<(), VMError> {
        let target = ActorID::Hash(self.pop_string_32()?);
        let method = Int253::from(self.pop_value()?.to_int253()?);
        let (gas, vbytes) = self.pop_gas_bytes()?;
        let refund_predicate = Predicate::opaque(
            curve25519_dalek::ristretto::CompressedRistretto(self.pop_string_32()?),
        );
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;

        for v in &args {
            if !v.is_portable() {
                return Err(VMError::NonPortableInSend);
            }
        }

        let anchor = self.consume_anchor()?;
        let caller = self.current_call.kind.actor().cloned();
        // Single source of truth: the full Message lives in the
        // TxLog as `TxEntry::Send(Message)` — symmetric with
        // `TxEntry::Output(Cell)`. The block builder scans these
        // entries to construct internal-tx deliveries; no separate
        // queue.
        let message = crate::send::Message {
            target,
            method,
            caller,
            anchor,
            payload: args,
            gas,
            vbytes,
            refund_predicate,
        };
        self.txlog.push(crate::tx::TxEntry::Send(message));
        Ok(())
    }

    /// _args… k gas bytes method addr_ **call** → _results…_
    ///
    /// Synchronous actor-to-actor call. Re-entrancy guard rejects direct
    /// or indirect cycles. Emits no txlog entry — calls are intra-tx
    /// control flow; the callee's state mutation (if any) is recorded
    /// later via `TxEntry::ActorSave` when `op_save` runs.
    fn op_call(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let callee = ActorID::Hash(self.pop_string_32()?);
        let method = Int253::from(self.pop_value()?.to_int253()?);
        let (gas, vbytes) = self.pop_gas_bytes()?;
        // Debit the grant from the caller (see op_open). A caller that
        // can't afford the grant hard-fails OutOfGas — its own budget
        // is exhausted, not a soft "callee unavailable" marker.
        self.current_call.charge_gas(gas)?;
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;

        // Pre-frame setup. Any failure here ("cannot enter callee")
        // converts to a `0` failure marker on the caller's stack —
        // the call simply "did not happen" from the caller's POV. A
        // re-entrant call into an actor that's mid-update lands here
        // too: its state is checked out, so `resolve_method` returns
        // `ActorEmpty` (ADR 0017 — the state is the re-entrancy lock).
        let pre_frame: Result<(Vec<u8>, u64, ActorID), VMError> = (|| {
            if self.call_stack.len() >= MAX_CALL_DEPTH {
                return Err(VMError::CallDepthExceeded);
            }
            let script = registry.load_code(&callee)?;
            let mem_limit = registry.actor_vbytes(&callee)?.saturating_mul(4);
            let caller = self
                .current_call
                .kind
                .actor()
                .cloned()
                .unwrap_or(ActorID::Hash([0u8; 32]));
            Ok((script, mem_limit, caller))
        })();
        let (script, mem_limit, caller) = match pre_frame {
            Ok(v) => v,
            Err(_) => {
                // Pre-frame failure (reentrancy, missing actor, etc.):
                // push marker `0`, no frame created, no rollback needed.
                // The call "did not happen" — refund the debited grant.
                self.current_call.gas_used =
                    self.current_call.gas_used.saturating_sub(gas);
                self.push_value(Value::Int253(Int253::from(0u64)));
                return Ok(());
            }
        };

        // Split the parent's anchor: `left` (callee_anchor) seeds
        // the callee's `last_anchor`; `right` is stashed on the
        // parent frame's `post_call_anchor` for restoration on
        // return (success or failure). Also snapshots side-effect
        // cursors so we can roll back if the child errors out.
        let callee_anchor = self.split_anchor_for_call()?;

        let callee_hash = callee.to_hash();
        let mut frame = CallFrame::from_bytecode(
            script,
            CallKind::ActorCall {
                actor: callee,
                method,
                caller,
                anchor: callee_anchor,
            },
            gas,
            mem_limit,
            vbytes,
        );
        frame.code_epoch = self.code_epoch;
        // Seed labels discovered by earlier calls to this actor in this
        // tx — repeated dispatch skips the forward scan.
        if let Some(cached) = self.label_cache.get(&callee_hash) {
            frame.labels = cached.clone();
        }
        for v in args {
            frame.stack.push(v);
        }

        let parent = core::mem::replace(&mut self.current_call, frame);
        self.call_stack.push(parent);
        // Switch the active anchor to the callee's half.
        self.last_anchor = Some(callee_anchor);
        Ok(())
    }

    /// **load** → _dict_
    ///
    /// **Checks out** the current actor's state: moves the Dict out of
    /// the registry (the actor goes empty) and pushes it onto the
    /// stack. While checked out, any call/load against this actor fails
    /// `ActorEmpty` — the state's presence is the re-entrancy lock (ADR
    /// 0017). A frame must `save` it back (or dismantle it) before
    /// returning, per the frame-end clean-stack rule; a load left
    /// unmatched at tx end self-destructs the actor (Q6). Conventional
    /// shape `{0x00 → public, 0x01 → private}` (spec.md §Actors), not
    /// VM-enforced. Re-loading an already-checked-out actor → `ActorEmpty`.
    fn op_load(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let actor = self.require_actor()?.clone();
        let state = registry.load_state(&actor)?;
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
    /// must be portable (`Int253`, `String`, `Point`, `Dict` of
    /// portable, non-negative `ClearToken`, `Token`). Non-portable
    /// values (`Cell`, `Merlin`, `Variable`, `Expression`,
    /// `Constraint`, `MultiscalarMul`, `WideToken`, negative
    /// `ClearToken`) hard-fail `NonPortableInState`.
    fn op_save(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let actor = ActorID::Hash(self.require_actor()?.to_hash());
        let state = self.pop_value()?;
        // Portability is the canonical storage gate — checked here
        // before any registry mutation so a bad state is rejected
        // cleanly. Distinct from encodability (which the encoder may
        // or may not implement for a given variant).
        if !state.is_portable() {
            return Err(VMError::NonPortableInState);
        }
        // Rust-level deep clone for the txlog entry. The registry
        // takes ownership of one copy; the txlog gets another.
        // `clone` ignores VM stack-copyability rules so portable
        // linear values (Token) survive — those are exactly what
        // actor state is for.
        let state_for_log = state.clone();
        // Moves the state back in; errors `SaveWithoutLoad` if the
        // actor isn't checked out (no matching `load`).
        registry.save_state(&actor, state)?;
        // Structural effect. State-machine replay applies these
        // last-write-wins per actor; the merkle leaf hashes
        // `state_root(state)`, while the entry carries the full
        // Dict for direct consumers.
        self.txlog.push(crate::tx::TxEntry::ActorSave {
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
    fn op_setcode(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let actor = ActorID::Hash(self.require_actor()?.to_hash());
        let code = self.pop_value()?.to_string()?.to_bytes_vec();
        registry.set_code(&actor, code.clone())?;
        // New code → old label positions are invalid; bump the epoch so
        // in-flight frames (which may still run the old code) can't
        // re-harvest stale positions at exit.
        self.label_cache.remove(&actor.to_hash());
        self.code_epoch += 1;
        self.txlog.push(crate::tx::TxEntry::SetCode { actor, code });
        Ok(())
    }

    /// _cell_ **signtx** → _items… k_
    ///
    /// Defers a TxID-bound signature for the cell's predicate, pours
    /// the cell's payload onto the stack, pushes `k`.
    fn op_signtx(&mut self) -> Result<(), VMError> {
        let cell = self.pop_value()?.to_cell()?;
        let k = cell.payload.len();
        self.deferred_sigs.push(DeferredSig::TxBound {
            verification_key: cell.predicate.verification_key(),
            cell_id: cell.id(),
        });
        for v in cell.payload {
            self.push_value(v);
        }
        self.push_value(Value::Int253(Int253::from(k as u64)));
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
    /// has been claimed yet — same rule as `cell` / `output` /
    /// `send`. Available in either context.
    fn op_anchor(&mut self) -> Result<(), VMError> {
        let a = self.last_anchor.ok_or(VMError::AnchorMissing)?;
        self.push_value(Value::String(String::from(a.0.to_vec())));
        Ok(())
    }

    /// **callerid** → _string_
    ///
    /// Pushes the caller actor id, or all-zero String when the
    /// originator is an external send.
    fn op_callerid(&mut self) -> Result<(), VMError> {
        self.require_actor()?;
        let bytes = self.current_call.kind.caller().map(|c| c.to_hash()).unwrap_or([0u8; 32]);
        self.push_value(Value::String(String::from(bytes.to_vec())));
        Ok(())
    }

    /// **method** → _int_
    fn op_method(&mut self) -> Result<(), VMError> {
        let m = self.current_call.kind.method().ok_or(VMError::OpcodeRequiresActorContext)?;
        self.push_value(Value::Int253(m));
        Ok(())
    }

    /// **timelock** → _n {0|1}_
    ///
    /// Pushes the transaction's `locktime` and a unit flag
    /// (0 = block height, 1 = Unix timestamp). Bitcoin BIP-65
    /// convention: `flag = 1` iff `locktime >= LOCKTIME_TIMESTAMP_THRESHOLD`
    /// (i.e. ≥ Tue 2025-11-05 = 500_000_000 = ~1985-11-05 Unix epoch).
    fn op_timelock(&mut self) -> Result<(), VMError> {
        let lt = self.header.locktime as u64;
        let flag: u64 = if lt >= LOCKTIME_TIMESTAMP_THRESHOLD as u64 { 1 } else { 0 };
        self.push_value(Value::Int253(Int253::from(lt)));
        self.push_value(Value::Int253(Int253::from(flag)));
        Ok(())
    }

    /// **gas** → _n_  Pushes remaining gas for the current call.
    fn op_gas(&mut self) -> Result<(), VMError> {
        let remaining = self
            .current_call
            .gas_limit
            .saturating_sub(self.current_call.gas_used);
        self.push_value(Value::Int253(Int253::from(remaining)));
        Ok(())
    }

    /// **bytes** → _n_  Pushes the current actor's remaining
    /// persistent vbyte balance. Internal-only (requires a registry
    /// and an actor identity).
    fn op_bytes(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let actor = self.require_actor()?;
        let balance = registry.actor_vbytes(actor)?;
        self.push_value(Value::Int253(Int253::from(balance)));
        Ok(())
    }

    /// **alloc** → _expr_
    ///
    /// Allocates a low-level R1CS variable. Witness `Some(i)` on the
    /// prover; `None` on the verifier.
    fn op_alloc<D: Delegate>(
        &mut self,
        witness: Option<Int253>,
        delegate: &mut D,
    ) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::r1cs::ConstraintSystem;
        let witness_scalar = witness.map(|i| i.to_scalar_mod_order());
        let r1cs_var = delegate
            .cs()
            .allocate(witness_scalar)
            .map_err(VMError::R1CSError)?;
        let expr = crate::Expression::LinearCombination(
            vec![(r1cs_var, curve25519_dalek::scalar::Scalar::ONE)],
            witness,
        );
        self.push_value(Value::Expression(expr));
        Ok(())
    }

    /// `0x5d expr` — `var → expr`. Pops a `Variable`, calls
    /// `delegate.commit_variable` to allocate a CS-side variable for
    /// the commitment, pushes a one-term Expression.
    fn op_expr<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use curve25519_dalek::scalar::Scalar;
        let var = self.pop_value()?.to_variable()?;
        let (_point, r1cs_var) = delegate.commit_variable(&var.commitment)?;
        let witness = var.commitment.assignment();
        let expr = crate::Expression::LinearCombination(
            vec![(r1cs_var, Scalar::ONE)],
            witness,
        );
        self.push_value(Value::Expression(expr));
        Ok(())
    }

    /// _expr n_ **range** → _expr_
    ///
    /// Pops the bit-count and Expression, adds an `n`-bit non-negativity
    /// range proof, pushes the Expression back.
    fn op_range<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::r1cs::LinearCombination as LC;
        use spacesuit::BitRange;

        // Pop n (bit-count) — must be a non-negative Int253 in [1, 64].
        let n_int = self.pop_value()?.to_int253()?;
        let n_u64 = n_int.to_u64().ok_or(VMError::BitCountOutOfRange)?;
        let n_usize = usize::try_from(n_u64).map_err(|_| VMError::BitCountOutOfRange)?;
        if n_usize == 0 {
            return Err(VMError::BitCountOutOfRange);
        }
        let bit_range = BitRange::new(n_usize).ok_or(VMError::BitCountOutOfRange)?;

        let expr = self.pop_value()?.to_expression()?;

        match &expr {
            crate::Expression::Constant(value) => {
                // Cleartext: the value must fit in [0, 2^n). Negative
                // or too-large constants are caught here without
                // touching the CS.
                if !int_fits_in_n_bits(*value, n_usize) {
                    return Err(VMError::InvalidBitrange);
                }
                self.push_value(Value::Expression(expr));
                Ok(())
            }
            crate::Expression::LinearCombination(terms, assignment) => {
                let lc: LC = terms.iter().cloned().collect();
                // Convert the witness (if present) to spacesuit's
                // SignedInteger. Non-negative Int253s up to u64::MAX
                // map cleanly; anything else fails the prover at
                // gadget time via `to_u64() → None` inside
                // spacesuit::range_proof.
                let assignment_si = match assignment {
                    Some(i) => Some(int253_to_signed_integer(*i)?),
                    None => None,
                };
                spacesuit::range_proof(delegate.cs(), lc, assignment_si, bit_range)
                    .map_err(VMError::R1CSError)?;
                self.push_value(Value::Expression(expr));
                Ok(())
            }
        }
    }

    /// `0x5a scalar` — `string → expr`. Pops a String, downcasts to
    /// `Int253` via `String::to_scalar`, pushes `Expression::Constant`.
    /// For `String::Opaque(bytes)`, the bytes are parsed as a
    /// canonical sign-magnitude Int253. For `String::Scalar(i)`, the
    /// witness is extracted directly.
    fn op_scalar(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_value()?.to_string()?;
        let int = s.to_scalar()?;
        self.push_value(Value::Expression(crate::Expression::constant(int)));
        Ok(())
    }

    /// _s_ **commit** → _var_
    fn op_commit(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_value()?.to_string()?;
        let commitment = s.to_commitment()?;
        let var = crate::Variable { commitment };
        self.push_value(Value::Variable(var));
        Ok(())
    }

    /// Encrypted-branch body for `0x73 borrow`. Caller has already
    /// popped `(qty, flv)` and verified both are `Variable`; this
    /// just runs the CS plumbing — range-proof + additive-inverse
    /// allocation — and pushes the `WideToken` / `Token` pair.
    fn op_borrow_encrypted_inner<D: Delegate>(
        &mut self,
        qty: crate::Variable,
        flv: crate::Variable,
        delegate: &mut D,
    ) -> Result<(), VMError> {
        use bulletproofs::r1cs::ConstraintSystem;
        use spacesuit::BitRange;
        // Commit both to the CS. Prover uses the open witness; verifier
        // sees only the closed point. Either way, the returned r1cs vars
        // are bound to the same commitment point on both sides.
        let (_flv_point, flv_var) = delegate.commit_variable(&flv.commitment)?;
        let (_qty_point, qty_var) = delegate.commit_variable(&qty.commitment)?;
        // Witness assignments (prover only). Negative qty is a
        // protocol error here — borrow's +T is range-proven non-negative.
        let qty_assignment = match qty.commitment.assignment() {
            Some(i) => Some(int253_to_signed_integer(i)?),
            None => None,
        };
        let flv_assignment = flv.commitment.assignment().map(|i| i.to_scalar_mod_order());
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
        let wide = crate::WideToken(spacesuit::AllocatedValue {
            q: neg_qty_var,
            f: flv_var,
            assignment: match (neg_qty_assignment, flv_assignment) {
                (Some(q), Some(f)) => Some(spacesuit::Value { q, f }),
                _ => None,
            },
        });
        let token = crate::Token::new(qty.commitment, flv.commitment);
        self.push_value(Value::WideToken(wide));
        self.push_value(Value::Token(token));
        Ok(())
    }

    /// _qty flv_ **fee** → _widetoken_
    ///
    /// External-only. Records `TxEntry::Fee(qty)`, allocates a WideToken
    /// debt with `q = -qty`, `f = flv`, pushes it.
    fn op_fee<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::r1cs::ConstraintSystem;
        let flv = self.pop_value()?.to_int253()?;
        let qty = self.pop_value()?.to_int253()?;
        if qty.is_negative() {
            return Err(VMError::FeeQtyNegative);
        }
        let qty_u64 = qty.to_u64().ok_or(VMError::FeeTooHigh)?;
        self.total_fee.add(qty_u64)?;
        let qty_scalar: curve25519_dalek::scalar::Scalar = qty.into();
        let flv_scalar: curve25519_dalek::scalar::Scalar = flv.into();
        let q_var = delegate.cs().allocate(Some(-qty_scalar)).map_err(VMError::R1CSError)?;
        delegate.cs().constrain(q_var + qty_scalar);
        let f_var = delegate.cs().allocate(Some(flv_scalar)).map_err(VMError::R1CSError)?;
        delegate.cs().constrain(f_var - flv_scalar);
        let assignment = Some(spacesuit::Value {
            q: -spacesuit::SignedInteger::from(qty_u64),
            f: flv_scalar,
        });
        let wide = crate::WideToken(spacesuit::AllocatedValue {
            q: q_var,
            f: f_var,
            assignment,
        });
        self.push_value(Value::WideToken(wide));
        self.txlog.push(crate::tx::TxEntry::Fee(qty_u64));
        Ok(())
    }

    /// Converts a stack value into a `spacesuit::AllocatedValue` for
    /// the cloak gadget. Token/ClearToken commit to the CS; WideToken
    /// unwraps in place.
    fn value_to_allocated<D: Delegate>(
        &mut self,
        value: Value,
        delegate: &mut D,
    ) -> Result<spacesuit::AllocatedValue, VMError> {
        match value {
            Value::WideToken(w) => Ok(*w.allocated()),
            Value::Token(t) => {
                let (_, qty_var) = delegate.commit_variable(&t.qty)?;
                let (_, flv_var) = delegate.commit_variable(&t.flv)?;
                let qty_assg = match t.qty.assignment() {
                    Some(i) => Some(int253_to_signed_integer(i)?),
                    None => None,
                };
                let flv_assg = t.flv.assignment().map(|i| i.to_scalar_mod_order());
                Ok(spacesuit::AllocatedValue {
                    q: qty_var,
                    f: flv_var,
                    assignment: match (qty_assg, flv_assg) {
                        (Some(q), Some(f)) => Some(spacesuit::Value { q, f }),
                        _ => None,
                    },
                })
            }
            Value::ClearToken(c) => {
                let token = crate::Token::cleartext(c.qty(), c.flv());
                self.value_to_allocated(Value::Token(token), delegate)
            }
            _ => Err(VMError::TypeNotToken),
        }
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
        // Stack depth check: we'll pop 2n commitment Strings + m token values.
        let needed = m.saturating_add(n.saturating_mul(2));
        if needed > self.current_call.stack.len() {
            return Err(VMError::StackUnderflow);
        }
        // Build outputs (closest to top): each output pops (flv, qty)
        // Strings → builds Token (with Closed commitments since the
        // String→Commitment downcast retains witness only for
        // String::Commitment variants).
        let mut output_tokens: Vec<crate::Token> = Vec::with_capacity(n);
        let mut cloak_outs: Vec<spacesuit::AllocatedValue> = Vec::with_capacity(n);
        for _ in 0..n {
            let flv_str = self.pop_value()?.to_string()?;
            let qty_str = self.pop_value()?.to_string()?;
            let flv_commit = flv_str.to_commitment()?;
            let qty_commit = qty_str.to_commitment()?;
            let token = crate::Token::new(qty_commit, flv_commit);
            // Build the AllocatedValue against the CS.
            let allocated = self.value_to_allocated(
                Value::Token(token.clone()),
                delegate,
            )?;
            // Insert at front so the deepest output ends up at cloak_outs[0].
            output_tokens.insert(0, token);
            cloak_outs.insert(0, allocated);
        }
        // Build inputs.
        let mut cloak_ins: Vec<spacesuit::AllocatedValue> = Vec::with_capacity(m);
        for _ in 0..m {
            let item = self.pop_value()?;
            let allocated = self.value_to_allocated(item, delegate)?;
            cloak_ins.insert(0, allocated);
        }
        // Run the cloak gadget. On constraint-system error, surface
        // as R1CSError; the verifier will reject the proof.
        spacesuit::cloak(delegate.cs(), cloak_ins, cloak_outs)
            .map_err(VMError::R1CSError)?;
        // Push the output Tokens in the same order (deepest first).
        for token in output_tokens {
            self.push_value(Value::Token(token));
        }
        Ok(())
    }

    /// `0x77 decrypt` — `token f f' q q' → cleartoken`. Reveals a
    /// cleartext quantity / flavor pair for an encrypted Token by
    /// supplying their cleartext values (`f`, `q`) and Pedersen
    /// blinding factors (`f'`, `q'`).
    ///
    /// Each Pedersen-opening — `token.qty == q*B + q'*B_blinding` and
    /// `token.flv == f*B + f'*B_blinding` — is rewritten as the MSM
    /// assertion `q*B + q'*B_blinding − token.qty == 0` (and likewise
    /// for `flv`) and appended to the delegate's `BatchVerifier` as
    /// two independent statements. The actual multi-scalar
    /// multiplication runs once per tx at finalize, alongside the
    /// Schnorr / MuSig / MSM batch — same lane and rollback story as
    /// `op_verify` for `MultiscalarMul`. So a wrong `(q, q', f, f')`
    /// surfaces as `BatchSignatureVerificationFailed` at finalize,
    /// not synchronously here.
    ///
    /// All four scalar operands (`f`, `f'`, `q`, `q'`) are popped as
    /// `Int253`. The Token is popped last (deepest on stack). The
    /// `ClearToken(q, f)` push happens unconditionally — the
    /// soundness of the `q, f` declaration is the deferred batch
    /// check above.
    fn op_decrypt<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::PedersenGens;
        let q_blind = self.pop_value()?.to_int253()?;
        let q_value = self.pop_value()?.to_int253()?;
        let f_blind = self.pop_value()?.to_int253()?;
        let f_value = self.pop_value()?.to_int253()?;
        let token = match self.pop_value()? {
            Value::Token(t) => t,
            _ => return Err(VMError::TypeNotToken),
        };
        let gens = PedersenGens::default();
        // Append two independent statements:
        //   q*B + q'*B_blinding + (-1)*token.qty.to_point() == identity
        //   f*B + f'*B_blinding + (-1)*token.flv.to_point() == identity
        // Each gets its own random factor from the batch verifier, so
        // they can't cancel each other or other batched statements.
        // `B` is the Ristretto basepoint (PedersenGens default), so the
        // value scalar rides on the BatchVerifier's basepoint lane.
        let neg_one = -Scalar::ONE;
        let qty_point = token.qty.to_point().decompress();
        let flv_point = token.flv.to_point().decompress();
        musig::BatchVerification::append(
            delegate.batch_verifier(),
            q_value.to_scalar_mod_order(),
            [q_blind.to_scalar_mod_order(), neg_one],
            [Some(gens.B_blinding), qty_point],
        );
        musig::BatchVerification::append(
            delegate.batch_verifier(),
            f_value.to_scalar_mod_order(),
            [f_blind.to_scalar_mod_order(), neg_one],
            [Some(gens.B_blinding), flv_point],
        );
        self.push_value(Value::ClearToken(ClearToken::new(q_value, f_value)));
        Ok(())
    }
}

/// Returns `true` iff `value` is non-negative and fits in `[0, 2^n)`.
/// Used by `op_range` to short-circuit cleartext Expression::Constant
/// arguments without touching the CS.
fn int_fits_in_n_bits(value: Int253, n: usize) -> bool {
    if value.is_negative() {
        return false;
    }
    if n >= 64 {
        // Any non-negative Int253 fits — but for n in [1, 64] this
        // collapses to "fits in u64", which we check via to_u64().
        return value.to_u64().is_some();
    }
    match value.to_u64() {
        Some(v) => v < (1u64 << n),
        None => false,
    }
}

/// Converts an `Int253` to `spacesuit::SignedInteger` if it fits the
/// `±2^64` range. Out-of-range values error `InvalidBitrange`.
/// Converts a cleartext `Int253` witness to a `SignedInteger` for a
/// range-proof assignment.
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
fn int253_to_signed_integer(value: Int253) -> Result<spacesuit::SignedInteger, VMError> {
    if value.is_negative() {
        let mag = value.abs();
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
