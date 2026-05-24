//! FlameVM execution engine.
//!
//! One [`VM`] type drives both external and internal transactions via
//! two distinct entry points:
//!
//! - [`VM::execute_external`] is generic over a [`Delegate`] (prover or
//!   verifier) which owns the R1CS constraint system and finalizes the
//!   transaction by producing or verifying a Bulletproofs proof.
//! - [`VM::execute_internal`] takes no delegate; internal transactions
//!   do not produce or consume R1CS proofs and operate against a live
//!   actor registry instead.
//!
//! ## Three layers of nesting
//!
//! - **Tx** — one transaction (this `VM` instance). Owns the txlog,
//!   gas/vbyte totals, deferred signature records, and the active call.
//! - **Call** ([`CallFrame`]) — one isolated execution scope created by
//!   `call`, `open`, or the outermost frame of a tx. Has its own stack,
//!   gas budget, and transient-memory cap.
//! - **Run** ([`Run`]) — one bytecode script being interpreted. Created
//!   by `run`, `loop`, `switch`, and at the entry of every Call.

use bulletproofs::r1cs;
use bulletproofs::r1cs::R1CSProof;
use core::convert::TryFrom;
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use core::mem;
use merlin::Transcript;

use crate::errors::VMError;
use crate::tx::TxHeader;
use crate::cell::{CallProof, Cell, Predicate};
use crate::constraints::Commitment;
use crate::token::flavor_from_actor;
use crate::{ClearToken, Dict, Int253, Merlin, Point, String, Value};

// ── Identifiers and metadata ──────────────────────────────────────

/// 32-byte actor identifier (hash of initial state or constructor script).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorID(pub [u8; 32]);

/// Method index within an actor's `public` dict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodKey(pub u64);

/// 32-byte anchor unique to a tx-initiated send or cell identity.
///
/// Anchors form chains: each output's anchor is derived by `ratchet`-ing
/// from the previous one, guaranteeing uniqueness across all outputs in
/// a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor(pub [u8; 32]);

impl Anchor {
    /// Derives the next anchor from this one using a domain-separated
    /// Merlin transcript. Used by `cell` / `output` to chain anchors
    /// uniquely across a transaction without colliding with anchors from
    /// inputs or other sources.
    pub fn ratchet(&self) -> Anchor {
        let mut t = Transcript::new(b"flamevm.anchor.ratchet.v1");
        t.append_message(b"prev", &self.0);
        let mut next = [0u8; 32];
        t.challenge_bytes(b"next", &mut next);
        Anchor(next)
    }
}

// `Predicate` lives in `cell::predicate`; re-exported via `crate::Predicate`.

// ── Deferred signature records ────────────────────────────────────

/// Signature check whose verification is deferred to `Delegate::finalize`.
///
/// Two flavors:
///
/// - **`TxBound`** — created by `signtx`. The cell-holder authorizes the
///   *whole transaction*; the actual signature lives in the tx envelope
///   and the message comes from the eventually-computed TxID. At
///   finalize, the delegate aggregates all `TxBound` keys (via MuSig)
///   and verifies the envelope signature against the TxID-bound message.
///
/// - **`Explicit`** — created by `signrun`. The cell-holder signed a
///   specific program at run time; both the message (a transcript over
///   the program bytes) and the signature are known immediately. The
///   delegate batch-verifies them at finalize.
#[derive(Clone, Debug)]
pub enum DeferredSig {
    /// Cell-holder must sign the transaction's TxID. The signature is
    /// supplied via the tx envelope (not on the stack).
    ///
    /// Carries the consumed cell's id so the multi-message context
    /// can give each signer a distinct message — the same
    /// pattern zkvm uses for `signtx_items: Vec<(VerificationKey,
    /// ContractID)>`. The aggregate signature is verified against
    /// `Vec<(verification_key, cell_id)>` with the transcript bound
    /// to the TxID.
    TxBound {
        verification_key: CompressedRistretto,
        cell_id: crate::cell::CellID,
    },

    /// Cell-holder signed an explicit message at run time. The signature
    /// is on the stack at the time the record is created.
    Explicit {
        verification_key: CompressedRistretto,
        message: Vec<u8>,
        signature: [u8; 64],
    },
}

// ── Inbound message and context ──────────────────────────────────

/// Inbound message that triggers an internal transaction.
pub struct Message {
    pub target: ActorID,
    pub method: MethodKey,
    pub caller: Option<ActorID>,
    pub anchor: Anchor,
    pub payload: Vec<Value>,
    pub gas: u64,
    pub vbytes: u64,
}

/// Block-level immutable context (height, chain stats).
pub struct BlockContext {
    pub height: u64,
}

/// Mutable handle into the live actor registry. The internal-tx VM consults
/// this to resolve method bytecode and to read/write actor state.
pub trait ActorRegistry {
    /// Returns the bytecode of the given method on the given actor.
    fn resolve_method(
        &self,
        actor: &ActorID,
        method: MethodKey,
    ) -> Result<Vec<u8>, VMError>;

    /// Returns the actor's persistent vbyte balance. Used to size the
    /// transient-memory cap (`4× persistent` per design.md).
    fn actor_vbytes(&self, actor: &ActorID) -> Result<u64, VMError>;
}

// ── Delegate ─────────────────────────────────────────────────────

/// External-context user-task abstraction.
///
/// Two implementations: a prover that builds an R1CS proof + signs, and a
/// verifier that verifies a proof + batch-checks signatures. Both thread
/// the same VM over the same opcodes; only the proof-machinery differs.
pub trait Delegate {
    type CS: r1cs::RandomizableConstraintSystem;
    /// Per-side batched scalar-point check accumulator. Mirrors zkvm's
    /// `Delegate::BatchVerifier`. Built up by `signtx`/`signrun` (and,
    /// later, `unblind` / `issue`'s flavor check); drained by
    /// `Verifier::verify_proof` via `batch.verify()`.
    type BatchVerifier: musig::BatchVerification;

    /// Mutable access to the constraint system.
    fn cs(&mut self) -> &mut Self::CS;

    /// Mutable access to the batch verifier. Used by deferred-sig
    /// finalization on the verifier side; the prover's
    /// `BatchVerifier` accumulates the same items (used by the
    /// `Explicit` deferred-sig path's batch check on the prover, too,
    /// since proving doesn't change the algebraic check shape).
    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier;

    /// Allocates an R1CS variable backed by a Pedersen commitment.
    ///
    /// Prover-side: the `Commitment::Open(witness)` case carries the
    /// cleartext value and blinding factor; the prover calls
    /// `cs.commit(value, blinding)` to bind both into the proof.
    /// Verifier-side: `Commitment::Closed(point)` is the only thing
    /// the verifier sees; it calls `cs.commit(point)` to bind the
    /// point into the proof. Both return the same `(point, variable)`
    /// pair so downstream opcode logic is agnostic to which side it's
    /// running on.
    fn commit_variable(
        &mut self,
        commitment: &crate::Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError>;

    /// Consumes the delegate after VM execution finishes cleanly.
    ///
    /// Prover: builds the Bulletproofs proof, processes deferred sigs as
    /// signing material. Verifier: verifies the supplied proof, processes
    /// deferred sigs as a batched check.
    fn finalize(self, deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError>;
}

/// No-op [`Delegate`] used by [`VM::step_internal`].
///
/// Internal-context transactions never run CS-touching opcodes —
/// every external-only handler (`op_alloc`, `op_expr`, `op_range`,
/// `op_scalar`, `op_commit`, `op_decrypt`, `op_mix`, `op_fee`,
/// `op_input`) calls [`VM::require_external`] at its top and
/// returns `ExternalOnly` before any `delegate.cs()` access. The
/// `cs` / `batch_verifier` / `commit_variable` methods therefore
/// `unreachable!()` in this impl — they exist only to satisfy the
/// trait so a single `step<D: Delegate>` can serve both contexts.
struct InternalDelegate;

impl Delegate for InternalDelegate {
    type CS = r1cs::Verifier<merlin::Transcript>;
    type BatchVerifier = musig::BatchVerifier<rand::rngs::ThreadRng>;

    fn cs(&mut self) -> &mut Self::CS {
        unreachable!(
            "InternalDelegate::cs is unreachable — CS opcodes call \
             `require_external()` first and error before reaching here",
        )
    }

    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier {
        unreachable!(
            "InternalDelegate::batch_verifier is unreachable — \
             signature-batching opcodes are external-only",
        )
    }

    fn commit_variable(
        &mut self,
        _commitment: &crate::Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError> {
        unreachable!(
            "InternalDelegate::commit_variable is unreachable — \
             `commit`/`expr`/`mix` are external-only",
        )
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        Ok(())
    }
}

// ── Run ──────────────────────────────────────────────────────────

/// Multiple Runs may nest within one call (via `run` / `loop` /
/// `switch`); each pushes onto `CallFrame.run_stack` and is resumed
/// on `break` / `return` / end-of-program.
///
/// A single executable script slice the VM is currently walking —
/// always a pre-decoded `Vec<Instruction>` plus a cursor.
///
/// Prover and verifier feed the VM through the same shape: the
/// prover hands in instructions with their witness slots populated
/// (`Instruction::Input(Some(_))`, `Alloc(Some(_))`, …), the
/// verifier hands in instructions parsed from bytecode (witness
/// slots all `None`). Nested programs (entered via `op_run`,
/// `op_open`, etc.) decode the bytes-on-stack via `Program::parse`
/// the same way on both sides — `String` payloads can't carry
/// witnesses so the inner Run is always witness-free regardless of
/// which side is running.
pub struct Run {
    instructions: Vec<crate::ops::Instruction>,
    cursor: usize,
}

impl Run {
    /// Constructs a Run from a pre-decoded instruction stream. The
    /// verifier calls `Program::parse(&bytecode)` to produce this;
    /// the prover passes its witness-bearing `Program` directly.
    pub fn new(instructions: Vec<crate::ops::Instruction>) -> Self {
        Run { instructions, cursor: 0 }
    }

    /// Returns the next [`Instruction`] in this Run, advancing the
    /// cursor. `Ok(None)` at end of program. Result-shaped to keep
    /// the call-site uniform with the previous bytecode-on-the-fly
    /// parsing path — Phase-19 bytecode errors that used to surface
    /// here now surface at `Program::parse` entry instead.
    pub(crate) fn next_instruction(
        &mut self,
    ) -> Result<Option<crate::ops::Instruction>, VMError> {
        if self.cursor >= self.instructions.len() {
            return Ok(None);
        }
        let instr = self.instructions[self.cursor].clone();
        self.cursor += 1;
        Ok(Some(instr))
    }

    /// True iff the Run has reached its end.
    pub(crate) fn is_finished(&self) -> bool {
        self.cursor >= self.instructions.len()
    }

    /// Resets the cursor to the start of the Run. Used by `loop`.
    fn rewind(&mut self) {
        self.cursor = 0;
    }

    /// Jumps the cursor past the end of the Run, so the next call to
    /// `next_instruction` returns `None`. Used by `break:k`.
    fn jump_to_end(&mut self) {
        self.cursor = self.instructions.len();
    }
}

// ── CallFrame ────────────────────────────────────────────────────

/// What kind of scope a [`CallFrame`] represents. Drives identity, the
/// re-entrancy check, and dispatch of identity-aware opcodes (`actorid`,
/// `callerid`, `method`, `anchor`).
pub enum CallKind {
    /// Outer scope of an external transaction.
    ExternalRoot,

    /// Outer scope of an internal transaction, entered by dispatching to
    /// the target actor's method.
    InternalRoot {
        actor: ActorID,
        method: MethodKey,
        caller: Option<ActorID>,
        anchor: Anchor,
    },

    /// Synchronous actor-to-actor `call` inside an internal tx.
    ActorCall {
        actor: ActorID,
        method: MethodKey,
        caller: ActorID,
    },

    /// `open` of a cell predicate (either context).
    CellOpen {
        anchor: Anchor,
        predicate: Predicate,
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
}

/// An isolated execution scope. Holds its own stack, run, gas budget, and
/// transient-memory cap. Created by `call`, `open`, or the outermost frame
/// of a tx.
pub struct CallFrame {
    /// Isolated stack visible to scripts in this scope.
    pub(crate) stack: Vec<Value>,

    /// Currently executing script.
    pub(crate) current_run: Run,

    /// Suspended scripts at this same call level (from `run`/`loop`/`switch`).
    pub(crate) run_stack: Vec<Run>,

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
}

impl CallFrame {
    /// Builds a fresh CallFrame whose Run walks `instructions`. The
    /// caller has already decoded the script bytes (verifier via
    /// `Program::parse`) or is supplying a witness-bearing Program
    /// (prover) — either way the Run shape is the same.
    pub fn new(
        instructions: Vec<crate::ops::Instruction>,
        kind: CallKind,
        gas_limit: u64,
        mem_limit: u64,
        newbytes: u64,
    ) -> Self {
        Self {
            stack: Vec::new(),
            current_run: Run::new(instructions),
            run_stack: Vec::new(),
            kind,
            gas_limit,
            gas_used: 0,
            mem_limit,
            mem_used: 0,
            newbytes,
        }
    }
}

// ── Result ───────────────────────────────────────────────────────

/// Outcome of a successful transaction execution. Returned by both
/// `Prover::prove` (with `proof: Some(...)`) and `Verifier::verify`
/// (with `proof: None` — proof has already been verified by then).
///
/// Carries the full "observed effects" of the tx: the canonical
/// TxID, the txlog, the running fee total, the resource usage, the
/// deferred signature records, and the optional R1CS proof.
/// Downstream consumers (mempool, validator, wallet) read from a
/// single value rather than reassembling fields from a multi-tuple
/// return.
///
/// Linear-value variants in `TxEntry::Output(Cell)` prevent
/// `#[derive(Clone, Debug, Serialize, Deserialize)]` here — see the
/// matching note on `TxEntry`. A custom `Debug` impl on the carrier
/// vectors would address most needs; for now the struct itself is
/// `Debug`-skipped and callers project out the fields they want to
/// log.
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

    /// Outbound message sends recorded by `op_send`. Empty placeholder
    /// until the opcode and the matching `TxEntry::Send` variant are
    /// wired in.
    pub sends: Vec<()>,
}

/// Manual `Debug` — same reason as `TxEntry`'s manual impl: the
/// linear `Cell` inside `txlog` blocks `#[derive(Debug)]`.
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
            .field("sends.len", &self.sends.len())
            .finish()
    }
}

// ── VM ───────────────────────────────────────────────────────────

pub struct VM {
    #[allow(dead_code)]
    header: TxHeader,
    pub(crate) last_anchor: Option<Anchor>,

    gas_used: u64,
    vbytes_used: u64,

    current_call: CallFrame,
    call_stack: Vec<CallFrame>,

    /// Effects emitted during execution. Outputs, data entries, future
    /// issuances/retirements/fees/sends. Used at finalize to compute TxID.
    pub(crate) txlog: Vec<crate::tx::TxEntry>,

    /// Running per-tx fee accumulator. Each `op_fee` increments it;
    /// overflow → `FeeTooHigh`. Surfaced through
    /// `TxResult.total_fee`.
    total_fee: crate::fees::CheckedFee,

    /// Signature checks deferred to `Delegate::finalize`. Always empty in
    /// internal mode.
    deferred_sigs: Vec<DeferredSig>,
}

impl VM {
    /// Executes an external transaction script with the given delegate.
    /// Consumes the delegate (calls `finalize` at the end). Returns
    /// the full Phase-21 [`TxResult`] with `proof = None` — this
    /// entry point is for callers that don't care about ZK shape
    /// (integration tests, stub delegates, …); the
    /// [`crate::Prover::prove`] / [`crate::Verifier::verify`] entry
    /// points are the real ZK boundaries.
    pub fn execute_external<D: Delegate>(
        header: TxHeader,
        script: Vec<u8>,
        gas_limit: u64,
        mem_limit: u64,
        mut delegate: D,
    ) -> Result<TxResult, VMError> {
        let bytecode = script.clone();
        let program = crate::program::Program::parse(&script)?;
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
        while vm.step_external(&mut delegate)? {}
        // Finalize the delegate first (signatures, proof verification
        // on real delegates; no-op on the stub). On success, drain
        // the VM state into a TxResult.
        let sigs = mem::take(&mut vm.deferred_sigs);
        delegate.finalize(sigs.clone())?;
        // Re-attach the drained sigs into TxResult so callers can
        // inspect them post-finalize. The clone above lets `finalize`
        // own its copy without losing the audit trail here.
        vm.deferred_sigs = sigs;
        Ok(vm.into_result(bytecode, None))
    }

    /// Runs an external-root program through the VM to completion
    /// without calling `Delegate::finalize`. Single entry point for
    /// both prover and verifier — the prover passes a `Program`
    /// with witnesses inline, the verifier passes one decoded from
    /// raw bytecode via `Program::parse`. Either way the VM walks
    /// the resulting `Vec<Instruction>` through one dispatch.
    ///
    /// Returns the full Phase-21 [`TxResult`] (with `proof = None`
    /// — the caller, typically [`crate::Verifier::verify`] or
    /// [`crate::Prover::prove`], attaches the proof afterward).
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

    /// Executes an internal transaction triggered by `message`. Resolves
    /// the target method's bytecode and the target actor's vbyte balance
    /// from `registry`; provides chain info from `block`.
    pub fn execute_internal(
        header: TxHeader,
        message: Message,
        registry: &mut dyn ActorRegistry,
        _block: &BlockContext,
    ) -> Result<TxResult, VMError> {
        let script = registry.resolve_method(&message.target, message.method)?;
        let mem_limit = registry.actor_vbytes(&message.target)?.saturating_mul(4);
        let kind = CallKind::InternalRoot {
            actor: message.target,
            method: message.method,
            caller: message.caller,
            anchor: message.anchor,
        };
        let program = crate::program::Program::parse(&script)?;
        let mut vm = Self::new(
            header,
            CallFrame::new(
                program.into_instructions(),
                kind,
                message.gas,
                mem_limit,
                message.vbytes,
            ),
        );
        while vm.step_internal()? {}
        // Internal context produces no proof and no deferred sigs.
        Ok(vm.into_result(Vec::new(), None))
    }

    fn new(header: TxHeader, initial_call: CallFrame) -> Self {
        // Header is the first txlog entry so TxID::from_log binds to
        // version + locktime alongside the effects. Mirrors zkvm.
        let mut txlog = Vec::new();
        txlog.push(crate::tx::TxEntry::Header(header));
        Self {
            header,
            last_anchor: None,
            gas_used: 0,
            vbytes_used: 0,
            current_call: initial_call,
            call_stack: Vec::new(),
            txlog,
            total_fee: crate::fees::CheckedFee::zero(),
            deferred_sigs: Vec::new(),
        }
    }

    /// Drain the VM into a Phase-21 [`TxResult`]. Computes
    /// `TxID::from_log(&txlog)` from the accumulated log so the
    /// returned struct is self-contained — every consumer reads it
    /// off the result without re-running the merkle root.
    ///
    /// `bytecode` and `proof` are filled by the caller (the VM
    /// doesn't always have the bytecode — `VM::run`
    /// walked a `Run::Queue` over `Instructions` instead of raw
    /// bytes — and the proof is constructed by the Prover after the
    /// run finishes).
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
            gas_used: self.gas_used,
            vbytes_used: self.vbytes_used,
            bytecode,
            proof,
            deferred_sigs,
            sends: Vec::new(),
        }
    }

    // ── Dispatch ─────────────────────────────────────────────────

    /// Returns `Ok(())` iff the current call frame is in external
    /// context (`ExternalRoot` or `CellOpen` — see `CallKind`).
    /// External-only opcode handlers call this at the top to gate
    /// CS-touching work; internal context errors `ExternalOnly`
    /// before the handler reaches `delegate.cs()`. Centralising the
    /// check here means dispatch stays flat and per-opcode rules
    /// live in handlers (zkvm pattern).
    fn require_external(&self) -> Result<(), VMError> {
        if self.is_external() {
            Ok(())
        } else {
            Err(VMError::ExternalOnly)
        }
    }

    /// True iff the current frame is `ExternalRoot` or a `CellOpen`
    /// nested inside one. Used by polymorphic handlers (`eq`,
    /// `neg`, …) to know whether the Expression / Constraint
    /// branches are even reachable — those require a constraint
    /// system, which only external context provides.
    fn is_external(&self) -> bool {
        matches!(
            self.current_call.kind,
            CallKind::ExternalRoot | CallKind::CellOpen { .. },
        )
    }

    /// External-context step: thin wrapper around the unified
    /// [`Self::step`]. Retained as a stable name for callers that
    /// already drive the VM step-by-step with an explicit delegate
    /// (tests, prover/verifier internals).
    pub(crate) fn step_external<D: Delegate>(
        &mut self,
        delegate: &mut D,
    ) -> Result<bool, VMError> {
        self.step(delegate)
    }

    /// Internal-context step: drives the unified [`Self::step`]
    /// with a private no-op `InternalDelegate`. The CS opcodes in
    /// `step` all call `require_external()` first and return
    /// `ExternalOnly` before any `delegate.cs()` use — so the
    /// no-op delegate's panicking `cs()` is unreachable at runtime.
    pub(crate) fn step_internal(&mut self) -> Result<bool, VMError> {
        let mut stub = InternalDelegate;
        self.step(&mut stub)
    }

    /// Executes one [`Instruction`]. Returns `Ok(true)` to keep
    /// running, `Ok(false)` to stop. The flat match is the only
    /// dispatch — no external/internal/common split, no `if` peeks
    /// on the stack. Per-opcode context and operand rules live
    /// inside each handler.
    fn step<D: Delegate>(&mut self, delegate: &mut D) -> Result<bool, VMError> {
        let Some(instr) = self.current_call.current_run.next_instruction()? else {
            return self.finish_run();
        };
        use crate::ops::Instruction as I;
        match instr {
            // ── Stack literals & manipulation ─────────────────────
            I::PushInt(i) => {
                self.push_value(Value::Int253(i));
                Ok(())
            }
            I::PushStr(s) => {
                self.push_value(Value::String(s));
                Ok(())
            }
            I::PushPoint(bytes) => {
                self.push_value(Value::Point(Point::from_bytes(bytes)));
                Ok(())
            }
            I::PushToken => self.op_pushtoken(),
            I::Drop => self.op_drop(),
            I::Nop => self.op_nop(),
            I::Dup => self.op_dup(),
            I::Roll => self.op_roll(),
            I::DupK(k) => self.op_dup_k(k as usize),
            I::RollK(k) => self.op_roll_k(k as usize),
            // ── String ops ────────────────────────────────────────
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
            I::Keccak256 => self.op_keccak256(),
            // ── Int253 / polymorphic arithmetic ───────────────────
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
            // ── Dict ops ──────────────────────────────────────────
            I::Dict => self.op_dict(),
            I::Put => self.op_put(),
            I::Replace => self.op_replace(),
            I::Get => self.op_get(),
            I::GetOpt => self.op_getopt(),
            I::GetDup => self.op_getdup(),
            I::First => self.op_first(),
            I::Last => self.op_last(),
            I::Next => self.op_next(),
            // ── Hash & Merlin ─────────────────────────────────────
            I::Merlin => self.op_merlin(),
            I::MerlinWrite => self.op_merlin_write(),
            I::MerlinRead => self.op_merlin_read(),
            I::Sha256 => self.op_sha256(),
            I::Sha512 => self.op_sha512(),
            I::Sha3 => self.op_sha3(),
            I::Log => self.op_log(),
            // ── Tokens ────────────────────────────────────────────
            I::Amount => self.op_amount(),
            I::Issue => self.op_issue(),
            I::Retire => self.op_retire(),
            I::Borrow => self.op_borrow(delegate),
            I::Merge => self.op_merge(),
            I::Split => self.op_split(),
            I::IssueFlv => self.op_issueflv(),
            // ── CS-bound (external-only via `require_external`) ──
            I::Alloc(w) => self.op_alloc(w, delegate),
            I::Expr => self.op_expr(delegate),
            I::Range => self.op_range(delegate),
            I::Scalar => self.op_scalar(),
            I::Commit => self.op_commit(),
            I::Decrypt => self.op_decrypt(),
            I::Mix => self.op_mix(delegate),
            I::Fee => self.op_fee(delegate),
            I::Verify => self.op_verify(delegate),
            // ── Control flow ──────────────────────────────────────
            I::Run => self.op_run(),
            I::Loop => self.op_loop(),
            I::Switch => self.op_switch(),
            I::Return => self.op_return(),
            I::Type => self.op_type(),
            I::BreakK(k) => self.op_break_k(k as usize),
            // ── Cells / cell-open / signtx / signrun / input ──────
            I::Input(w) => self.op_input(w.as_deref()),
            I::Cell => self.op_cell(),
            I::Output => self.op_output(),
            I::Open => self.op_open(),
            I::Signtx => self.op_signtx(),
            I::Signrun => self.op_signrun(),
            // ── Extension / unknown ───────────────────────────────
            I::Ext(b) => Err(VMError::UnknownOpcode(b)),
        }?;
        Ok(true)
    }

    /// End-of-script: pop the run stack, or finish the current call.
    fn finish_run(&mut self) -> Result<bool, VMError> {
        if let Some(run) = self.current_call.run_stack.pop() {
            self.current_call.current_run = run;
            return Ok(true);
        }
        self.finish_call()
    }

    /// Pops the current call frame back to its caller after a clean exit.
    ///
    /// Strict semantics: the callee's stack must already be empty. Values
    /// destined for the caller cross the boundary *only* via the explicit
    /// `return` opcode, which pops them from the callee, pops this frame,
    /// and pushes them onto the caller's stack as a single atomic step.
    /// This routine never moves stack items between frames; reaching it
    /// with a non-empty stack is a script bug, not a salvage opportunity.
    ///
    /// Gas refund, by contrast, is a structural property of `call` (per
    /// design.md §Gas: "Unused gas in a call remains with the caller").
    /// It happens here unconditionally on clean exit.
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
            self.current_call.gas_limit = self
                .current_call
                .gas_limit
                .saturating_add(leftover_gas);
            return Ok(true);
        }
        // Outermost call returned: entire tx complete.
        Ok(false)
    }

    // ── Stack helpers ────────────────────────────────────────────

    /// Pushes a value onto the current call's stack.
    fn push_value(&mut self, v: Value) {
        self.current_call.stack.push(v);
    }

    /// Pops the top value from the current call's stack.
    fn pop_value(&mut self) -> Result<Value, VMError> {
        self.current_call.stack.pop().ok_or(VMError::StackUnderflow)
    }

    /// Pops the top value, asserting it is an `Int253`.
    fn pop_int253(&mut self) -> Result<Int253, VMError> {
        match self.pop_value()? {
            Value::Int253(i) => Ok(i),
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// Converts a stack-popped `Int253` index into a `usize`, rejecting
    /// negatives, values that don't fit `u64`, and values exceeding the
    /// addressed stack depth.
    fn int253_to_stack_index(&self, i: Int253) -> Result<usize, VMError> {
        let v = i.to_u64().ok_or(VMError::IndexOutOfRange)?;
        let idx = usize::try_from(v).map_err(|_| VMError::IndexOutOfRange)?;
        if idx >= self.current_call.stack.len() {
            return Err(VMError::IndexOutOfRange);
        }
        Ok(idx)
    }

    // ── opcode handlers ─────────────────────────────────

    // `pushint`/`pushstr`/`pushpoint` are now inline in
    // `dispatch_common`: `Instruction::parse` decodes their inline
    // bytes into typed payloads (Int253 / String / [u8; 32]) so the
    // handlers reduce to a single `push_value` call.

    /// `0x1b` `pushtoken` — `flv → token`. Pops an `Int253` flavor from
    /// the stack and pushes a zero-qty `ClearToken { qty: 0, flv }`. This
    /// is the canonical "empty bearer of a flavor" used as a starting
    /// point for issuance / borrow flows.
    fn op_pushtoken(&mut self) -> Result<(), VMError> {
        let flv = self.pop_int253()?;
        self.push_value(Value::ClearToken(ClearToken::new(Int253::zero(), flv)));
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
        let k = self.pop_int253()?;
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
        let k = self.pop_int253()?;
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

    // ── Hash & Merlin ───────────────────────────────────

    /// Pops the top value, asserting it is a `Merlin` transcript.
    fn pop_merlin(&mut self) -> Result<Merlin, VMError> {
        match self.pop_value()? {
            Value::Merlin(m) => Ok(m),
            _ => Err(VMError::TypeNotMerlin),
        }
    }

    /// `0x69` `merlin` — `label → merlin`. Pops a label string, creates
    /// a fresh transcript bound to it.
    fn op_merlin(&mut self) -> Result<(), VMError> {
        let label = self.pop_string()?;
        self.push_value(Value::Merlin(Merlin::new(label.as_bytes())));
        Ok(())
    }

    /// `0x6a` `merlinwrite` — `merlin label str → merlin`. Pops `str`
    /// (top), `label`, and `merlin`; absorbs `(label, str)` into the
    /// transcript; pushes merlin back.
    fn op_merlin_write(&mut self) -> Result<(), VMError> {
        let data = self.pop_string()?;
        let label = self.pop_string()?;
        let mut m = self.pop_merlin()?;
        m.write_bytes(label.as_bytes(), data.as_bytes());
        self.push_value(Value::Merlin(m));
        Ok(())
    }

    /// `0x6b` `merlinread` — `merlin label n → merlin str`. Squeezes
    /// `n` bytes of challenge from the transcript under `label`; pushes
    /// the merlin back, then the new String.
    fn op_merlin_read(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let label = self.pop_string()?;
        let mut m = self.pop_merlin()?;
        let out = m.read_bytes(label.as_bytes(), n);
        self.push_value(Value::Merlin(m));
        self.push_value(Value::String(String::from(out)));
        Ok(())
    }

    /// `0x6c` `sha256` — pops a String, pushes the 32-byte SHA-256 digest.
    fn op_sha256(&mut self) -> Result<(), VMError> {
        use sha2::{Digest, Sha256};
        let s = self.pop_string()?;
        let digest = Sha256::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x6d` `sha512` — pops a String, pushes the 64-byte SHA-512 digest.
    fn op_sha512(&mut self) -> Result<(), VMError> {
        use sha2::{Digest, Sha512};
        let s = self.pop_string()?;
        let digest = Sha512::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x6e` `sha3` — pops a String, pushes the 32-byte SHA3-256 digest
    /// (FIPS-202; padding `0x06`).
    fn op_sha3(&mut self) -> Result<(), VMError> {
        use sha3::{Digest, Sha3_256};
        let s = self.pop_string()?;
        let digest = Sha3_256::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x4e` `keccak256` — pops a String, pushes the 32-byte Keccak-256
    /// digest (pre-FIPS Keccak; padding `0x01`). Distinct from `sha3` for
    /// Ethereum compatibility.
    fn op_keccak256(&mut self) -> Result<(), VMError> {
        use sha3::{Digest, Keccak256};
        let s = self.pop_string()?;
        let digest = Keccak256::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x6f` `log` — `str → ø`. Pops a String, emits
    /// `TxEntry::Data(bytes)` into the txlog. Mirrors zkvm's
    /// `log` opcode (same byte). Witness-bearing String variants
    /// serialize via `to_bytes` so prover and verifier emit the
    /// same canonical bytes.
    fn op_log(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        self.txlog.push(crate::tx::TxEntry::Data(s.to_bytes()));
        Ok(())
    }

    // ── Dict ops ────────────────────────────────────────

    /// Pops the top value, asserting it is a `Dict`.
    fn pop_dict(&mut self) -> Result<Dict, VMError> {
        match self.pop_value()? {
            Value::Dict(d) => Ok(d),
            _ => Err(VMError::TypeNotDict),
        }
    }

    /// `0x60` `dict` — `... val key val key n → dict`. Pops `n`, then `n`
    /// key/value pairs (key on top of each pair). Duplicate keys error.
    fn op_dict(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let mut dict = Dict::new();
        for _ in 0..n {
            let key = self.pop_int253()?;
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
        let k = self.pop_int253()?;
        let mut dict = self.pop_dict()?;
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
        let k = self.pop_int253()?;
        let mut dict = self.pop_dict()?;
        let prev = dict.insert(k, v);
        self.push_value(Value::Dict(dict));
        match prev {
            Some(prev_v) => {
                self.push_value(prev_v);
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    /// `0x63` `get` — `dict k → dict' k v`. Removes and returns the
    /// value at `k`. Fails if the key is missing.
    fn op_get(&mut self) -> Result<(), VMError> {
        let k = self.pop_int253()?;
        let mut dict = self.pop_dict()?;
        let v = dict.remove(&k).ok_or(VMError::DictKeyNotFound)?;
        self.push_value(Value::Dict(dict));
        self.push_value(Value::Int253(k));
        self.push_value(v);
        Ok(())
    }

    /// `0x64` `getopt` — `dict k → dict' {v 1 | 0}`. Like `get`, but
    /// soft-fails (pushes `0`) when the key is missing.
    fn op_getopt(&mut self) -> Result<(), VMError> {
        let k = self.pop_int253()?;
        let mut dict = self.pop_dict()?;
        let v = dict.remove(&k);
        self.push_value(Value::Dict(dict));
        match v {
            Some(v) => {
                self.push_value(v);
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    /// `0x65` `getdup` — `dict k → dict {v 1 | 0}`. Copies the value
    /// without consuming it. Soft-fails with `0` on missing key; hard
    /// errors if the value exists but isn't copyable.
    fn op_getdup(&mut self) -> Result<(), VMError> {
        let k = self.pop_int253()?;
        let dict = self.pop_dict()?;
        let copied = match dict.get(&k) {
            Some(v) => Some(v.try_clone()?),
            None => None,
        };
        self.push_value(Value::Dict(dict));
        match copied {
            Some(v) => {
                self.push_value(v);
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    /// `0x66` `first` — `dict → dict {k 1 | 0}`. Pushes the smallest key
    /// alongside a flag, or `0` if the dict is empty.
    fn op_first(&mut self) -> Result<(), VMError> {
        let dict = self.pop_dict()?;
        let k = dict.first_key();
        self.push_value(Value::Dict(dict));
        match k {
            Some(k) => {
                self.push_value(Value::Int253(k));
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    /// `0x67` `last` — `dict → dict {k 1 | 0}`. Mirror of `first`.
    fn op_last(&mut self) -> Result<(), VMError> {
        let dict = self.pop_dict()?;
        let k = dict.last_key();
        self.push_value(Value::Dict(dict));
        match k {
            Some(k) => {
                self.push_value(Value::Int253(k));
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    /// `0x68` `next` — `dict k → dict {k' 1 | 0}`. Smallest key strictly
    /// greater than `k`, or `0` if no such key exists.
    fn op_next(&mut self) -> Result<(), VMError> {
        let k = self.pop_int253()?;
        let dict = self.pop_dict()?;
        let next_k = dict.next_key_after(&k);
        self.push_value(Value::Dict(dict));
        match next_k {
            Some(k) => {
                self.push_value(Value::Int253(k));
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            None => {
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    // ── String ops ──────────────────────────────────────

    /// Helper: converts a stack-popped count into a `usize` ≤ `max`.
    /// Returns `IndexOutOfRange` on overflow or above `max`.
    fn pop_byte_count(&mut self, max: usize) -> Result<usize, VMError> {
        let n_int = self.pop_int253()?;
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
        self.push_value(Value::Int253(Int253::zero()));
    }


    /// `0x40` `readbits` — `s n → s' x 1 | s 0`. Reads `n ≤ 256` bits
    /// from the front of `s`, **LSB-first within each byte**, into bits
    /// `0..n-1` of a fresh `Int253`. The sign bit (in-memory bit 255) is
    /// only set when `n = 256` AND the input's bit 255 is `1`; for any
    /// `n < 256` the result is non-negative.
    ///
    /// Soft-fails (`s 0`, string left untouched) on:
    /// - insufficient bytes in `s` (need `ceil(n/8)` bytes),
    /// - magnitude ≥ ℓ (only reachable when `n ≥ 253`),
    /// - negative zero (only reachable when `n = 256`, magnitude = 0,
    ///   sign bit = 1).
    ///
    /// **Hard-fails** the script (programmer error) when `n > 256`.
    fn op_read_bits(&mut self) -> Result<(), VMError> {
        // Hard-fail at n > 256 (script abort). `pop_byte_count` returns
        // `IndexOutOfRange` if n exceeds the cap, which is the
        // hard-fail path.
        let n = self.pop_byte_count(256)?;
        let s = self.pop_string()?;
        let n_bytes = (n + 7) / 8;
        if s.len() < n_bytes {
            self.push_read_failure(s);
            return Ok(());
        }
        let (remainder, consumed) = s.split_at(n_bytes).expect("length checked");
        let mut int_bytes = [0u8; 32];
        if n_bytes > 0 {
            int_bytes[..n_bytes].copy_from_slice(consumed.as_bytes());
            // Mask off bits above position n-1 within the final byte so
            // that bits `n..n_bytes*8` are forced to zero.
            let tail_bits = n % 8;
            if tail_bits != 0 {
                let mask = (1u8 << tail_bits) - 1;
                int_bytes[n_bytes - 1] &= mask;
            }
        }
        // `Int253::from_bytes` enforces both canonicality (magnitude < ℓ)
        // and the negative-zero invariant. Either violation soft-fails.
        let value = match Int253::from_bytes(int_bytes) {
            Some(v) => v,
            None => {
                // Restore the original string (untouched) and push 0.
                let mut v = Vec::with_capacity(consumed.len() + remainder.len());
                v.extend_from_slice(consumed.as_bytes());
                v.extend_from_slice(remainder.as_bytes());
                self.push_read_failure(String::from(v));
                return Ok(());
            }
        };
        self.push_value(Value::String(remainder));
        self.push_value(Value::Int253(value));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x41` `readint` — `s → s' x 1 | s 0`. Reads the canonical
    /// 32-byte `Int253` (bit 255 = sign, bits 0..254 = magnitude) from
    /// the front of `s`. Equivalent to `readbits(s, 256)`. Soft-fails
    /// on insufficient bytes, magnitude ≥ ℓ, or negative zero.
    fn op_read_int(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        if s.len() < 32 {
            self.push_read_failure(s);
            return Ok(());
        }
        let (remainder, consumed) = s.split_at(32).expect("length checked");
        let mut int_bytes = [0u8; 32];
        int_bytes.copy_from_slice(consumed.as_bytes());
        let value = match Int253::from_bytes(int_bytes) {
            Some(v) => v,
            None => {
                // Restore the original string and push 0.
                let mut v = Vec::with_capacity(consumed.len() + remainder.len());
                v.extend_from_slice(consumed.as_bytes());
                v.extend_from_slice(remainder.as_bytes());
                self.push_read_failure(String::from(v));
                return Ok(());
            }
        };
        self.push_value(Value::String(remainder));
        self.push_value(Value::Int253(value));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x42` `readstr` — `s n → s' s'' 1 | s 0`. Splits off the first
    /// `n` bytes of `s` as a new String.
    fn op_read_str(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let s = self.pop_string()?;
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
        let s = self.pop_string()?;
        if s.len() < 32 {
            self.push_read_failure(s);
            return Ok(());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&s.as_bytes()[..32]);
        let point = Point::from_bytes(arr);
        let (remainder, _consumed) = s.split_at(32).expect("length checked");
        self.push_value(Value::String(remainder));
        self.push_value(Value::Point(point));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x44` `writebits` — `s x n → s'`. Appends the low `n` bits of
    /// `x`'s canonical 32-byte `Int253` representation to `s`. `n` must
    /// be a multiple of 8 and `≤ 256` (byte-aligned strings only —
    /// non-multiples-of-8 hard-fail with `BitCountOutOfRange`).
    ///
    /// The sign bit lives at bit 7 of byte 31; it is included in the
    /// output iff `n = 256`.
    fn op_write_bits(&mut self) -> Result<(), VMError> {
        // Hard-fail at n > 256 (script abort) via the `pop_byte_count` cap.
        let n = self.pop_byte_count(256)?;
        if n % 8 != 0 {
            return Err(VMError::BitCountOutOfRange);
        }
        let n_bytes = n / 8;
        let x = self.pop_int253()?;
        let s = self.pop_string()?;
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
        let x = self.pop_int253()?;
        let s = self.pop_string()?;
        let appended = s.append_bytes(&x.to_bytes());
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// `0x46` `append` — `s s' → s''`. Concatenates two strings.
    fn op_append(&mut self) -> Result<(), VMError> {
        let s2 = self.pop_string()?;
        let s1 = self.pop_string()?;
        self.push_value(Value::String(s1.append(&s2)));
        Ok(())
    }

    /// `0x47` `writezeros` — `s n → s'`. Appends `n` zero bytes.
    fn op_write_zeros(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let s = self.pop_string()?;
        let appended = s.append_bytes(&vec![0u8; n]);
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// `0x48` `bitnot` — `s → s'`. Inverts every bit.
    fn op_bit_not(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        self.push_value(Value::String(s.bit_not()));
        Ok(())
    }

    /// `0x49` `bitor` — `a b → c`. Bytewise OR. Fails if sizes differ.
    fn op_bit_or(&mut self) -> Result<(), VMError> {
        let b = self.pop_string()?;
        let a = self.pop_string()?;
        let c = a.bit_or(&b).ok_or(VMError::BitwiseSizeMismatch)?;
        self.push_value(Value::String(c));
        Ok(())
    }

    /// `0x4a` `bitand` — `a b → c`. Bytewise AND. Fails on size mismatch.
    fn op_bit_and(&mut self) -> Result<(), VMError> {
        let b = self.pop_string()?;
        let a = self.pop_string()?;
        let c = a.bit_and(&b).ok_or(VMError::BitwiseSizeMismatch)?;
        self.push_value(Value::String(c));
        Ok(())
    }

    /// `0x4b` `bitxor` — `a b → c`. Bytewise XOR. Fails on size mismatch.
    fn op_bit_xor(&mut self) -> Result<(), VMError> {
        let b = self.pop_string()?;
        let a = self.pop_string()?;
        let c = a.bit_xor(&b).ok_or(VMError::BitwiseSizeMismatch)?;
        self.push_value(Value::String(c));
        Ok(())
    }

    /// `0x4c` `shiftleft` — `a n → b c`. Shifts `a` left by `n ≤ 256`
    /// bits; pushes the shifted string and the removed bits (zero-padded
    /// on the left).
    fn op_shift_left(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(256)?;
        let a = self.pop_string()?;
        let (shifted, removed) = a.shift_left(n);
        self.push_value(Value::String(shifted));
        self.push_value(Value::String(removed));
        Ok(())
    }

    /// `0x4d` `shiftright` — `a n → b c`. Mirror of `shiftleft`; removed
    /// bits are zero-padded on the right.
    fn op_shift_right(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(256)?;
        let a = self.pop_string()?;
        let (shifted, removed) = a.shift_right(n);
        self.push_value(Value::String(shifted));
        self.push_value(Value::String(removed));
        Ok(())
    }

    // ── Int253 arithmetic, logic, size ──────────────────

    /// `0x50` `abs` — pops an `Int253`, pushes its magnitude (positive
    /// `Int253`), then pushes the original sign as `Int253` (`0` for
    /// non-negative, `1` for negative). Top of stack ends up holding
    /// the sign bit.
    fn op_abs(&mut self) -> Result<(), VMError> {
        let v = self.pop_int253()?;
        let sign_bit = if v.is_negative() { 1u64 } else { 0u64 };
        self.push_value(Value::Int253(v.abs()));
        self.push_value(Value::Int253(Int253::from(sign_bit)));
        Ok(())
    }

    /// `0x51` `eq` — peeks the top two stack values and pushes `1` if
    /// equal, `0` otherwise. The operands themselves stay on the stack.
    /// `0x51 eq` — pop two and test equality.
    ///
    /// - Both `Int253` (or other cleartext-comparable cross types):
    ///   peeks both, pushes `1` if equal, `0` otherwise. Operands
    ///   stay on the stack — `a b → a b {0|1}` per spec.
    /// - At least one `Expression` / `Variable` (only possible in
    ///   external context): pops both, lifts each to
    ///   `Expression-or-Constant`, and pushes a `Constraint::eq` —
    ///   different stack diagram (`a b → constraint`) because the
    ///   equality becomes a CS constraint, not an immediate
    ///   boolean.
    fn op_eq<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let n = self.current_call.stack.len();
        if n < 2 {
            return Err(VMError::StackUnderflow);
        }
        let two_have_non_int = !matches!(self.current_call.stack[n - 1], Value::Int253(_))
            || !matches!(self.current_call.stack[n - 2], Value::Int253(_));
        if self.is_external() && two_have_non_int {
            let b = self.pop_value()?;
            let a = self.pop_value()?;
            let bexpr = Self::into_expression_or_const(b)?;
            let aexpr = Self::into_expression_or_const(a)?;
            self.push_value(Value::Constraint(crate::Constraint::eq(aexpr, bexpr)));
        } else {
            let eq = self.current_call.stack[n - 1]
                .try_eq(&self.current_call.stack[n - 2])?;
            let bit = if eq { 1u64 } else { 0u64 };
            self.push_value(Value::Int253(Int253::from(bit)));
        }
        Ok(())
    }

    /// `0x52 neg` — pop one and negate.
    ///
    /// - `Int253`: cleartext negation (zero stays positive).
    /// - `Expression`: structural negation of the LC (external
    ///   context only; an Expression can only exist on the stack
    ///   after `alloc` / `scalar` / `expr`, all of which require
    ///   external context).
    fn op_neg<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        match self.pop_value()? {
            Value::Int253(v) => {
                self.push_value(Value::Int253(-v));
                Ok(())
            }
            Value::Expression(e) => {
                self.push_value(Value::Expression(-e));
                Ok(())
            }
            other => {
                self.push_value(other);
                Err(VMError::TypeNotInt253)
            }
        }
    }

    /// `0x53 add` — pop two and sum.
    ///
    /// - Both `Int253`: cleartext sum modulo ℓ.
    /// - Otherwise: lift each operand to `Expression` (with Int253
    ///   folding into `Expression::Constant`) and emit an
    ///   LC-addition Expression. Requires external context.
    fn op_add<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        match (a, b) {
            (Value::Int253(x), Value::Int253(y)) => {
                self.push_value(Value::Int253(x + y));
                Ok(())
            }
            (a, b) if self.is_external() => {
                let aexpr = Self::into_expression_or_const(a)?;
                let bexpr = Self::into_expression_or_const(b)?;
                self.push_value(Value::Expression(aexpr + bexpr));
                Ok(())
            }
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// `0x54 mul` — pop two and multiply.
    ///
    /// - Both `Int253`: cleartext product modulo ℓ.
    /// - Otherwise: lift each to `Expression` and emit a CS
    ///   multiplication (constant-folded where both are
    ///   constants, allocates a multiplier gate otherwise).
    ///   Requires external context (needs `delegate.cs()`).
    fn op_mul<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        match (a, b) {
            (Value::Int253(x), Value::Int253(y)) => {
                self.push_value(Value::Int253(x * y));
                Ok(())
            }
            (a, b) if self.is_external() => {
                let aexpr = Self::into_expression_or_const(a)?;
                let bexpr = Self::into_expression_or_const(b)?;
                let product = aexpr.multiply(bexpr, delegate.cs());
                self.push_value(Value::Expression(product));
                Ok(())
            }
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// Lifts a stack value to `Expression`. Int253 folds to
    /// `Expression::Constant`; Expression passes through. Anything
    /// else errors `TypeNotExpression`. Used by `add` / `mul` /
    /// `eq` when at least one operand is non-Int253.
    fn into_expression_or_const(v: Value) -> Result<crate::Expression, VMError> {
        match v {
            Value::Expression(e) => Ok(e),
            Value::Int253(i) => Ok(crate::Expression::constant(i)),
            _ => Err(VMError::TypeNotExpression),
        }
    }

    /// `0x55` `divmod` — `x z → d r`. Truncated division: `sign(d) =
    /// sign(x) XOR sign(z)`, `sign(r) = sign(x)`. Errors on zero divisor.
    fn op_divmod(&mut self) -> Result<(), VMError> {
        let z = self.pop_int253()?;
        let x = self.pop_int253()?;
        let (d, r) = x.div_rem(z).ok_or(VMError::DivByZero)?;
        self.push_value(Value::Int253(d));
        self.push_value(Value::Int253(r));
        Ok(())
    }

    /// `0x56` `mod252` — pops a `String` of 0..=64 bytes, interprets it
    /// as a little-endian unsigned integer, reduces it modulo ℓ, and
    /// pushes the result as a non-negative `Int253`.
    fn op_mod252(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        let bytes = s.as_bytes();
        if bytes.len() > 64 {
            return Err(VMError::StringTooLongForModReduction);
        }
        let mut buf = [0u8; 64];
        buf[..bytes.len()].copy_from_slice(bytes);
        let scalar = Scalar::from_bytes_mod_order_wide(&buf);
        self.push_value(Value::Int253(Int253::from(scalar)));
        Ok(())
    }

    /// `0x57 not` — pop one and negate.
    ///
    /// - `Int253`: zero → `1`, non-zero → `0`.
    /// - `Constraint`: structural negation
    ///   (`Constraint::not(c)`). External context only.
    fn op_not<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        match self.pop_value()? {
            Value::Int253(v) => {
                let r = if v.is_zero() { 1u64 } else { 0u64 };
                self.push_value(Value::Int253(Int253::from(r)));
                Ok(())
            }
            Value::Constraint(c) => {
                self.push_value(Value::Constraint(crate::Constraint::not(c)));
                Ok(())
            }
            other => {
                self.push_value(other);
                Err(VMError::TypeNotInt253)
            }
        }
    }

    /// `0x58 and` — pop two and conjoin.
    ///
    /// - Both `Int253`: logical AND, `1` iff both non-zero.
    /// - At least one `Constraint` (only possible in external
    ///   context): structural `Constraint::and`, with Int253
    ///   operands lifted to `Constraint::Cleartext`.
    fn op_and<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let n = self.current_call.stack.len();
        if n < 2 {
            return Err(VMError::StackUnderflow);
        }
        let either_constraint = matches!(self.current_call.stack[n - 1], Value::Constraint(_))
            || matches!(self.current_call.stack[n - 2], Value::Constraint(_));
        if self.is_external() && either_constraint {
            let b = Self::into_constraint_or_int253(self.pop_value()?)?;
            let a = Self::into_constraint_or_int253(self.pop_value()?)?;
            self.push_value(Value::Constraint(crate::Constraint::and(a, b)));
        } else {
            let b = self.pop_int253()?;
            let a = self.pop_int253()?;
            let r = if !a.is_zero() && !b.is_zero() { 1u64 } else { 0u64 };
            self.push_value(Value::Int253(Int253::from(r)));
        }
        Ok(())
    }

    /// `0x59 or` — mirror of `and` for disjunction.
    fn op_or<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        let n = self.current_call.stack.len();
        if n < 2 {
            return Err(VMError::StackUnderflow);
        }
        let either_constraint = matches!(self.current_call.stack[n - 1], Value::Constraint(_))
            || matches!(self.current_call.stack[n - 2], Value::Constraint(_));
        if self.is_external() && either_constraint {
            let b = Self::into_constraint_or_int253(self.pop_value()?)?;
            let a = Self::into_constraint_or_int253(self.pop_value()?)?;
            self.push_value(Value::Constraint(crate::Constraint::or(a, b)));
        } else {
            let b = self.pop_int253()?;
            let a = self.pop_int253()?;
            let r = if !a.is_zero() || !b.is_zero() { 1u64 } else { 0u64 };
            self.push_value(Value::Int253(Int253::from(r)));
        }
        Ok(())
    }

    /// Lifts a stack value to `Constraint`. Constraint passes
    /// through; Int253 folds to `Constraint::Cleartext(value !=
    /// 0)`. Anything else errors. Used by `and` / `or` / `not`
    /// when at least one operand is a Constraint.
    fn into_constraint_or_int253(v: Value) -> Result<crate::Constraint, VMError> {
        match v {
            Value::Constraint(c) => Ok(c),
            Value::Int253(i) => Ok(crate::Constraint::Cleartext(!i.is_zero())),
            _ => Err(VMError::TypeNotConstraint),
        }
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

    // ── control flow ────────────────────────────────────

    /// `0x79 verify` — pop one and assert truthiness.
    ///
    /// - `Int253`: errors `VerifyFailed` if zero, else pops.
    /// - `Constraint`: hands the constraint to the CS so the proof
    ///   commits to its truth. Requires external context.
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
            other => {
                self.push_value(other);
                Err(VMError::TypeNotInt253)
            }
        }
    }

    /// `0x7b` `run` — pops a `String`, suspends the current Run onto
    /// the run-stack, and switches to a fresh Run over the string's
    /// instructions. For `String::Script(instrs)` the witness slots
    /// (`Alloc(Some(_))`, nested `Input(Some(_))`, …) survive into
    /// the new Run; for `String::Opaque(bytes)` the verifier-side
    /// path parses the bytes and the witness slots default to
    /// `None`. Both sides hash to the same bytecode for the proof
    /// transcript.
    fn op_run(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        let instrs = s.to_instructions()?;
        self.enter_run(instrs)
    }

    /// `0x7c` `loop` — resets the current Run's cursor to the start.
    /// Without a `break`/`return` reachable from inside, this is an
    /// unbounded loop; gas metering is the long-term cap.
    fn op_loop(&mut self) -> Result<(), VMError> {
        self.current_call.current_run.rewind();
        Ok(())
    }

    /// `0x7d` `switch` — pops three values `x a b` (top is `b`), chooses
    /// `a` if `x` is non-zero and `b` if `x` is zero, then enters the
    /// chosen script as a new Run (same semantics as `run`,
    /// including the witness-preserving path for `String::Script`).
    fn op_switch(&mut self) -> Result<(), VMError> {
        let b = self.pop_string()?;
        let a = self.pop_string()?;
        let x = self.pop_int253()?;
        let chosen = if x.is_zero() { b } else { a };
        let instrs = chosen.to_instructions()?;
        self.enter_run(instrs)
    }

    /// `0x7e` `return k` — atomic cross-frame return:
    ///
    /// 1. pop the `k` count (must be a non-negative `Int253`),
    /// 2. assert there is an enclosing call frame to return into
    ///    (otherwise `ReturnAtRoot` — see below),
    /// 3. assert the callee's stack has *exactly* `k` items left,
    /// 4. pop the call frame,
    /// 5. refund leftover gas to the parent,
    /// 6. push the `k` items onto the parent's stack.
    ///
    /// **At the outermost call frame, `return` always errors regardless
    /// of `k`** — `return` semantically requires a recipient, and at
    /// root there is none. Scripts that want to exit early use `break:0`
    /// (end the current Run; if the stack is clean and the run-stack is
    /// empty, the call exits cleanly).
    fn op_return(&mut self) -> Result<(), VMError> {
        let k_int = self.pop_int253()?;
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
        self.current_call.gas_limit = self
            .current_call
            .gas_limit
            .saturating_add(leftover_gas);
        self.current_call.stack.extend(return_values);
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

    /// `0x80..=0x8f` `break:k` — stops the current Run and, if `k > 0`,
    /// also discards the `k` Runs that would have resumed next. If `k`
    /// exceeds the number of suspended Runs in this call, the script
    /// tried to break past the call boundary — fail (`BreakOutOfCall`).
    fn op_break_k(&mut self, k: usize) -> Result<(), VMError> {
        if k > self.current_call.run_stack.len() {
            return Err(VMError::BreakOutOfCall);
        }
        // Discard `k` to-be-resumed Runs.
        for _ in 0..k {
            self.current_call.run_stack.pop();
        }
        // End the current Run by jumping its cursor to the end. The
        // dispatch loop's `finish_run` will pop the next saved Run (or
        // call `finish_call` if none).
        self.current_call.current_run.jump_to_end();
        Ok(())
    }

    // ── helpers ──────────────────────────────────────────

    /// Pops the top value, asserting it is a `String`.
    fn pop_string(&mut self) -> Result<String, VMError> {
        match self.pop_value()? {
            Value::String(s) => Ok(s),
            _ => Err(VMError::TypeNotString),
        }
    }

    /// Pushes the current Run onto the run-stack and replaces it
    /// with a fresh Run over the supplied instructions. Callers
    /// that have raw bytecode (`op_open`'s CallProof leaf,
    /// `op_signrun`'s wire-message script) parse via
    /// `Program::parse` first and pass `Vec<Instruction>` here;
    /// callers with a stack `String` go through
    /// [`String::to_instructions`], which preserves witnesses for
    /// `String::Script` and parses bytes for `String::Opaque`.
    fn enter_run(&mut self, instructions: Vec<crate::ops::Instruction>) -> Result<(), VMError> {
        let new_run = Run::new(instructions);
        let old_run = mem::replace(&mut self.current_call.current_run, new_run);
        self.current_call.run_stack.push(old_run);
        Ok(())
    }

    fn op_nop(&mut self) -> Result<(), VMError> {
        Ok(())
    }

    // ── token helpers ───────────────────────────────────

    /// Pops a `ClearToken` from the stack. Errors `TypeNotClearToken`
    /// for any other variant (including encrypted `Token` /
    /// `WideToken` — those have separate cleartext-vs-CS code paths).
    fn pop_clear_token(&mut self) -> Result<ClearToken, VMError> {
        match self.pop_value()? {
            Value::ClearToken(t) => Ok(t),
            _ => Err(VMError::TypeNotClearToken),
        }
    }

    /// Returns the current call's actor identity, or
    /// `OpcodeRequiresActorContext` if the frame has none
    /// (`ExternalRoot` or a `CellOpen` not nested in an actor call).
    fn require_actor(&self) -> Result<&crate::vm::ActorID, VMError> {
        self.current_call
            .kind
            .actor()
            .ok_or(VMError::OpcodeRequiresActorContext)
    }

    // ── token opcode handlers ───────────────────────────

    /// `0x70 amount` — peeks the top token-shaped value and pushes its
    /// `qty` then `flv` underneath/above it, leaving the source token
    /// on the stack untouched (`token → token qty flv`).
    ///
    /// - `ClearToken`: pushes both as `Int253` (cleartext).
    /// - `Token`: pushes both as `Point` (the compressed commitment
    ///   point of each component — works without a live CS).
    /// - `WideToken`: errors `TypeNotToken` — the encrypted
    ///   intermediate is produced and consumed only by CS opcodes
    ///   and is never inspected via `amount`.
    /// - Other types: `TypeNotToken`.
    fn op_amount(&mut self) -> Result<(), VMError> {
        // The spec diagram leaves the original token on the stack and
        // adds qty + flv above it. We pop the token (to inspect by
        // value), then push back token, qty, flv in that order.
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
                // Restore the value before erroring so the caller's
                // stack isn't silently mutated (matches `op_drop`).
                self.push_value(other);
                Err(VMError::TypeNotToken)
            }
        }
    }

    /// `0x71 issue` — `qty tag → T`. Cleartext branch: pops a tag
    /// `String` and a qty `Int253`; computes the flavor from
    /// `(current actor id, tag)`; emits `TxEntry::Issue` with the
    /// two unblinded commitment points; pushes a fresh `ClearToken`.
    ///
    /// Hard-fails with `TokenRequiresCS` if `qty` is a `Point` (the
    /// encrypted branch is not yet wired). Hard-fails with
    /// `OpcodeRequiresActorContext` if the current frame has no actor
    /// identity. Hard-fails with `TypeNotInt253` for any other qty
    /// type.
    fn op_issue(&mut self) -> Result<(), VMError> {
        let tag = self.pop_string()?;
        let qty_val = self.pop_value()?;
        let qty = match qty_val {
            Value::Int253(i) => i,
            // Encrypted branch: defer to a later phase that wires CS.
            Value::Point(_) => return Err(VMError::TokenRequiresCS),
            _ => return Err(VMError::TypeNotInt253),
        };
        let actor = *self.require_actor()?;
        let flv = flavor_from_actor(&actor, &tag);
        let qty_commit = Commitment::unblinded(qty);
        let flv_commit = Commitment::unblinded(flv);
        self.txlog.push(crate::tx::TxEntry::Issue(
            qty_commit.to_point(),
            flv_commit.to_point(),
        ));
        self.push_value(Value::ClearToken(ClearToken::new(qty, flv)));
        Ok(())
    }

    /// `0x72 retire` — `token → ø`. Consumes a token off the stack and
    /// emits `TxEntry::Retire(qty_point, flv_point)`. Works for both
    /// `ClearToken` (unblinded commitments) and `Token` (the live
    /// commitment points). `WideToken` and other types error
    /// `TypeNotToken`.
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

    /// `0x73 borrow` — pop `(qty, flv)` and produce a debit/credit
    /// pair.
    ///
    /// - Both `Int253`: cleartext borrow — pushes
    ///   `ClearToken(-qty, flv)` on the bottom and
    ///   `ClearToken(qty, flv)` on top.
    /// - Both `Variable`: encrypted borrow — commits both
    ///   commitments to the CS, range-proves the positive `qty`,
    ///   allocates `-qty`, constrains the sum to zero, and pushes
    ///   `WideToken(-qty, flv)` + `Token(qty, flv)`. External
    ///   context only (CS allocation).
    /// - `Point` operand: legacy encrypted-via-point hint, no
    ///   longer accepted — errors `TokenRequiresCS`.
    /// - Anything else: `TypeNotInt253`.
    fn op_borrow<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let flv_val = self.pop_value()?;
        let qty_val = self.pop_value()?;
        match (qty_val, flv_val) {
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

    /// `0x74 merge` — `a b → {c 1 | a b 0}`. Both operands must be
    /// `ClearToken`s. On flavor match, sums quantities and pushes
    /// `(merged, 1)`. On mismatch, restores the originals and pushes
    /// `(a, b, 0)` (soft-fail per spec).
    fn op_merge(&mut self) -> Result<(), VMError> {
        let b = self.pop_clear_token()?;
        let a = self.pop_clear_token()?;
        match a.merge_into(b) {
            Ok(c) => {
                self.push_value(Value::ClearToken(c));
                self.push_value(Value::Int253(Int253::from(1u64)));
            }
            Err((a, b)) => {
                self.push_value(Value::ClearToken(a));
                self.push_value(Value::ClearToken(b));
                self.push_value(Value::Int253(Int253::zero()));
            }
        }
        Ok(())
    }

    /// `0x75 split` — `a q → a' b`. Takes `q` from `a.qty`, returns
    /// the remainder `a' = ClearToken(a.qty - q, a.flv)` (bottom) and
    /// the carved-off `b = ClearToken(q, a.flv)` (top). Both produced
    /// tokens share `a.flv`.
    ///
    /// Hard-fails `TokenSplitOutOfRange` if `q > a.qty`, if `q < 0`,
    /// or if `a.qty < 0`.
    fn op_split(&mut self) -> Result<(), VMError> {
        let q = self.pop_int253()?;
        let a = self.pop_clear_token()?;
        match a.split(q) {
            Some((remainder, new_token)) => {
                self.push_value(Value::ClearToken(remainder));
                self.push_value(Value::ClearToken(new_token));
                Ok(())
            }
            None => Err(VMError::TokenSplitOutOfRange),
        }
    }

    /// `0x78 issueflv` — `cid tag → int`. Pops a tag `String` and an
    /// actor-id `String` (must be exactly 32 bytes), pushes
    /// `flavor_from_actor(cid, tag)` as `Int253`.
    ///
    /// Pure helper — no CS, no txlog effect, no actor-context
    /// requirement. Hard-fails with `MalformedCellEncoding`-style
    /// errors for wrong-length cid: we reuse `TypeNotString` semantics
    /// since the spec doesn't mandate a specific error, by checking
    /// length and erroring `IndexOutOfRange` if cid isn't 32 bytes.
    fn op_issueflv(&mut self) -> Result<(), VMError> {
        let tag = self.pop_string()?;
        let cid_str = self.pop_string()?;
        if cid_str.len() != 32 {
            return Err(VMError::IndexOutOfRange);
        }
        let mut actor_bytes = [0u8; 32];
        actor_bytes.copy_from_slice(cid_str.as_bytes());
        let actor = crate::vm::ActorID(actor_bytes);
        let flv = flavor_from_actor(&actor, &tag);
        self.push_value(Value::Int253(flv));
        Ok(())
    }

    // ── cell helpers ────────────────────────────────────

    /// Pops a `Point` from the stack.
    fn pop_point(&mut self) -> Result<Point, VMError> {
        match self.pop_value()? {
            Value::Point(p) => Ok(p),
            _ => Err(VMError::TypeNotPoint),
        }
    }

    /// Pops a `Cell` from the stack.
    fn pop_cell(&mut self) -> Result<Cell, VMError> {
        match self.pop_value()? {
            Value::Cell(c) => Ok(c),
            _ => Err(VMError::TypeNotCell),
        }
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
    // item 9.5.)

    // `signtx` no longer builds a message at op-time — the TxID-bound
    // message is constructed by the delegate at finalize, when TxID is
    // known. See `DeferredSig::TxBound`.

    /// Constructs the Merlin transcript message for `signrun`.
    ///
    /// Binds the signature to **the program bytes only**. The program is
    /// expected to bind itself to any further context (cell anchor,
    /// actor identity, tx anchor) by including explicit checks
    /// (e.g. `anchor pushstr<expected> eq verify`). This pushes the
    /// binding policy into the program author's hands rather than
    /// pre-baking a fixed envelope; see todo item 9.2 for the rationale.
    fn signrun_message(program: &[u8]) -> Vec<u8> {
        let mut t = Transcript::new(b"flamevm.signrun.v1");
        t.append_message(b"program", program);
        let mut out = vec![0u8; 32];
        t.challenge_bytes(b"msg", &mut out);
        out
    }

    // ── input opcode ───────────────────────────────────

    /// `0x90 input` **[E]** — `string → cell`. Decodes a canonical
    /// wire-encoded cell from `string`, pushes the resulting `Cell`
    /// handle, seeds `last_anchor` from the cell's identity, and emits
    /// a `TxEntry::Input(cell_id)` effect into the txlog.
    ///
    /// **VM is stateless w.r.t. Utreexo.** The opcode does not consult
    /// any accumulator — the caller is expected to have validated the
    /// supplied bytes against the Utreexo proof *outside* the VM
    /// before invoking the script. From the VM's perspective the bytes
    /// simply assert "this cell existed as a UTXO"; the txlog entry
    /// commits the script's reliance on that assertion so the outer
    /// verifier can cross-check it against Utreexo state.
    ///
    /// External-context only — internal transactions cannot consume
    /// Utreexo entries (`step_internal` errors `ExternalOnly` on
    /// `0x90`).
    ///
    /// Hard-fails on:
    /// - non-`String` top of stack (`TypeNotString`),
    /// - bytes that do not decode as a canonical cell
    ///   (`MalformedCellEncoding`), including trailing bytes after the
    ///   cell's last byte.
    /// `0x90 input` — `string → cell`. **[E]** external-only.
    ///
    /// Decodes a wire-encoded `Cell` from a `String` on top of the
    /// stack and pushes the resulting `Cell` handle. Emits
    /// `TxEntry::Input(cell.id())` and seeds the VM's anchor chain
    /// at the cell's ratcheted post-anchor.
    ///
    /// `witness` is `Some` on the prover side when the
    /// consumed cell's payload contains any `Token` entries that
    /// participate in a downstream `mix`. On the wire, witnesses
    /// don't exist — `Cell::decode` always rebuilds Tokens as
    /// `Commitment::Closed(point)`. The prover-side witness queue
    /// pairs each Token entry with its Open `(value, blinding)` so
    /// `value_to_allocated` can later call
    /// `r1cs::Prover::commit(value, blinding)` instead of bailing
    /// `WitnessMissing`.
    ///
    /// Witness validation:
    /// - The witness queue length must equal the number of `Token`
    ///   entries in the decoded payload (`WitnessCountMismatch`).
    /// - Each Open commitment's compressed point must equal the
    ///   decoded Closed commitment's point
    ///   (`WitnessPointMismatch`). This guards against a buggy
    ///   prover passing the wrong blinding factor (the resulting
    ///   proof would silently fail R1CS otherwise).
    fn op_input(
        &mut self,
        witness: Option<&crate::witness::InputWitnesses>,
    ) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_string()?;
        let bytes = s.as_bytes();
        let mut reader: &[u8] = bytes;
        let mut cell = Cell::decode(&mut reader)?;
        if !reader.is_empty() {
            return Err(VMError::MalformedCellEncoding);
        }
        // Re-attach prover-side witnesses to Token payload entries.
        // Verifier-side this branch is dormant (witness is None),
        // so the payload retains its decoded Closed Tokens.
        if let Some(w) = witness {
            self.attach_input_witnesses(&mut cell, w)?;
        }
        let cell_id = cell.id();
        self.txlog.push(crate::tx::TxEntry::Input(cell_id));
        // `Cell::to_anchor()` already ratchets, so this seeds the anchor
        // chain at the post-ratchet point — matching zkvm's
        // `contract_id.to_anchor().ratchet()` semantics.
        self.last_anchor = Some(cell.to_anchor());
        self.push_value(Value::Cell(cell));
        Ok(())
    }

    /// Walks `cell.payload` and swaps each `Value::Token`'s
    /// `Commitment::Closed` for the matching `Commitment::Open`
    /// from `witness`. Non-Token entries are passed through; the
    /// witness queue is consumed in payload order.
    ///
    /// Errors:
    /// - `WitnessCountMismatch` if the queue length doesn't equal
    ///   the count of Token entries.
    /// - `WitnessPointMismatch` if any Open commitment's
    ///   compressed point differs from the decoded Closed point.
    fn attach_input_witnesses(
        &self,
        cell: &mut Cell,
        witness: &crate::witness::InputWitnesses,
    ) -> Result<(), VMError> {
        // Count Tokens to verify queue length up-front. Cheaper than
        // discovering a mismatch mid-walk.
        let token_count = cell
            .payload
            .iter()
            .filter(|v| matches!(v, Value::Token(_)))
            .count();
        if witness.tokens.len() != token_count {
            return Err(VMError::WitnessCountMismatch);
        }
        let mut wi = 0usize;
        for v in cell.payload.iter_mut() {
            if let Value::Token(t) = v {
                let tw = &witness.tokens[wi];
                wi += 1;
                // Both witness commitments must be `Commitment::Open`
                // — the whole point of the witness path is to
                // re-attach openings, so a Closed witness here is a
                // caller bug. Without this check the silent
                // failure cascade would be: copy Closed → Closed
                // (no-op) → `mix` calls `commit_variable` →
                // `commitment.witness()` → `None` →
                // `WitnessMissing` raised far from the real cause.
                // Fail loudly at attach time instead.
                if tw.qty.witness().is_none() || tw.flv.witness().is_none()
                {
                    return Err(VMError::WitnessNotOpen);
                }
                // Point-equality check: the witnessed Open commitment
                // must agree with the on-wire Closed commitment.
                // Mismatch is a prover bug — fail loudly so it's
                // caught at test time rather than as a silent
                // InvalidR1CSProof later.
                if tw.qty.to_point() != t.qty.to_point()
                    || tw.flv.to_point() != t.flv.to_point()
                {
                    return Err(VMError::WitnessPointMismatch);
                }
                t.qty = tw.qty.clone();
                t.flv = tw.flv.clone();
            }
        }
        Ok(())
    }

    // ── cell opcode handlers ────────────────────────────

    /// `0x91 cell` — `args… k pred → cell`. Builds a transient `Cell`
    /// on the stack. Predicate comes in as a `Point` (opaque); payload
    /// items must all be portable. Consumes the VM's `last_anchor`,
    /// then advances it to the new cell's anchor.
    fn op_cell(&mut self) -> Result<(), VMError> {
        let pred_point = self.pop_point()?;
        let k = self.pop_byte_count(usize::MAX)?;
        let payload = self.pop_n_portable(k)?;
        let anchor = self.last_anchor.take().ok_or(VMError::AnchorMissing)?;
        let cell = Cell::new(Predicate::Opaque(pred_point.inner), anchor, payload);
        self.last_anchor = Some(cell.to_anchor());
        self.push_value(Value::Cell(cell));
        Ok(())
    }

    /// `0x92 output` — `args… k pred → ø`. Same construction as `cell`,
    /// but instead of pushing the handle, emits a `TxEntry::Output(cell)`
    /// into the txlog.
    fn op_output(&mut self) -> Result<(), VMError> {
        let pred_point = self.pop_point()?;
        let k = self.pop_byte_count(usize::MAX)?;
        let payload = self.pop_n_portable(k)?;
        let anchor = self.last_anchor.take().ok_or(VMError::AnchorMissing)?;
        let cell = Cell::new(Predicate::Opaque(pred_point.inner), anchor, payload);
        self.last_anchor = Some(cell.to_anchor());
        self.txlog.push(crate::tx::TxEntry::Output(cell));
        Ok(())
    }

    /// `0x93 open` — `cell internal_key neighbors position program args… k → results…`.
    ///
    /// CallProof components are passed as distinct stack values rather
    /// than a packed blob, so scripts can compose proofs dynamically and
    /// the existing String/Dict/Point machinery is reused for free.
    /// On success, pours the cell's payload then the args onto the
    /// current frame's stack and enters a new Run over the unlocked
    /// program. No new call frame — the program shares the current
    /// call's stack, gas, mem, and identity (Run-level cell-open).
    fn op_open(&mut self) -> Result<(), VMError> {
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;
        let program_str = self.pop_string()?;
        let position_str = self.pop_string()?;
        let neighbors_dict = self.pop_dict()?;
        let internal_key_pt = self.pop_point()?;
        let cell = self.pop_cell()?;

        let cp = Self::callproof_from_stack_pieces(
            internal_key_pt,
            &neighbors_dict,
            &position_str,
            &program_str,
        )?;
        // `verify_callproof` succeeds iff `program_str`'s canonical
        // bytes match the predicate tree's leaf (the merkle path
        // was built from `program_str` in `callproof_from_stack_pieces`).
        // So once it's accepted, we can use the witness-bearing
        // `program_str.to_instructions()` instead of re-parsing the
        // leaf bytes — the two are byte-identical, but the former
        // preserves any `String::Script(instrs)` witness slots the
        // prover wrapped the unlock into. Verifier-side
        // (`String::Opaque(bytes)`) falls back to `Program::parse`,
        // same as before.
        let _ = cell.predicate.verify_callproof(&cp)?;
        let instrs = program_str.to_instructions()?;

        for v in cell.payload {
            self.push_value(v);
        }
        for v in args {
            self.push_value(v);
        }
        self.enter_run(instrs)
    }

    /// Builds a `CallProof` from the four stack-popped pieces.
    ///
    /// `neighbors` is expected to be a list-style Dict (keys `0..n-1`)
    /// of 32-byte String values. Any deviation → `MalformedCallProof`.
    fn callproof_from_stack_pieces(
        internal_key: Point,
        neighbors: &Dict,
        position: &String,
        program: &String,
    ) -> Result<CallProof, VMError> {
        // Use `bytes_view` throughout — `as_bytes` panics for any
        // witness-bearing String variant, and the prover can push
        // any of them (e.g. `String::Script` for the unlock script).
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
                    h.copy_from_slice(&s.bytes_view());
                    n_vec.push(h);
                }
                _ => return Err(VMError::MalformedCallProof),
            }
        }
        Ok(CallProof {
            internal_key: internal_key.inner,
            neighbors: n_vec,
            position: position.bytes_view().into_owned(),
            program: program.bytes_view().into_owned(),
        })
    }

    /// `0x98 signtx` — `cell → items… k`. Pops the cell, records a
    /// **TxBound** deferred signature (the cell holder must sign the
    /// transaction's TxID via the tx envelope; no message is built
    /// here), pours the cell's payload onto the current stack, and
    /// pushes the count `k`. No new Run, no new frame — the cell holder
    /// is just authorizing the existing transaction.
    fn op_signtx(&mut self) -> Result<(), VMError> {
        let cell = self.pop_cell()?;
        let k = cell.payload.len();
        // Record both the verification key (the predicate's NUMS-Taproot
        // root point) and the cell id. The id becomes the per-signer
        // message in the Phase-20 multi-message context, so the same
        // (key, cell_id) pair can be signed once across many TxBound
        // items by a single multi-signature.
        let cell_id = cell.id();
        self.deferred_sigs.push(DeferredSig::TxBound {
            verification_key: cell.predicate.verification_key(),
            cell_id,
        });
        for v in cell.payload {
            self.push_value(v);
        }
        self.push_value(Value::Int253(Int253::from(k as u64)));
        Ok(())
    }

    /// `0x99 signrun` — `cell prog sig args… m → items… k`.
    ///
    /// Records an **Explicit** deferred-sig commitment over `prog` only
    /// (the program is responsible for binding further context via
    /// explicit checks inside its code), then pours the cell's payload
    /// and the `m` args onto the current stack and enters a new Run
    /// over `prog`.
    fn op_signrun(&mut self) -> Result<(), VMError> {
        let m = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(m)?;
        let sig_str = self.pop_string()?;
        let prog_str = self.pop_string()?;
        let cell = self.pop_cell()?;
        let sig_bytes = sig_str.as_bytes();
        if sig_bytes.len() != 64 {
            return Err(VMError::BadSignatureBytes);
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(sig_bytes);
        // The deferred-sig message commits to the script's CANONICAL
        // wire bytes — same on both sides regardless of whether
        // the prover pushed `String::Script(instrs)` or raw bytes.
        // Materialise via `bytes_view` so both `Script` and
        // `Opaque` produce identical message bytes.
        let program_bytes = prog_str.bytes_view().into_owned();
        let msg = Self::signrun_message(&program_bytes);
        self.deferred_sigs.push(DeferredSig::Explicit {
            verification_key: cell.predicate.verification_key(),
            message: msg,
            signature: sig,
        });
        for v in cell.payload {
            self.push_value(v);
        }
        for v in args {
            self.push_value(v);
        }
        // Witness-preserving path: `Script(instrs)` returns instrs
        // verbatim; `Opaque(bytes)` parses them.
        let instrs = prog_str.to_instructions()?;
        self.enter_run(instrs)
    }

    // ── CS opcode handlers + Expression overloads ──────

    /// Pops a `Variable` from the stack.
    fn pop_variable(&mut self) -> Result<crate::Variable, VMError> {
        match self.pop_value()? {
            Value::Variable(v) => Ok(v),
            _ => Err(VMError::TypeNotVariable),
        }
    }

    /// Pops an `Expression` from the stack.
    fn pop_expression(&mut self) -> Result<crate::Expression, VMError> {
        match self.pop_value()? {
            Value::Expression(e) => Ok(e),
            _ => Err(VMError::TypeNotExpression),
        }
    }

    /// Pops a `Constraint` from the stack.
    fn pop_constraint(&mut self) -> Result<crate::Constraint, VMError> {
        match self.pop_value()? {
            Value::Constraint(c) => Ok(c),
            _ => Err(VMError::TypeNotConstraint),
        }
    }

    /// `0x5c alloc` — allocates a low-level R1CS variable. The witness
    /// comes from `Instruction::Alloc(Option<Int253>)`: `Some(i)` on
    /// the prover side (cleartext value the CS uses when proving),
    /// `None` on the verifier side (no assignment — the variable is
    /// algebraically constrained later by `eq` / `verify`). Pushes
    /// `Expression::LinearCombination([(v, 1)], witness?)` so
    /// downstream arithmetic / equality ops see a one-term Expression.
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
            vec![(r1cs_var, curve25519_dalek::scalar::Scalar::one())],
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
        let var = self.pop_variable()?;
        let (_point, r1cs_var) = delegate.commit_variable(&var.commitment)?;
        let witness = var.commitment.assignment();
        let expr = crate::Expression::LinearCombination(
            vec![(r1cs_var, Scalar::one())],
            witness,
        );
        self.push_value(Value::Expression(expr));
        Ok(())
    }

    // ── range proofs ───────────────────────────────────

    /// Legacy helper kept only for `op_range`'s Constraint
    /// lift-from-Int253 case. The polymorphic `and` / `or` / `not`
    /// handlers inline the equivalent logic via
    /// `into_constraint_or_int253`.
    fn pop_constraint_or_int253(&mut self) -> Result<crate::Constraint, VMError> {
        match self.pop_value()? {
            Value::Constraint(c) => Ok(c),
            Value::Int253(i) => Ok(crate::Constraint::Cleartext(!i.is_zero())),
            _ => Err(VMError::TypeNotConstraint),
        }
    }

    /// `0x5e range` — `expr n → expr`. Pops the bit-count `n` (Int253,
    /// must be in `[1, 64]`) and the `Expression`, adds an `n`-bit
    /// range-proof gadget asserting `0 ≤ expr.value < 2^n`, and
    /// pushes the Expression back unchanged so callers can continue
    /// using it.
    ///
    /// For `Expression::Constant(int)`: range proof reduces to a
    /// cleartext check (no CS work), erroring `InvalidBitrange` if
    /// the constant doesn't fit. For `LinearCombination`: invokes
    /// `spacesuit::range_proof` with the prover's witness (when
    /// available) and a freshly-built `LinearCombination` over the
    /// expression's terms.
    fn op_range<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::r1cs::LinearCombination as LC;
        use spacesuit::BitRange;

        // Pop n (bit-count) — must be a non-negative Int253 in [1, 64].
        let n_int = self.pop_int253()?;
        let n_u64 = n_int.to_u64().ok_or(VMError::BitCountOutOfRange)?;
        let n_usize = usize::try_from(n_u64).map_err(|_| VMError::BitCountOutOfRange)?;
        if n_usize == 0 {
            return Err(VMError::BitCountOutOfRange);
        }
        let bit_range = BitRange::new(n_usize).ok_or(VMError::BitCountOutOfRange)?;

        let expr = self.pop_expression()?;

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
                // Build the LC from the term list. zkvm uses
                // `r1cs::LinearCombination::from_iter(terms)`.
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

    // ── scalar / commit / decrypt / encrypted token ops ────

    /// `0x5a scalar` — `string → expr`. Pops a String, downcasts to
    /// `Int253` via `String::to_scalar`, pushes `Expression::Constant`.
    /// For `String::Opaque(bytes)`, the bytes are parsed as a
    /// canonical sign-magnitude Int253. For `String::Scalar(i)`, the
    /// witness is extracted directly.
    fn op_scalar(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_string()?;
        let int = s.to_scalar()?;
        self.push_value(Value::Expression(crate::Expression::constant(int)));
        Ok(())
    }

    /// `0x5b commit` — `string → var`. Pops a String, downcasts to
    /// `Commitment`, wraps in `Variable { commitment }`. Verifier:
    /// `String::Opaque(point bytes)` → `Commitment::Closed(point)`.
    /// Prover: `String::Commitment(Open(witness))` → witness preserved.
    /// Downstream `expr` opcode then calls `commit_variable` on the
    /// resulting Variable to bind it into the CS.
    fn op_commit(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let s = self.pop_string()?;
        let commitment = s.to_commitment()?;
        let var = crate::Variable { commitment };
        self.push_value(Value::Variable(var));
        Ok(())
    }

    /// Encrypted-branch body for `0x73 borrow`. Caller has already
    /// popped `(qty, flv)` and verified both are `Variable`; this
    /// just runs the CS plumbing — range-proof + additive-inverse
    /// allocation — and pushes the `WideToken` / `Token` pair.
    /// Mirrors zkvm's `borrow` exactly.
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
        // 64-bit range proof on the positive qty (matches zkvm BitRange::max()).
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

    /// `0x7a fee` — `qty flv → widetoken`. **[E]** external-only.
    ///
    /// Pops a non-negative `qty: Int253` (must fit in `u64` and be
    /// `≤ MAX_FEE`) and a `flv: Int253`. Records `TxEntry::Fee(qty)`
    /// into the txlog and bumps `VM::total_fee`. Allocates a fresh
    /// `WideToken` in the CS with `q = -qty` and `f = flv`, constrains
    /// both to their cleartext values (unblinded), and pushes the
    /// debt token to the stack so the script must balance it against
    /// real tokens (typically via `mix`).
    ///
    /// Cleartext-only: the qty is exposed as a `u64` (recorded in
    /// `TxEntry::Fee`). A future blinded-fee branch — gated by ADR
    /// — would carry a Pedersen commitment instead, mirroring how
    /// `issue` evolved from cleartext to encrypted.
    ///
    /// Hard-fails:
    /// - `FeeQtyNegative` if `qty < 0`.
    /// - `FeeTooHigh` if `qty > MAX_FEE` or the per-tx accumulator
    ///   would exceed `MAX_FEE`.
    /// - `TypeNotInt253` if either operand isn't an `Int253`.
    fn op_fee<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::r1cs::ConstraintSystem;
        // Stack convention: flv on top, qty below — matches spec
        // `qty flv → widetoken` and the `borrow` opcode pattern.
        let flv = self.pop_int253()?;
        let qty = self.pop_int253()?;
        // Reject negative qty up front. A negative fee would be a
        // refund and Flame has no refund mechanism — the negative
        // half is the debt token returned to the caller, not the
        // recorded fee amount.
        if qty.is_negative() {
            return Err(VMError::FeeQtyNegative);
        }
        // Pack `qty` into u64 for the txlog. `Int253::to_u64()`
        // returns `None` for magnitudes >= 2^64; reject those before
        // CheckedFee even sees them — keeps the cap policy a single
        // `if fee > MAX_FEE` check instead of two-stage cap math.
        let qty_u64 = qty.to_u64().ok_or(VMError::FeeTooHigh)?;
        // Aggregate into the per-tx accumulator. Errors `FeeTooHigh`
        // if the single arg or the running total exceeds `MAX_FEE`.
        self.total_fee.add(qty_u64)?;
        // Build the WideToken debt half in the CS. Mirrors the
        // (cleartext) zkvm pattern: allocate q + f, constrain to
        // `-qty` and `flv` respectively. The cleartext branch leaves
        // the witness side fully known to both prover and verifier;
        // no Pedersen commitment needed.
        let qty_scalar: curve25519_dalek::scalar::Scalar = qty.into();
        let flv_scalar: curve25519_dalek::scalar::Scalar = flv.into();
        // Allocate `q` with witness = -qty. Constrain `q + qty = 0`
        // so `q == -qty`. (Bulletproofs needs an explicit linear
        // constraint; you can't just assign the scalar.)
        let q_var = delegate
            .cs()
            .allocate(Some(-qty_scalar))
            .map_err(VMError::R1CSError)?;
        delegate.cs().constrain(q_var + qty_scalar);
        // Allocate `f` with witness = flv. Constrain `f - flv = 0`.
        let f_var = delegate
            .cs()
            .allocate(Some(flv_scalar))
            .map_err(VMError::R1CSError)?;
        delegate.cs().constrain(f_var - flv_scalar);
        // Witness assignment (prover side): match the constraint
        // shape so downstream `mix` sees a fully-witnessed
        // AllocatedValue. Verifier side: assignment = None.
        let assignment = Some(spacesuit::Value {
            q: -spacesuit::SignedInteger::from(qty_u64),
            f: flv_scalar,
        });
        // Push debt WideToken.
        let wide = crate::WideToken(spacesuit::AllocatedValue {
            q: q_var,
            f: f_var,
            assignment,
        });
        self.push_value(Value::WideToken(wide));
        // Record TxEntry::Fee *after* CS allocation: keeps the
        // ordering predictable in the merkle tree if a future variant
        // needs to commit the qty/flv points instead.
        self.txlog.push(crate::tx::TxEntry::Fee(qty_u64));
        Ok(())
    }

    /// Converts a stack value into a `spacesuit::AllocatedValue` for
    /// the cloak gadget. Mirrors zkvm's `item_to_wide_value`:
    /// - `Token`: commit both Commitments to the CS, build AllocatedValue.
    /// - `WideToken`: unwrap the inner AllocatedValue (already in CS).
    /// - `ClearToken`: promote to unblinded `Token`, then commit.
    /// - Other types: error `TypeNotToken`.
    fn value_to_allocated<D: Delegate>(
        &mut self,
        value: Value,
        delegate: &mut D,
    ) -> Result<spacesuit::AllocatedValue, VMError> {
        match value {
            // Use the `allocated()` accessor (rather than `.0`) so the
            // wrapper stays the only public surface for inspecting a
            // WideToken's CS-bound shape — keeps the spacesuit
            // dependency from leaking through `w.0` callsites.
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

    /// `0x76 mix` — `anytokens… commitments… m n → values`. Pops
    /// `n` (output count) then `m` (input count) as `Int253`; then
    /// `n` output commitment String pairs (qty / flv on top of each
    /// pair); then `m` input token-shaped values. Invokes
    /// `spacesuit::cloak` to constrain that inputs balance with
    /// outputs per flavor (each output range-proven 64-bit, all
    /// values shuffled consistently). Pushes `n` output `Token`s.
    ///
    /// Mirrors zkvm's `cloak(m, n)` opcode exactly.
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
            let flv_str = self.pop_string()?;
            let qty_str = self.pop_string()?;
            let flv_commit = flv_str.to_commitment()?;
            let qty_commit = qty_str.to_commitment()?;
            let token = crate::Token::new(qty_commit, flv_commit);
            // Build the AllocatedValue against the CS.
            let allocated = self.value_to_allocated(
                Value::Token(token.clone()),
                delegate,
            )?;
            // Insert at front so the deepest output ends up at cloak_outs[0],
            // matching zkvm's ordering convention.
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
    /// blinding factors (`f'`, `q'`). Verifies that
    /// `token.qty.to_point() == q*B + q'*B_blinding` and analogously
    /// for the flavor, then pushes a `ClearToken(q, f)`.
    ///
    /// All four scalar operands (`f`, `f'`, `q`, `q'`) are popped as
    /// `Int253`. The Token is popped last (deepest on stack). Errors
    /// `CleartextConstraintFalse` if either commitment doesn't open.
    fn op_decrypt(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        use bulletproofs::PedersenGens;
        let q_blind = self.pop_int253()?;
        let q_value = self.pop_int253()?;
        let f_blind = self.pop_int253()?;
        let f_value = self.pop_int253()?;
        let token = match self.pop_value()? {
            Value::Token(t) => t,
            _ => return Err(VMError::TypeNotToken),
        };
        let gens = PedersenGens::default();
        let expected_qty_point = gens
            .commit(q_value.to_scalar_mod_order(), q_blind.to_scalar_mod_order())
            .compress();
        let expected_flv_point = gens
            .commit(f_value.to_scalar_mod_order(), f_blind.to_scalar_mod_order())
            .compress();
        if expected_qty_point != token.qty.to_point()
            || expected_flv_point != token.flv.to_point()
        {
            return Err(VMError::CleartextConstraintFalse);
        }
        self.push_value(Value::ClearToken(ClearToken::new(q_value, f_value)));
        Ok(())
    }
}

// ── helpers ─────────────────────────────────────────────────

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

/// Converts an `Int253` witness into a `spacesuit::SignedInteger` if it
/// fits the [-(2^64), 2^64] range spacesuit operates over. Out-of-range
/// witnesses on the prover side error `InvalidBitrange` here (the
/// `range_proof` gadget itself would reject the assignment downstream,
/// but failing early gives a clearer error code).
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
