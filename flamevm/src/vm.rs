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
    TxBound { verification_key: CompressedRistretto },

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

    /// Mutable access to the constraint system.
    fn cs(&mut self) -> &mut Self::CS;

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

// ── Run ──────────────────────────────────────────────────────────

/// A single program being interpreted by the VM. Two shapes share a
/// `(content, index)` skeleton:
///
/// - `Bytecode { script, pc }` — verifier-side and internal-context:
///   parses one [`Instruction`] from `script[pc..]` at each step.
/// - `Queue { instructions, index }` — prover-side: returns
///   `instructions[index]` (with witness baked into its variant)
///   and advances `index`.
///
/// Both expose [`Run::next_instruction`], so the dispatch loop is
/// identical — the prover/verifier asymmetry lives only in how the
/// Run is constructed. `loop` (resets the cursor to 0) and `break:k`
/// (jumps the cursor to end) work identically for both.
///
/// Multiple Runs may nest within one call (via `run` / `loop` /
/// `switch`); each pushes onto `CallFrame.run_stack` and is resumed
/// on `break` / `return` / end-of-program.
pub enum Run {
    /// Walks raw bytecode and parses Instructions on the fly.
    Bytecode { script: Vec<u8>, pc: usize },
    /// Walks a pre-decoded list of Instructions with witnesses
    /// already attached (prover side).
    Queue {
        instructions: Vec<crate::ops::Instruction>,
        index: usize,
    },
}

impl Run {
    /// Constructs a Run that walks `script` as bytecode (verifier /
    /// internal / nested `run`/`switch`).
    pub fn new(script: Vec<u8>) -> Self {
        Run::Bytecode { script, pc: 0 }
    }

    /// Constructs a Run that walks a pre-decoded Program (prover's
    /// main program — the witness-bearing variant of [`Instruction`]
    /// is preserved at each step).
    pub(crate) fn from_program(program: crate::program::Program) -> Self {
        Run::Queue {
            instructions: program.instructions().to_vec(),
            index: 0,
        }
    }

    /// Returns the next [`Instruction`] in this Run, advancing the
    /// cursor. `Ok(None)` at end of program.
    pub(crate) fn next_instruction(
        &mut self,
    ) -> Result<Option<crate::ops::Instruction>, VMError> {
        match self {
            Run::Bytecode { script, pc } => {
                if *pc >= script.len() {
                    return Ok(None);
                }
                let mut slice: &[u8] = &script[*pc..];
                let before = slice.len();
                let instr = crate::ops::Instruction::parse(&mut slice)?;
                *pc += before - slice.len();
                Ok(Some(instr))
            }
            Run::Queue { instructions, index } => {
                if *index >= instructions.len() {
                    return Ok(None);
                }
                let instr = instructions[*index].clone();
                *index += 1;
                Ok(Some(instr))
            }
        }
    }

    /// True iff the Run has reached its end.
    pub(crate) fn is_finished(&self) -> bool {
        match self {
            Run::Bytecode { script, pc } => *pc >= script.len(),
            Run::Queue { instructions, index } => *index >= instructions.len(),
        }
    }

    /// Resets the cursor to the start of the Run. Used by `loop`.
    fn rewind(&mut self) {
        match self {
            Run::Bytecode { pc, .. } => *pc = 0,
            Run::Queue { index, .. } => *index = 0,
        }
    }

    /// Jumps the cursor past the end of the Run, so the next call to
    /// `next_instruction` returns `None`. Used by `break:k`.
    fn jump_to_end(&mut self) {
        match self {
            Run::Bytecode { script, pc } => *pc = script.len(),
            Run::Queue { instructions, index } => *index = instructions.len(),
        }
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
    pub fn new(
        script: Vec<u8>,
        kind: CallKind,
        gas_limit: u64,
        mem_limit: u64,
        newbytes: u64,
    ) -> Self {
        Self::new_with_run(
            Run::new(script),
            kind,
            gas_limit,
            mem_limit,
            newbytes,
        )
    }

    /// Like [`CallFrame::new`] but takes a pre-constructed [`Run`] —
    /// used by the prover-side entry point ([`VM::run_external_program`])
    /// which needs a `Run::Queue` over a witness-bearing Program rather
    /// than a `Run::Bytecode` over a script slice.
    pub fn new_with_run(
        run: Run,
        kind: CallKind,
        gas_limit: u64,
        mem_limit: u64,
        newbytes: u64,
    ) -> Self {
        Self {
            stack: Vec::new(),
            current_run: run,
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

/// Outcome of a successful transaction execution.
#[derive(Debug)]
pub struct TxResult {
    pub gas_used: u64,
    pub vbytes_used: u64,
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

    /// Signature checks deferred to `Delegate::finalize`. Always empty in
    /// internal mode.
    deferred_sigs: Vec<DeferredSig>,
}

impl VM {
    /// Executes an external transaction script with the given delegate.
    /// Consumes the delegate (calls `finalize` at the end).
    pub fn execute_external<D: Delegate>(
        header: TxHeader,
        script: Vec<u8>,
        gas_limit: u64,
        mem_limit: u64,
        mut delegate: D,
    ) -> Result<TxResult, VMError> {
        let mut vm = Self::new(
            header,
            CallFrame::new(script, CallKind::ExternalRoot, gas_limit, mem_limit, 0),
        );
        while vm.step_external(&mut delegate)? {}
        let sigs = mem::take(&mut vm.deferred_sigs);
        delegate.finalize(sigs)?;
        Ok(vm.into_result())
    }

    /// Runs an external transaction *bytecode* to completion without
    /// calling `Delegate::finalize`. Returns the resource summary plus
    /// the accumulated deferred signatures; the caller (typically
    /// [`crate::Verifier::verify`]) then drives its own
    /// proof-verification step against the borrowed delegate before
    /// discarding it.
    ///
    /// Lower-level counterpart of [`Self::execute_external`], which
    /// consumes the delegate and finalizes it inline. Both share the
    /// same dispatch loop; only the post-run lifecycle differs.
    pub(crate) fn run_external<D: Delegate>(
        header: TxHeader,
        script: Vec<u8>,
        gas_limit: u64,
        mem_limit: u64,
        delegate: &mut D,
    ) -> Result<(TxResult, Vec<DeferredSig>), VMError> {
        let mut vm = Self::new(
            header,
            CallFrame::new(script, CallKind::ExternalRoot, gas_limit, mem_limit, 0),
        );
        while vm.step_external(delegate)? {}
        let sigs = mem::take(&mut vm.deferred_sigs);
        Ok((vm.into_result(), sigs))
    }

    /// Prover-side counterpart of [`Self::run_external`]: takes a
    /// [`crate::Program`] (witness-bearing Instructions) instead of
    /// bytecode. The VM walks the program via `Run::Queue`, so
    /// `Instruction::Alloc(Some(witness))` retains its witness when
    /// dispatched.
    pub(crate) fn run_external_program<D: Delegate>(
        header: TxHeader,
        program: crate::program::Program,
        gas_limit: u64,
        mem_limit: u64,
        delegate: &mut D,
    ) -> Result<(TxResult, Vec<DeferredSig>), VMError> {
        let mut vm = Self::new(
            header,
            CallFrame::new_with_run(
                Run::from_program(program),
                CallKind::ExternalRoot,
                gas_limit,
                mem_limit,
                0,
            ),
        );
        while vm.step_external(delegate)? {}
        let sigs = mem::take(&mut vm.deferred_sigs);
        Ok((vm.into_result(), sigs))
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
        let mut vm = Self::new(
            header,
            CallFrame::new(script, kind, message.gas, mem_limit, message.vbytes),
        );
        while vm.step_internal()? {}
        Ok(vm.into_result())
    }

    fn new(header: TxHeader, initial_call: CallFrame) -> Self {
        Self {
            header,
            last_anchor: None,
            gas_used: 0,
            vbytes_used: 0,
            current_call: initial_call,
            call_stack: Vec::new(),
            txlog: Vec::new(),
            deferred_sigs: Vec::new(),
        }
    }

    fn into_result(self) -> TxResult {
        TxResult {
            gas_used: self.gas_used,
            vbytes_used: self.vbytes_used,
        }
    }

    // ── Dispatch ─────────────────────────────────────────────────

    /// Executes one [`Instruction`] in external context. Returns
    /// `Ok(true)` to keep running, `Ok(false)` to stop (entire tx
    /// finished).
    fn step_external<D: Delegate>(&mut self, delegate: &mut D) -> Result<bool, VMError> {
        let Some(instr) = self.current_call.current_run.next_instruction()? else {
            return self.finish_run();
        };
        self.dispatch_external(instr, delegate)?;
        Ok(true)
    }

    /// Executes one [`Instruction`] in internal context.
    fn step_internal(&mut self) -> Result<bool, VMError> {
        let Some(instr) = self.current_call.current_run.next_instruction()? else {
            return self.finish_run();
        };
        self.dispatch_internal(instr)?;
        Ok(true)
    }

    /// External-context dispatch. Routes CS-bound and external-only
    /// instructions; falls through to common dispatch for the rest.
    /// Expression / Constraint overloads of `add` / `mul` / `eq` /
    /// `neg` / `verify` are detected via a stack-top type peek (top of
    /// stack is an `Expression` or `Constraint`).
    fn dispatch_external<D: Delegate>(
        &mut self,
        instr: crate::ops::Instruction,
        delegate: &mut D,
    ) -> Result<(), VMError> {
        use crate::ops::Instruction as I;
        match instr {
            // ── Phase 11: CS-bound opcodes ────────────────────────
            I::Alloc(w) => self.op_alloc(w, delegate),
            I::Expr => self.op_expr(delegate),
            // ── Phase 12: range proof ─────────────────────────────
            I::Range => self.op_range(delegate),
            // ── Phase 13: CS-bound stack→type lifts ────────────────
            I::Scalar => self.op_scalar(),
            I::Commit => self.op_commit(),
            I::Decrypt => self.op_decrypt(),
            I::Mix => self.op_mix(delegate),
            // ── Phase 11: Expression / Constraint overloads ───────
            I::Neg if self.top_is_expression() => self.op_neg_expr(),
            I::Add if self.top_two_have_non_int253() => self.op_add_expr(),
            I::Mul if self.top_two_have_non_int253() => self.op_mul_expr(delegate),
            I::Eq if self.top_two_have_non_int253() => self.op_eq_expr(),
            I::Verify if self.top_is_constraint() => self.op_verify_constraint(delegate),
            // ── Phase 12: Constraint composition overloads ────────
            I::Not if self.top_is_constraint() => self.op_not_constraint(),
            I::And if self.top_two_have_constraint() => self.op_and_constraint(),
            I::Or if self.top_two_have_constraint() => self.op_or_constraint(),
            // ── Phase 10a: external-only ──────────────────────────
            I::Input => self.op_input(),
            // ── Everything else → common dispatch ─────────────────
            other => self.dispatch_common(other),
        }
    }

    /// Internal-context dispatch. External-only instructions surface a
    /// definite `ExternalOnly` error rather than the generic
    /// `UnknownOpcode` path.
    fn dispatch_internal(&mut self, instr: crate::ops::Instruction) -> Result<(), VMError> {
        use crate::ops::Instruction as I;
        match instr {
            I::Input
            | I::Alloc(_)
            | I::Expr
            | I::Range
            | I::Scalar
            | I::Commit
            | I::Decrypt
            | I::Mix => Err(VMError::ExternalOnly),
            other => self.dispatch_common(other),
        }
    }

    /// Dispatches instructions whose behavior is identical in both
    /// contexts. Unhandled instructions error `UnknownOpcode` (via
    /// `Ext(byte)`) — internal/external dispatchers should intercept
    /// any instruction that has context-dependent semantics before
    /// falling through here.
    fn dispatch_common(&mut self, instr: crate::ops::Instruction) -> Result<(), VMError> {
        use crate::ops::Instruction as I;
        match instr {
            // ── Phase 1: stack literals & manipulation ────────────
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
            // ── Phase 4: string ops ───────────────────────────────
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
            // ── Phase 3: Int253 arithmetic ────────────────────────
            I::Abs => self.op_abs(),
            I::Eq => self.op_eq(),
            I::Neg => self.op_neg(),
            I::Add => self.op_add(),
            I::Mul => self.op_mul(),
            I::DivMod => self.op_divmod(),
            I::Mod252 => self.op_mod252(),
            I::Not => self.op_not(),
            I::And => self.op_and(),
            I::Or => self.op_or(),
            I::Size => self.op_size(),
            // ── Phase 5: Dict ops ─────────────────────────────────
            I::Dict => self.op_dict(),
            I::Put => self.op_put(),
            I::Replace => self.op_replace(),
            I::Get => self.op_get(),
            I::GetOpt => self.op_getopt(),
            I::GetDup => self.op_getdup(),
            I::First => self.op_first(),
            I::Last => self.op_last(),
            I::Next => self.op_next(),
            // ── Phase 6: Hash & Merlin ────────────────────────────
            I::Merlin => self.op_merlin(),
            I::MerlinWrite => self.op_merlin_write(),
            I::MerlinRead => self.op_merlin_read(),
            I::Sha256 => self.op_sha256(),
            I::Sha512 => self.op_sha512(),
            I::Sha3 => self.op_sha3(),
            // ── Phase 8: tokens (cleartext branches) ──────────────
            I::Amount => self.op_amount(),
            I::Issue => self.op_issue(),
            I::Retire => self.op_retire(),
            I::Borrow => self.op_borrow(),
            I::Merge => self.op_merge(),
            I::Split => self.op_split(),
            I::IssueFlv => self.op_issueflv(),
            // ── Phase 2: control flow ─────────────────────────────
            I::Verify => self.op_verify(),
            I::Run => self.op_run(),
            I::Loop => self.op_loop(),
            I::Switch => self.op_switch(),
            I::Return => self.op_return(),
            I::Type => self.op_type(),
            I::BreakK(k) => self.op_break_k(k as usize),
            // ── Phase 9: cells & cell-open ────────────────────────
            I::Cell => self.op_cell(),
            I::Output => self.op_output(),
            I::Open => self.op_open(),
            I::Signtx => self.op_signtx(),
            I::Signrun => self.op_signrun(),
            // ── Context-only — caller should have intercepted ─────
            I::Input
            | I::Alloc(_)
            | I::Expr
            | I::Range
            | I::Scalar
            | I::Commit
            | I::Decrypt
            | I::Mix => {
                // External-only instructions reach common dispatch
                // only via internal context (where they're already
                // intercepted) or via misdispatch. Surface a definite
                // error.
                Err(VMError::ExternalOnly)
            }
            // Unknown / extension opcodes.
            I::Ext(b) => Err(VMError::UnknownOpcode(b)),
        }
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

    // ── Phase 1 opcode handlers ─────────────────────────────────

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

    // ── Phase 6: Hash & Merlin ───────────────────────────────────

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

    // ── Phase 5: Dict ops ────────────────────────────────────────

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

    // ── Phase 4: String ops ──────────────────────────────────────

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

    // ── Phase 3: Int253 arithmetic, logic, size ──────────────────

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
    fn op_eq(&mut self) -> Result<(), VMError> {
        let n = self.current_call.stack.len();
        if n < 2 {
            return Err(VMError::StackUnderflow);
        }
        let eq = self.current_call.stack[n - 1]
            .try_eq(&self.current_call.stack[n - 2])?;
        let bit = if eq { 1u64 } else { 0u64 };
        self.push_value(Value::Int253(Int253::from(bit)));
        Ok(())
    }

    /// `0x52` `neg` — negates an `Int253` (Expression overload deferred
    /// to Phase 11). Zero stays positive.
    fn op_neg(&mut self) -> Result<(), VMError> {
        let v = self.pop_int253()?;
        self.push_value(Value::Int253(-v));
        Ok(())
    }

    /// `0x53` `add` — adds two `Int253`s modulo ℓ (Expression overload
    /// deferred to Phase 11).
    fn op_add(&mut self) -> Result<(), VMError> {
        let y = self.pop_int253()?;
        let x = self.pop_int253()?;
        self.push_value(Value::Int253(x + y));
        Ok(())
    }

    /// `0x54` `mul` — multiplies two `Int253`s modulo ℓ (Expression
    /// overload deferred to Phase 11).
    fn op_mul(&mut self) -> Result<(), VMError> {
        let y = self.pop_int253()?;
        let x = self.pop_int253()?;
        self.push_value(Value::Int253(x * y));
        Ok(())
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

    /// `0x57` `not` — `Int253` boolean negation: zero → `1`, non-zero
    /// → `0`. Constraint overload deferred to Phase 12.
    fn op_not(&mut self) -> Result<(), VMError> {
        let v = self.pop_int253()?;
        let r = if v.is_zero() { 1u64 } else { 0u64 };
        self.push_value(Value::Int253(Int253::from(r)));
        Ok(())
    }

    /// `0x58` `and` — `Int253` logical AND: `1` if both operands are
    /// non-zero, else `0`. Constraint overload deferred to Phase 12.
    fn op_and(&mut self) -> Result<(), VMError> {
        let b = self.pop_int253()?;
        let a = self.pop_int253()?;
        let r = if !a.is_zero() && !b.is_zero() { 1u64 } else { 0u64 };
        self.push_value(Value::Int253(Int253::from(r)));
        Ok(())
    }

    /// `0x59` `or` — `Int253` logical OR: `1` if either operand is
    /// non-zero, else `0`. Constraint overload deferred to Phase 12.
    fn op_or(&mut self) -> Result<(), VMError> {
        let b = self.pop_int253()?;
        let a = self.pop_int253()?;
        let r = if !a.is_zero() || !b.is_zero() { 1u64 } else { 0u64 };
        self.push_value(Value::Int253(Int253::from(r)));
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

    // ── Phase 2: control flow ────────────────────────────────────

    /// `0x79` `verify` — fails the script if the top of stack is zero.
    /// Pops the value on success.
    fn op_verify(&mut self) -> Result<(), VMError> {
        let v = self.pop_int253()?;
        if v.is_zero() {
            return Err(VMError::VerifyFailed);
        }
        Ok(())
    }

    /// `0x7b` `run` — pops a `String`, suspends the current Run onto
    /// the run-stack, and switches to a fresh Run over the string's bytes.
    fn op_run(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        self.enter_run(s.as_bytes().to_vec());
        Ok(())
    }

    /// `0x7c` `loop` — resets the current Run's cursor to the start.
    /// Without a `break`/`return` reachable from inside, this is an
    /// unbounded loop; gas metering (Phase 17) is the long-term cap.
    fn op_loop(&mut self) -> Result<(), VMError> {
        self.current_call.current_run.rewind();
        Ok(())
    }

    /// `0x7d` `switch` — pops three values `x a b` (top is `b`), chooses
    /// `a` if `x` is non-zero and `b` if `x` is zero, then enters the
    /// chosen program as a new Run (same semantics as `run`).
    fn op_switch(&mut self) -> Result<(), VMError> {
        let b = self.pop_string()?;
        let a = self.pop_string()?;
        let x = self.pop_int253()?;
        let chosen = if x.is_zero() { b } else { a };
        self.enter_run(chosen.as_bytes().to_vec());
        Ok(())
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

    // ── Phase 2 helpers ──────────────────────────────────────────

    /// Pops the top value, asserting it is a `String`.
    fn pop_string(&mut self) -> Result<String, VMError> {
        match self.pop_value()? {
            Value::String(s) => Ok(s),
            _ => Err(VMError::TypeNotString),
        }
    }

    /// Pushes the current Run onto the run-stack and replaces it with a
    /// fresh Run over `script`. Used by `run` and `switch`.
    fn enter_run(&mut self, script: Vec<u8>) {
        let new_run = Run::new(script);
        let old_run = mem::replace(&mut self.current_call.current_run, new_run);
        self.current_call.run_stack.push(old_run);
    }

    fn op_nop(&mut self) -> Result<(), VMError> {
        Ok(())
    }

    // ── Phase 8: token helpers ───────────────────────────────────

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

    // ── Phase 8: token opcode handlers ───────────────────────────

    /// `0x70 amount` — peeks the top token-shaped value and pushes its
    /// `qty` then `flv` underneath/above it, leaving the source token
    /// on the stack untouched (`token → token qty flv`).
    ///
    /// - `ClearToken`: pushes both as `Int253` (cleartext).
    /// - `Token`: pushes both as `Point` (the compressed commitment
    ///   point of each component — works without a live CS).
    /// - `WideToken`: errors `TypeNotToken` in Phase 8 (the encrypted
    ///   intermediate isn't constructible until CS opcodes land).
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

    /// `0x71 issue` — `qty tag → T`. Cleartext branch only in Phase 8:
    /// pops a tag `String` and a qty `Int253`; computes the flavor
    /// from `(current actor id, tag)`; emits `TxEntry::Issue` with the
    /// two unblinded commitment points; pushes a fresh `ClearToken`.
    ///
    /// Hard-fails with `TokenRequiresCS` if `qty` is a `Point`
    /// (encrypted branch — lands in Phase 11/12). Hard-fails with
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

    /// `0x73 borrow` — `qty flv → –T +T`. Cleartext branch only in
    /// Phase 8: both operands must be `Int253`. Produces the
    /// debit/credit `ClearToken` pair with the negative qty on the
    /// bottom (non-portable) and the positive qty on top (portable).
    ///
    /// Hard-fails with `TokenRequiresCS` if either operand is a Point
    /// (the encrypted-borrow branch with range-proof needs CS — Phase
    /// 12). Hard-fails with `TypeNotInt253` for any other operand
    /// type.
    fn op_borrow(&mut self) -> Result<(), VMError> {
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

    // ── Phase 9: cell helpers ────────────────────────────────────

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

    // ── Phase 10: input opcode ───────────────────────────────────

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
    fn op_input(&mut self) -> Result<(), VMError> {
        let s = self.pop_string()?;
        let bytes = s.as_bytes();
        let mut reader: &[u8] = bytes;
        let cell = Cell::decode(&mut reader)?;
        if !reader.is_empty() {
            return Err(VMError::MalformedCellEncoding);
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

    // ── Phase 9: cell opcode handlers ────────────────────────────

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
        let program = cell.predicate.verify_callproof(&cp)?.to_vec();

        for v in cell.payload {
            self.push_value(v);
        }
        for v in args {
            self.push_value(v);
        }
        self.enter_run(program);
        Ok(())
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
                    h.copy_from_slice(s.as_bytes());
                    n_vec.push(h);
                }
                _ => return Err(VMError::MalformedCallProof),
            }
        }
        Ok(CallProof {
            internal_key: internal_key.inner,
            neighbors: n_vec,
            position: position.as_bytes().to_vec(),
            program: program.as_bytes().to_vec(),
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
        self.deferred_sigs.push(DeferredSig::TxBound {
            verification_key: cell.predicate.verification_key(),
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
        let program = prog_str.as_bytes().to_vec();
        let msg = Self::signrun_message(&program);
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
        self.enter_run(program);
        Ok(())
    }

    // ── Phase 11: CS opcode handlers + Expression overloads ──────

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

    /// Pops either an `Expression` or an `Int253` (lifted to a
    /// constant Expression). Used by Expression-overloaded
    /// arithmetic ops where one operand may be a cleartext int.
    fn pop_expression_or_const(&mut self) -> Result<crate::Expression, VMError> {
        match self.pop_value()? {
            Value::Expression(e) => Ok(e),
            Value::Int253(i) => Ok(crate::Expression::constant(i)),
            _ => Err(VMError::TypeNotExpression),
        }
    }

    /// True iff the top stack value is an `Expression`.
    fn top_is_expression(&self) -> bool {
        matches!(self.current_call.stack.last(), Some(Value::Expression(_)))
    }

    /// True iff the top stack value is a `Constraint`.
    fn top_is_constraint(&self) -> bool {
        matches!(self.current_call.stack.last(), Some(Value::Constraint(_)))
    }

    /// True iff at least one of the top two values isn't an `Int253` —
    /// i.e. the Expression-overloaded path should apply. (Both Int253
    /// → use the original `try_common` integer path.)
    fn top_two_have_non_int253(&self) -> bool {
        let n = self.current_call.stack.len();
        if n < 2 {
            return false;
        }
        let a = matches!(self.current_call.stack[n - 1], Value::Int253(_));
        let b = matches!(self.current_call.stack[n - 2], Value::Int253(_));
        !(a && b)
    }

    /// True iff at least one of the top two values is a `Constraint`.
    /// Used by `and` / `or` overload dispatch — the cleartext Int253
    /// path runs only when neither operand is already a Constraint.
    fn top_two_have_constraint(&self) -> bool {
        let n = self.current_call.stack.len();
        if n < 2 {
            return false;
        }
        let a = matches!(self.current_call.stack[n - 1], Value::Constraint(_));
        let b = matches!(self.current_call.stack[n - 2], Value::Constraint(_));
        a || b
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

    /// `0x52 neg` Expression overload.
    fn op_neg_expr(&mut self) -> Result<(), VMError> {
        let e = self.pop_expression()?;
        self.push_value(Value::Expression(-e));
        Ok(())
    }

    /// `0x53 add` Expression overload. Either operand may be Int253
    /// (constant-folded into Expression::Constant).
    fn op_add_expr(&mut self) -> Result<(), VMError> {
        let b = self.pop_expression_or_const()?;
        let a = self.pop_expression_or_const()?;
        self.push_value(Value::Expression(a + b));
        Ok(())
    }

    /// `0x54 mul` Expression overload. Allocates a multiplier in the
    /// CS for the non-constant case; constant-folds otherwise.
    fn op_mul_expr<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_expression_or_const()?;
        let a = self.pop_expression_or_const()?;
        let product = a.multiply(b, delegate.cs());
        self.push_value(Value::Expression(product));
        Ok(())
    }

    /// `0x51 eq` Expression overload. Both operands as Expression →
    /// pushes a `Constraint::eq` on top, leaving the operand expressions
    /// consumed. This differs from the Int253 `eq` (which peeks only),
    /// because Expression equality is a constraint to be verified later,
    /// not an immediate boolean.
    fn op_eq_expr(&mut self) -> Result<(), VMError> {
        let b = self.pop_expression_or_const()?;
        let a = self.pop_expression_or_const()?;
        let c = crate::Constraint::eq(a, b);
        self.push_value(Value::Constraint(c));
        Ok(())
    }

    /// `0x79 verify` Constraint overload. Hands the constraint to the
    /// CS so the proof commits to its truth.
    fn op_verify_constraint<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let c = self.pop_constraint()?;
        c.verify(delegate.cs())?;
        Ok(())
    }

    // ── Phase 12: range proofs + Constraint composition ──────────

    /// Pops a `Constraint` if the top of the stack is one, or lifts an
    /// `Int253` to `Constraint::Cleartext(value != 0)`. Mirrors zkvm's
    /// pattern of accepting "constant constraints" alongside witness
    /// constraints, leveraging the Constraint cleartext-fold to keep
    /// the CS minimal.
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

    /// `0x57 not` Constraint overload. Pops a Constraint, pushes its
    /// negation.
    fn op_not_constraint(&mut self) -> Result<(), VMError> {
        let c = self.pop_constraint_or_int253()?;
        self.push_value(Value::Constraint(crate::Constraint::not(c)));
        Ok(())
    }

    /// `0x58 and` Constraint overload. Pops two Constraints (or
    /// Int253-as-Cleartext), pushes their conjunction. Constraint
    /// composition is purely structural — the CS is only touched when
    /// `verify` is called on the resulting Constraint.
    fn op_and_constraint(&mut self) -> Result<(), VMError> {
        let b = self.pop_constraint_or_int253()?;
        let a = self.pop_constraint_or_int253()?;
        self.push_value(Value::Constraint(crate::Constraint::and(a, b)));
        Ok(())
    }

    /// `0x59 or` Constraint overload. Mirror of `and`.
    fn op_or_constraint(&mut self) -> Result<(), VMError> {
        let b = self.pop_constraint_or_int253()?;
        let a = self.pop_constraint_or_int253()?;
        self.push_value(Value::Constraint(crate::Constraint::or(a, b)));
        Ok(())
    }

    // ── Phase 13: scalar / commit / decrypt / encrypted token ops ────

    /// `0x5a scalar` — `string → expr`. Pops a String, downcasts to
    /// `Int253` via `String::to_scalar`, pushes `Expression::Constant`.
    /// For `String::Opaque(bytes)`, the bytes are parsed as a
    /// canonical sign-magnitude Int253. For `String::Scalar(i)`, the
    /// witness is extracted directly.
    fn op_scalar(&mut self) -> Result<(), VMError> {
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
        let s = self.pop_string()?;
        let commitment = s.to_commitment()?;
        let var = crate::Variable { commitment };
        self.push_value(Value::Variable(var));
        Ok(())
    }

    /// `0x76 mix` — `anytokens… commitments… m n → values`. Pops `n`
    /// then `m` (output and input counts) as `Int253`, then `n` output
    /// commitment String pairs (`qty`, `flv`), then `m` input
    /// token-shaped values; invokes the spacesuit cloak gadget to
    /// constrain that inputs balance with outputs per flavor; pushes
    /// `n` output `Token`s.
    ///
    /// **Phase-13 status: minimal scaffold.** Today returns
    /// `WitnessMissing` so callers see a clear "not yet wired" error.
    /// Full cloak-gadget wiring lands once `WideToken` has a
    /// public constructor (Phase 13.5 — needs encrypted-`borrow` to
    /// produce one first). Tests that exercise the dispatch path
    /// confirm the opcode is routed correctly even as the gadget
    /// itself stays stubbed.
    fn op_mix<D: Delegate>(&mut self, _delegate: &mut D) -> Result<(), VMError> {
        Err(VMError::WitnessMissing)
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

// ── Phase 12 helpers ─────────────────────────────────────────────────

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
mod tests {
    use super::*;

    /// Registry whose `resolve_method` always returns the same script and
    /// whose actors have zero vbytes.
    struct StubRegistry {
        script: Vec<u8>,
    }

    impl ActorRegistry for StubRegistry {
        fn resolve_method(
            &self,
            _actor: &ActorID,
            _method: MethodKey,
        ) -> Result<Vec<u8>, VMError> {
            Ok(self.script.clone())
        }

        fn actor_vbytes(&self, _actor: &ActorID) -> Result<u64, VMError> {
            Ok(0)
        }
    }

    fn dummy_header() -> TxHeader {
        TxHeader { version: 1, locktime: 0 }
    }

    fn dummy_message(gas: u64) -> Message {
        Message {
            target: ActorID([0u8; 32]),
            method: MethodKey(0),
            caller: None,
            anchor: Anchor([0u8; 32]),
            payload: Vec::new(),
            gas,
            vbytes: 0,
        }
    }

    #[test]
    fn internal_empty_script_finishes() {
        let mut reg = StubRegistry { script: vec![] };
        let block = BlockContext { height: 0 };
        let result =
            VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
        assert_eq!(result.gas_used, 0);
    }

    #[test]
    fn internal_nop_script_finishes() {
        let mut reg = StubRegistry { script: vec![0x1d, 0x1d, 0x1d] };
        let block = BlockContext { height: 0 };
        let result =
            VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
        assert_eq!(result.gas_used, 0); // gas accounting not yet wired up
    }

    #[test]
    fn internal_unknown_opcode_errors() {
        let mut reg = StubRegistry { script: vec![0x1d, 0xff] };
        let block = BlockContext { height: 0 };
        let err =
            VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap_err();
        assert!(matches!(err, VMError::UnknownOpcode(0xff)));
    }

    #[test]
    fn run_advances_through_instructions() {
        // Bytecode: push:5, drop, nop. Three Instructions, then end.
        let mut run = Run::new(vec![0x05, 0x1c, 0x1d]);
        use crate::ops::Instruction;
        assert!(matches!(
            run.next_instruction().unwrap(),
            Some(Instruction::PushInt(_))
        ));
        assert!(matches!(
            run.next_instruction().unwrap(),
            Some(Instruction::Drop)
        ));
        assert!(matches!(
            run.next_instruction().unwrap(),
            Some(Instruction::Nop)
        ));
        assert!(run.next_instruction().unwrap().is_none());
    }

    #[test]
    fn dirty_stack_at_call_exit_is_an_error() {
        // Strict cross-call semantics: a script that leaves anything on the
        // callee's stack must use `return` to ship those values explicitly.
        // Reaching end-of-script with a non-empty stack is a script bug.
        use crate::Int253;
        let reg = StubRegistry { script: vec![] };
        let block = BlockContext { height: 0 };
        // Re-create what `execute_internal` would, but pre-load the stack.
        let kind = CallKind::InternalRoot {
            actor: ActorID([0u8; 32]),
            method: MethodKey(0),
            caller: None,
            anchor: Anchor([0u8; 32]),
        };
        let mut vm = VM::new(
            dummy_header(),
            CallFrame::new(Vec::new(), kind, 1000, 0, 0),
        );
        vm.current_call.stack.push(Value::Int253(Int253::from(7u64)));
        // First step: empty script → finish_run → finish_call → dirty stack.
        let err = vm.step_internal().unwrap_err();
        assert!(matches!(err, VMError::StackNotClean));
        // Caller's perspective: nothing leaks. (Sanity — there's no parent
        // call to inspect because this is a root frame.)
        let _ = (reg.actor_vbytes(&ActorID([0; 32])), block.height); // silence warnings
    }

    #[test]
    fn callkind_actor_identity() {
        assert!(CallKind::ExternalRoot.actor().is_none());
        let aid = ActorID([1u8; 32]);
        assert_eq!(
            CallKind::InternalRoot {
                actor: aid,
                method: MethodKey(0),
                caller: None,
                anchor: Anchor([0u8; 32]),
            }
            .actor(),
            Some(&aid)
        );
    }

    // ── Phase 1 helpers ──────────────────────────────────────────

    /// Builds a VM running `script` as the entry Run of an InternalRoot.
    fn vm_with_script(script: Vec<u8>) -> VM {
        let kind = CallKind::InternalRoot {
            actor: ActorID([0u8; 32]),
            method: MethodKey(0),
            caller: None,
            anchor: Anchor([0u8; 32]),
        };
        VM::new(
            dummy_header(),
            CallFrame::new(script, kind, 1_000_000, 0, 0),
        )
    }

    /// Runs steps until the current Run is exhausted, *without* invoking
    /// finish_call (so the test can inspect the leftover stack).
    fn run_to_end(vm: &mut VM) -> Result<(), VMError> {
        while !vm.current_call.current_run.is_finished() {
            vm.step_internal()?;
        }
        Ok(())
    }

    fn assert_int(v: &Value, expected: Int253) {
        match v {
            Value::Int253(i) => assert_eq!(*i, expected, "expected {:?}, got {:?}", expected, i),
            other => panic!("expected Int253, got {:?}", value_kind(other)),
        }
    }

    fn value_kind(v: &Value) -> &'static str {
        match v {
            Value::Int253(_) => "Int253",
            Value::String(_) => "String",
            Value::Dict(_) => "Dict",
            Value::Point(_) => "Point",
            Value::Token(_) => "Token",
            Value::WideToken(_) => "WideToken",
            Value::ClearToken(_) => "ClearToken",
            Value::Cell(_) => "Cell",
            Value::Merlin(_) => "Merlin",
            Value::Variable(_) => "Variable",
            Value::Expression(_) => "Expression",
            Value::Constraint(_) => "Constraint",
        }
    }

    // ── push:k (0x00..=0x0f) ─────────────────────────────────────

    #[test]
    fn push_immediate_k_roundtrips_0_to_15() {
        for k in 0..=15u8 {
            let mut vm = vm_with_script(vec![k]);
            run_to_end(&mut vm).unwrap();
            assert_eq!(vm.current_call.stack.len(), 1, "k={}", k);
            assert_int(&vm.current_call.stack[0], Int253::from(k as u64));
        }
    }

    // ── pushint{8,16,64,128,full} (0x10..=0x18) ──────────────────

    #[test]
    fn pushint8_positive_and_negative() {
        let mut pos = vm_with_script(vec![0x10, 42]);
        run_to_end(&mut pos).unwrap();
        assert_int(&pos.current_call.stack[0], Int253::from(42u64));

        let mut neg = vm_with_script(vec![0x11, 42]);
        run_to_end(&mut neg).unwrap();
        assert_int(&neg.current_call.stack[0], Int253::from(-42i64));
    }

    #[test]
    fn pushint16_le_decoding() {
        // 0x12 = positive; bytes 0x02 0x01 LE = 258
        let mut vm = vm_with_script(vec![0x12, 0x02, 0x01]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(258u64));
    }

    #[test]
    fn pushint64_le_decoding() {
        let val: u64 = 0x0102_0304_0506_0708;
        let mut script = vec![0x14];
        script.extend_from_slice(&val.to_le_bytes());
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(val));
    }

    #[test]
    fn pushint128_le_decoding() {
        let val: u128 = 0xFEED_FACE_DEAD_BEEF_CAFE_BABE_BADD_CAFEu128;
        let mut script = vec![0x16];
        script.extend_from_slice(&val.to_le_bytes());
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&val.to_le_bytes());
        let expected =
            Int253::from_parts(false, Scalar::from_canonical_bytes(bytes).unwrap());
        assert_int(&vm.current_call.stack[0], expected);
    }

    #[test]
    fn pushint_full_roundtrip() {
        let expected = Int253::from(-1234567i64);
        let mut script = vec![0x18];
        script.extend_from_slice(&expected.to_bytes());
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], expected);
    }

    #[test]
    fn pushint_full_rejects_negative_zero() {
        // sign bit set, magnitude zero: -0, not representable
        let mut bytes = [0u8; 32];
        bytes[31] = 0x80;
        let mut script = vec![0x18];
        script.extend_from_slice(&bytes);
        let mut vm = vm_with_script(script);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::InvalidInt253Encoding));
    }

    #[test]
    fn pushint8_at_end_of_script_errors() {
        let mut vm = vm_with_script(vec![0x10]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::UnexpectedEndOfScript
        ));
    }

    // ── pushstr (0x19) ───────────────────────────────────────────

    #[test]
    fn pushstr_immediate_length() {
        // sub-varint tag 0, then byte 4 = length 4, then 4 bytes
        let script = vec![0x19, 0x00, 0x04, b'a', b'b', b'c', b'd'];
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::String(s) => assert_eq!(s.as_bytes(), b"abcd"),
            other => panic!("expected String, got {}", value_kind(other)),
        }
    }

    #[test]
    fn pushstr_short_input_errors() {
        let script = vec![0x19, 0x00, 0x04, b'a']; // length says 4, only 1 byte
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::UnexpectedEndOfScript
        ));
    }

    // ── pushpoint (0x1a) ─────────────────────────────────────────

    #[test]
    fn pushpoint_roundtrip() {
        let bytes = [0x42u8; 32];
        let mut script = vec![0x1a];
        script.extend_from_slice(&bytes);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::Point(p) => assert_eq!(p.as_bytes(), &bytes),
            other => panic!("expected Point, got {}", value_kind(other)),
        }
    }

    // ── pushtoken (0x1b) ─────────────────────────────────────────

    #[test]
    fn pushtoken_zero_qty_with_flavor() {
        // push:7, pushtoken — flavor comes from the stack now.
        let mut vm = vm_with_script(vec![0x07, 0x1b]);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => {
                assert!(t.is_zero_qty());
                assert_eq!(t.flv(), Int253::from(7u64));
            }
            other => panic!("expected ClearToken, got {}", value_kind(other)),
        }
    }

    #[test]
    fn pushtoken_requires_int_flavor() {
        // pushstr "x", pushtoken — top is String, not Int253.
        let mut script = pushstr_bytes(b"x");
        script.push(0x1b);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotInt253
        ));
    }

    #[test]
    fn pushtoken_full_flavor_via_pushint_full() {
        // pushint full <bytes>, pushtoken — exercises a non-small flavor.
        let flv = Int253::from(0x1234567890abcdefu64);
        let mut script = vec![0x18];
        script.extend_from_slice(&flv.to_bytes());
        script.push(0x1b);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => {
                assert!(t.is_zero_qty());
                assert_eq!(t.flv(), flv);
            }
            _ => panic!("expected ClearToken"),
        }
    }

    // ── drop (0x1c) ──────────────────────────────────────────────

    #[test]
    fn drop_droppable_int() {
        let mut vm = vm_with_script(vec![0x05, 0x1c]); // push:5, drop
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
    }

    #[test]
    fn drop_underflow_errors() {
        let mut vm = vm_with_script(vec![0x1c]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::StackUnderflow
        ));
    }

    // ── dup / dup:k (0x1e, 0x20..=0x2f) ──────────────────────────

    #[test]
    fn dup_immediate_zero_copies_top() {
        let mut vm = vm_with_script(vec![0x07, 0x20]); // push:7, dup:0
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[0], Int253::from(7u64));
        assert_int(&vm.current_call.stack[1], Int253::from(7u64));
    }

    #[test]
    fn dup_immediate_k_picks_kth_from_top() {
        // push:1, push:2, push:3, dup:2 → 1 2 3 1
        let mut vm = vm_with_script(vec![0x01, 0x02, 0x03, 0x22]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 4);
        assert_int(&vm.current_call.stack[3], Int253::from(1u64));
    }

    #[test]
    fn dup_dynamic_pops_index() {
        // push:9, push:8, push:0, dup → 9 8 (k=0) → 9 8 8
        let mut vm = vm_with_script(vec![0x09, 0x08, 0x00, 0x1e]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[2], Int253::from(8u64));
    }

    #[test]
    fn dup_out_of_range_errors() {
        // push:5, dup:5 (only 1 item on stack)
        let mut vm = vm_with_script(vec![0x05, 0x25]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }

    #[test]
    fn dup_noncopyable_errors() {
        // push:1, pushtoken (linear), dup:0
        let mut vm = vm_with_script(vec![0x01, 0x1b, 0x20]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotCopyable
        ));
    }

    // ── roll / roll:k (0x1f, 0x30..=0x3f) ────────────────────────

    #[test]
    fn roll_immediate_moves_kth_to_top() {
        // push:1, push:2, push:3, roll:2 → 2 3 1
        let mut vm = vm_with_script(vec![0x01, 0x02, 0x03, 0x32]);
        run_to_end(&mut vm).unwrap();
        let stack = &vm.current_call.stack;
        assert_int(&stack[0], Int253::from(2u64));
        assert_int(&stack[1], Int253::from(3u64));
        assert_int(&stack[2], Int253::from(1u64));
    }

    #[test]
    fn roll_zero_is_noop() {
        let mut vm = vm_with_script(vec![0x07, 0x30]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 1);
        assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    }

    #[test]
    fn roll_dynamic_pops_index() {
        // push:1, push:2, push:1, roll  → roll k=1 → stack {1,2} -> {2,1}
        let mut vm = vm_with_script(vec![0x01, 0x02, 0x01, 0x1f]);
        run_to_end(&mut vm).unwrap();
        let stack = &vm.current_call.stack;
        assert_int(&stack[0], Int253::from(2u64));
        assert_int(&stack[1], Int253::from(1u64));
    }

    #[test]
    fn roll_out_of_range_errors() {
        let mut vm = vm_with_script(vec![0x05, 0x35]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }

    // ── Phase 2: control flow helpers ────────────────────────────

    /// Runs `step_internal` until it reports the tx is done (Ok(false)).
    /// Used to exercise full programs including post-`run`/`switch`
    /// resumption and call-frame exit.
    fn run_until_tx_done(vm: &mut VM) -> Result<(), VMError> {
        while vm.step_internal()? {}
        Ok(())
    }

    /// Builds an inline subprogram string-payload as a script that
    /// `pushstr`s the subprogram's bytes. Returns the prefix bytes
    /// (`0x19` + sub-varint length + payload).
    fn pushstr_bytes(payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0x19, 0x00, payload.len() as u8];
        s.extend_from_slice(payload);
        s
    }

    // ── verify (0x79) ────────────────────────────────────────────

    #[test]
    fn verify_truthy_pops() {
        // push:1, verify — succeeds, stack empties.
        let mut vm = vm_with_script(vec![0x01, 0x79]);
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
    }

    #[test]
    fn verify_zero_fails() {
        let mut vm = vm_with_script(vec![0x00, 0x79]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::VerifyFailed
        ));
    }

    #[test]
    fn verify_requires_int() {
        // pushpoint, verify — top is Point not Int253.
        let mut script = vec![0x1a];
        script.extend_from_slice(&[0u8; 32]);
        script.push(0x79);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotInt253
        ));
    }

    // ── run (0x7b) ───────────────────────────────────────────────

    #[test]
    fn run_creates_nested_run() {
        // pushstr [push:7], run — after run, current_run is the
        // subprogram and outer is suspended.
        let mut script = pushstr_bytes(&[0x07]);
        script.push(0x7b);
        let mut vm = vm_with_script(script);
        vm.step_internal().unwrap(); // pushstr
        assert_eq!(vm.current_call.stack.len(), 1);
        vm.step_internal().unwrap(); // run
        assert_eq!(vm.current_call.run_stack.len(), 1);
        assert!(vm.current_call.stack.is_empty());
        vm.step_internal().unwrap(); // push:7 in subprog
        assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    }

    #[test]
    fn run_resumes_outer_after_subprogram_finishes() {
        // pushstr [push:7, drop], run — subprog cleans up, outer ends empty.
        let mut script = pushstr_bytes(&[0x07, 0x1c]);
        script.push(0x7b);
        let mut reg = StubRegistry { script };
        let block = BlockContext { height: 0 };
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
    }

    #[test]
    fn run_requires_string() {
        // push:5, run — top is Int253 not String.
        let mut vm = vm_with_script(vec![0x05, 0x7b]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotString
        ));
    }

    // ── loop (0x7c) ──────────────────────────────────────────────

    #[test]
    fn loop_resets_run_cursor_to_start() {
        // nop, loop — after `loop` the Run cursor is back at the start,
        // so the next step parses `nop` again (not end-of-script).
        let mut vm = vm_with_script(vec![0x1d, 0x7c]);
        vm.step_internal().unwrap(); // nop
        vm.step_internal().unwrap(); // loop
        // Cursor should be at the start: the next instruction is `nop` again.
        let next = vm.current_call.current_run.next_instruction().unwrap();
        assert!(matches!(next, Some(crate::ops::Instruction::Nop)));
    }

    // ── switch (0x7d) ────────────────────────────────────────────

    #[test]
    fn switch_picks_a_when_x_nonzero() {
        // push:1, pushstr [push:9, drop], pushstr [push:8, drop], switch
        // — x=1 → runs a (pushes 9, drops it). End stack empty.
        let mut script = vec![0x01];
        script.extend_from_slice(&pushstr_bytes(&[0x09, 0x1c]));
        script.extend_from_slice(&pushstr_bytes(&[0x08, 0x1c]));
        script.push(0x7d);
        let mut reg = StubRegistry { script };
        let block = BlockContext { height: 0 };
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block)
            .unwrap();
    }

    #[test]
    fn switch_a_actually_runs_when_x_nonzero() {
        // Verifies the *chosen* branch executes by inspecting mid-flight.
        // push:1, pushstr [push:9], pushstr [push:8], switch
        let mut script = vec![0x01];
        script.extend_from_slice(&pushstr_bytes(&[0x09]));
        script.extend_from_slice(&pushstr_bytes(&[0x08]));
        script.push(0x7d);
        let mut vm = vm_with_script(script);
        while !vm.current_call.run_stack.is_empty()
            || !vm.current_call.current_run.is_finished()
        {
            // Pre-switch: keep stepping until switch happens (run_stack
            // becomes non-empty) and then the chosen subprogram runs to
            // its end.
            if !vm.step_internal().unwrap() {
                break;
            }
            if !vm.current_call.run_stack.is_empty()
                && vm.current_call.current_run.is_finished()
            {
                break;
            }
        }
        // The 9 (from branch a) should be the only stack item.
        assert_int(
            vm.current_call.stack.last().unwrap(),
            Int253::from(9u64),
        );
    }

    #[test]
    fn switch_picks_b_when_x_zero() {
        // push:0, pushstr [push:9, drop], pushstr [push:8, drop], switch
        let mut script = vec![0x00];
        script.extend_from_slice(&pushstr_bytes(&[0x09, 0x1c]));
        script.extend_from_slice(&pushstr_bytes(&[0x08, 0x1c]));
        script.push(0x7d);
        let mut reg = StubRegistry { script };
        let block = BlockContext { height: 0 };
        // x=0 → runs branch b (push:8, drop) → empty stack at end → ok.
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
    }

    // ── return (0x7e) ────────────────────────────────────────────

    #[test]
    fn return_zero_at_root_errors() {
        // push:0, return — root frame has no caller, so `return` errors
        // even with k=0. Scripts that want a clean early exit use `break:0`.
        let mut vm = vm_with_script(vec![0x00, 0x7e]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::ReturnAtRoot
        ));
    }

    #[test]
    fn return_nonzero_at_root_errors() {
        // push:7, push:1, return — k=1 at root: nowhere for 7 to go.
        let mut vm = vm_with_script(vec![0x07, 0x01, 0x7e]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::ReturnAtRoot
        ));
    }

    #[test]
    fn break_zero_at_root_with_clean_stack_exits_cleanly() {
        // break:0 at root — preferred way to short-circuit cleanly.
        let mut vm = vm_with_script(vec![0x80]);
        run_until_tx_done(&mut vm).unwrap();
    }

    #[test]
    fn break_zero_at_root_with_leftover_stack_errors() {
        // push:5, break:0 — break works, but finish_call catches the leftover.
        let mut vm = vm_with_script(vec![0x05, 0x80]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::StackNotClean
        ));
    }

    /// Helper: builds a VM with a child CellOpen frame as `current_call`
    /// and a placeholder ExternalRoot on `call_stack`. Used by the arity
    /// / clean-stack `return` tests which need a non-root frame to
    /// exercise the inner checks (root frame would short-circuit with
    /// `ReturnAtRoot`).
    fn vm_with_nested_child_script(script: Vec<u8>) -> VM {
        let parent = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500, 0, 0);
        let child_kind = CallKind::CellOpen {
            anchor: Anchor([0u8; 32]),
            predicate: Predicate::Opaque(CompressedRistretto([0u8; 32])),
        };
        let child = CallFrame::new(script, child_kind, 500, 0, 0);
        let mut vm = VM::new(dummy_header(), parent);
        let p = mem::replace(&mut vm.current_call, child);
        vm.call_stack.push(p);
        vm
    }

    #[test]
    fn return_with_dirty_leftover_errors() {
        // Inside a child frame: push:9, push:7, push:1, return — k=1, two
        // items below count → StackNotClean.
        let mut vm = vm_with_nested_child_script(vec![0x09, 0x07, 0x01, 0x7e]);
        let err = loop {
            match vm.step_internal() {
                Ok(true) => continue,
                Ok(false) => panic!("expected error"),
                Err(e) => break e,
            }
        };
        assert!(matches!(err, VMError::StackNotClean));
    }

    #[test]
    fn return_too_few_items_errors() {
        // Inside a child frame: push:5, return — k=5 popped, zero items
        // remain → BadReturnArity.
        let mut vm = vm_with_nested_child_script(vec![0x05, 0x7e]);
        let err = loop {
            match vm.step_internal() {
                Ok(true) => continue,
                Ok(false) => panic!("expected error"),
                Err(e) => break e,
            }
        };
        assert!(matches!(err, VMError::BadReturnArity));
    }

    #[test]
    fn return_transfers_values_to_parent() {
        // Set up a nested call manually (proper `call` lands in Phase 15).
        // Child script: push:7, push:1, return (k=1).
        let child_script = vec![0x07, 0x01, 0x7e];
        let parent_frame =
            CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500, 0, 0);
        let child_kind = CallKind::CellOpen {
            anchor: Anchor([0u8; 32]),
            predicate: Predicate::Opaque(CompressedRistretto([0u8; 32])),
        };
        let child_frame = CallFrame::new(child_script, child_kind, 500, 0, 0);
        let mut vm = VM::new(dummy_header(), parent_frame);
        let initial_parent = mem::replace(&mut vm.current_call, child_frame);
        vm.call_stack.push(initial_parent);

        // Step until the call_stack collapses back to the parent. Stops
        // before the root finish_call check kicks in (it would error on
        // the leftover 7 because there's no further script to clean it).
        while !vm.call_stack.is_empty() {
            vm.step_internal().unwrap();
        }

        // Parent received the 7.
        assert_eq!(vm.current_call.stack.len(), 1);
        assert_int(&vm.current_call.stack[0], Int253::from(7u64));
    }

    // ── break:k (0x80..=0x8f) ────────────────────────────────────

    #[test]
    fn break_zero_ends_current_run_only() {
        // Outer: pushstr [break:0, pushint8 99], run
        // — break:0 stops the subprog before pushint8 runs; outer resumes
        //   with empty stack and the tx exits clean.
        let mut script = pushstr_bytes(&[0x80, 0x10, 99]);
        script.push(0x7b);
        let mut reg = StubRegistry { script };
        let block = BlockContext { height: 0 };
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block)
            .unwrap();
    }

    #[test]
    fn break_one_ends_subprog_and_outer() {
        // Outer: pushstr [break:1], run, push:99
        //  — subprog issues break:1, which also discards the outer's
        //    resumed run, so push:99 never executes. Outer call exits
        //    with empty stack.
        // (push:99 is encoded as pushint8 + byte, but break:1 makes it
        // unreachable, so we don't even need to keep stack clean for it.)
        let mut script = pushstr_bytes(&[0x81]); // [break:1]
        script.push(0x7b); // run
        script.push(0x10); // pushint8
        script.push(99);
        let mut reg = StubRegistry { script };
        let block = BlockContext { height: 0 };
        VM::execute_internal(dummy_header(), dummy_message(1000), &mut reg, &block).unwrap();
    }

    #[test]
    fn break_out_of_call_errors() {
        // Top-level break:1 — but run_stack is empty, so this tries to
        // break past the call boundary.
        let mut vm = vm_with_script(vec![0x81]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::BreakOutOfCall
        ));
    }

    #[test]
    fn break_zero_at_root_ends_cleanly() {
        // break:0 at root: ends current run (which IS the root run),
        // run_stack empty → finish_call with empty stack → clean exit.
        let mut vm = vm_with_script(vec![0x80]);
        run_until_tx_done(&mut vm).unwrap();
    }

    // ── type (0x7f) ──────────────────────────────────────────────

    #[test]
    fn type_pushes_int253_code() {
        // push:5, type, drop, drop — top is type code (0 for Int253), then 5.
        let mut vm = vm_with_script(vec![0x05, 0x7f]);
        vm.step_internal().unwrap(); // push:5
        vm.step_internal().unwrap(); // type
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
        assert_int(&vm.current_call.stack[0], Int253::from(5u64));
    }

    #[test]
    fn type_pushes_string_code() {
        let mut script = pushstr_bytes(&[]); // empty string
        script.push(0x7f); // type
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(68u64));
    }

    #[test]
    fn type_underflow_errors() {
        let mut vm = vm_with_script(vec![0x7f]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::StackUnderflow
        ));
    }

    // ── Phase 3 ──────────────────────────────────────────────────

    // ── abs (0x50) ───────────────────────────────────────────────

    #[test]
    fn abs_of_negative_pushes_magnitude_and_sign() {
        // pushint8(neg, 9), abs
        let mut vm = vm_with_script(vec![0x11, 9, 0x50]);
        run_to_end(&mut vm).unwrap();
        // Stack: [magnitude=9, sign=1] (sign on top)
        assert_int(&vm.current_call.stack[0], Int253::from(9u64));
        assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    }

    #[test]
    fn abs_of_positive_pushes_sign_zero() {
        let mut vm = vm_with_script(vec![0x10, 9, 0x50]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(9u64));
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn abs_of_zero_is_sign_zero() {
        let mut vm = vm_with_script(vec![0x00, 0x50]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── eq (0x51) ────────────────────────────────────────────────

    #[test]
    fn eq_pushes_one_for_equal_ints() {
        let mut vm = vm_with_script(vec![0x07, 0x07, 0x51]);
        run_to_end(&mut vm).unwrap();
        // Stack: [7, 7, 1]
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn eq_pushes_zero_for_distinct_ints() {
        let mut vm = vm_with_script(vec![0x07, 0x08, 0x51]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[2], Int253::from(0u64));
    }

    #[test]
    fn eq_cross_type_is_zero() {
        // pushpoint, push:0, eq — different variants → 0
        let mut script = vec![0x1a];
        script.extend_from_slice(&[0u8; 32]);
        script.push(0x00); // push:0
        script.push(0x51);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(vm.current_call.stack.last().unwrap(), Int253::from(0u64));
    }

    #[test]
    fn eq_underflow_errors() {
        let mut vm = vm_with_script(vec![0x05, 0x51]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::StackUnderflow
        ));
    }

    #[test]
    fn eq_noncomparable_linear_type_errors() {
        // Two ClearTokens of same flavor — same variant, but linear.
        // push:7, pushtoken, push:7, pushtoken, eq
        let mut vm = vm_with_script(vec![0x07, 0x1b, 0x07, 0x1b, 0x51]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotComparable
        ));
    }

    #[test]
    fn eq_two_dicts_is_not_comparable() {
        // push:0, dict, push:0, dict, eq — two empty dicts; eq must err.
        let mut vm = vm_with_script(vec![0x00, 0x60, 0x00, 0x60, 0x51]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotComparable
        ));
    }

    // ── neg (0x52) ───────────────────────────────────────────────

    #[test]
    fn neg_flips_sign() {
        let mut vm = vm_with_script(vec![0x05, 0x52]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(-5i64));
    }

    #[test]
    fn neg_of_zero_stays_positive() {
        let mut vm = vm_with_script(vec![0x00, 0x52]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    }

    // ── add (0x53), mul (0x54) ───────────────────────────────────

    #[test]
    fn add_basic() {
        // push:7, push:3, add  → 10
        let mut vm = vm_with_script(vec![0x07, 0x03, 0x53]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(10u64));
    }

    #[test]
    fn add_with_negative() {
        // pushint8(neg, 7), push:3, add  → -4
        let mut vm = vm_with_script(vec![0x11, 7, 0x03, 0x53]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(-4i64));
    }

    #[test]
    fn mul_basic() {
        let mut vm = vm_with_script(vec![0x07, 0x03, 0x54]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(21u64));
    }

    #[test]
    fn mul_sign_xor() {
        // pushint8(neg, 6), push:7, mul → -42
        let mut vm = vm_with_script(vec![0x11, 6, 0x07, 0x54]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(-42i64));
    }

    #[test]
    fn add_requires_int_operands() {
        // pushpoint, push:1, add — left operand not Int253.
        let mut script = vec![0x1a];
        script.extend_from_slice(&[0u8; 32]);
        script.push(0x01);
        script.push(0x53);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotInt253
        ));
    }

    // ── divmod (0x55) ────────────────────────────────────────────

    #[test]
    fn divmod_basic() {
        // push:13, push:5, divmod → d=2, r=3
        let mut vm = vm_with_script(vec![
            0x10, 13, // pushint8(pos, 13)
            0x10, 5, // pushint8(pos, 5)
            0x55,
        ]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(2u64));
        assert_int(&vm.current_call.stack[1], Int253::from(3u64));
    }

    #[test]
    fn divmod_negative_dividend() {
        // -13 / 5 → d=-2, r=-3
        let mut vm = vm_with_script(vec![0x11, 13, 0x10, 5, 0x55]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(-2i64));
        assert_int(&vm.current_call.stack[1], Int253::from(-3i64));
    }

    #[test]
    fn divmod_by_zero_errors() {
        let mut vm = vm_with_script(vec![0x07, 0x00, 0x55]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::DivByZero
        ));
    }

    #[test]
    fn divmod_full_width_magnitude_succeeds() {
        // 2^128 / 1 → d = 2^128, r = 0. Verifies the opcode now handles
        // magnitudes beyond u64::MAX (was MagnitudeTooLarge in Phase 3).
        let mut huge = [0u8; 32];
        huge[16] = 1; // 2^128
        let mut script = vec![0x18];
        script.extend_from_slice(&huge);
        script.push(0x01); // push:1
        script.push(0x55);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from_bytes(huge).unwrap());
        assert_int(&vm.current_call.stack[1], Int253::zero());
    }

    // ── mod252 (0x56) ────────────────────────────────────────────

    #[test]
    fn mod252_empty_string_is_zero() {
        let script = vec![0x19, 0x00, 0x00, 0x56]; // pushstr "", mod252
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    }

    #[test]
    fn mod252_short_string_is_le_value() {
        // pushstr [0x07, 0x00, 0x01], mod252
        // LE interpretation = 7 + 0*256 + 1*65536 = 65543
        let script = vec![0x19, 0x00, 0x03, 0x07, 0x00, 0x01, 0x56];
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(65543u64));
    }

    #[test]
    fn mod252_64_bytes_reduces() {
        // 64 bytes of 0xff — should equal 2^512 - 1 reduced mod ℓ.
        // sub-varint tag 0, byte 64, then 64 × 0xff, then mod252.
        let script = {
            let mut s = vec![0x19, 0x00, 64];
            s.extend_from_slice(&[0xffu8; 64]);
            s.push(0x56);
            s
        };
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        // Compare against the dalek reference path.
        let expected = Int253::from(Scalar::from_bytes_mod_order_wide(&[0xff; 64]));
        assert_int(&vm.current_call.stack[0], expected);
    }

    #[test]
    fn mod252_too_long_errors() {
        // 65-byte string
        let mut s = vec![0x19, 0x00, 65];
        s.extend_from_slice(&[0u8; 65]);
        s.push(0x56);
        let mut vm = vm_with_script(s);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::StringTooLongForModReduction
        ));
    }

    // ── not / and / or (0x57..=0x59) ─────────────────────────────

    #[test]
    fn not_zero_to_one() {
        let mut vm = vm_with_script(vec![0x00, 0x57]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(1u64));
    }

    #[test]
    fn not_nonzero_to_zero() {
        let mut vm = vm_with_script(vec![0x05, 0x57]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    }

    #[test]
    fn and_truth_table() {
        // (1, 1) → 1
        let mut vm = vm_with_script(vec![0x01, 0x01, 0x58]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(1u64));
        // (1, 0) → 0
        let mut vm = vm_with_script(vec![0x01, 0x00, 0x58]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
        // (0, 1) → 0
        let mut vm = vm_with_script(vec![0x00, 0x01, 0x58]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
        // (0, 0) → 0
        let mut vm = vm_with_script(vec![0x00, 0x00, 0x58]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
    }

    #[test]
    fn or_truth_table() {
        // (0, 0) → 0
        let mut vm = vm_with_script(vec![0x00, 0x00, 0x59]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(0u64));
        // (1, 0) → 1
        let mut vm = vm_with_script(vec![0x01, 0x00, 0x59]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(1u64));
        // (0, 1) → 1
        let mut vm = vm_with_script(vec![0x00, 0x01, 0x59]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(1u64));
    }

    // ── size (0x5f) ──────────────────────────────────────────────

    #[test]
    fn size_of_string() {
        // pushstr [a, b, c], size
        let mut script = pushstr_bytes(&[b'a', b'b', b'c']);
        script.push(0x5f);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        // Stack: [String("abc"), 3]
        assert_int(&vm.current_call.stack[1], Int253::from(3u64));
    }

    #[test]
    fn size_of_int_errors() {
        let mut vm = vm_with_script(vec![0x05, 0x5f]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeHasNoLength
        ));
    }

    #[test]
    fn size_underflow_errors() {
        let mut vm = vm_with_script(vec![0x5f]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::StackUnderflow
        ));
    }

    // ── Phase 4 ──────────────────────────────────────────────────

    fn assert_str(v: &Value, expected: &[u8]) {
        match v {
            Value::String(s) => assert_eq!(s.as_bytes(), expected),
            other => panic!("expected String, got {}", value_kind(other)),
        }
    }

    // ── readbits (0x40) ──────────────────────────────────────────

    /// Helper: pushes integer `n` (`0 ≤ n ≤ 65535`) onto the script
    /// using the shortest available immediate.
    fn push_small_uint(script: &mut Vec<u8>, n: u32) {
        if n <= 15 {
            script.push(n as u8);
        } else if n <= 255 {
            script.push(0x10);
            script.push(n as u8);
        } else {
            assert!(n <= 65_535);
            script.push(0x12);
            script.extend_from_slice(&(n as u16).to_le_bytes());
        }
    }

    /// Helper: encodes `value` (non-negative `Int253`) as a low-`n_bits`
    /// LSB-first byte sequence (writebits-compatible).
    fn writebits_bytes(value: &Int253, n_bits: usize) -> Vec<u8> {
        assert!(n_bits <= 256);
        let int_bytes = value.to_bytes();
        let n_bytes = (n_bits + 7) / 8;
        let mut out = int_bytes[..n_bytes].to_vec();
        let tail = n_bits % 8;
        if tail != 0 && n_bytes > 0 {
            let mask = (1u8 << tail) - 1;
            out[n_bytes - 1] &= mask;
        }
        out
    }

    #[test]
    fn read_bits_n_zero_succeeds_and_yields_zero() {
        let mut script = pushstr_bytes(&[0xaa, 0xbb]);
        push_small_uint(&mut script, 0);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 3);
        // string is unchanged (no bytes consumed)
        assert_str(&vm.current_call.stack[0], &[0xaa, 0xbb]);
        assert_int(&vm.current_call.stack[1], Int253::zero());
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn read_bits_partial_byte_masks_high_bits() {
        // Source byte: 0b1111_1111 = 0xff. Read 5 bits LSB-first → low 5 bits = 0b11111 = 31.
        let mut script = pushstr_bytes(&[0xff, 0x00]);
        push_small_uint(&mut script, 5);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        // One byte was consumed even though only 5 bits were "used".
        assert_str(&vm.current_call.stack[0], &[0x00]);
        assert_int(&vm.current_call.stack[1], Int253::from(31u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn read_bits_too_short_preserves_string() {
        // n=16 requires 2 bytes; only 1 available.
        let mut script = pushstr_bytes(&[0xaa]);
        push_small_uint(&mut script, 16);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_str(&vm.current_call.stack[0], &[0xaa]);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn read_bits_n_257_hard_fails() {
        let mut script = pushstr_bytes(&[0u8; 33]);
        push_small_uint(&mut script, 257);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }

    #[test]
    fn read_bits_magnitude_at_ell_soft_fails() {
        // Canonical scalar magnitude exactly ℓ (the order) is *not*
        // canonical — `from_canonical_bytes` rejects. With n=256 and
        // sign bit = 0, bytes are the encoding of ℓ.
        let ell_le: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
            0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ];
        let mut script = pushstr_bytes(&ell_le);
        push_small_uint(&mut script, 256);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        // Soft-fail: original 32-byte string restored, marker = 0.
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_str(&vm.current_call.stack[0], &ell_le);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn read_bits_magnitude_above_ell_soft_fails() {
        // ℓ + 1: still non-canonical, must soft-fail.
        let mut bytes: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
            0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ];
        bytes[0] = bytes[0].wrapping_add(1);
        let mut script = pushstr_bytes(&bytes);
        push_small_uint(&mut script, 256);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &bytes);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn read_bits_negative_zero_soft_fails() {
        // n=256, magnitude = 0, sign bit = 1 → negative zero. Reject.
        let mut bytes = [0u8; 32];
        bytes[31] = 0x80;
        let mut script = pushstr_bytes(&bytes);
        push_small_uint(&mut script, 256);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &bytes);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn read_bits_roundtrip_nonneg_at_various_n() {
        // n=1,8,64,252,253,255,256. Value chosen distinct for each.
        let cases: &[(usize, u64)] = &[
            (1, 1),
            (8, 0xab),
            (64, 0x0123_4567_89ab_cdef),
            (252, 0xdead_beef_cafe_babe),
            (253, 0xfeed_face_0123_4567),
            (255, 0x5555_5555_5555_5555),
            (256, 0x7fff_ffff_ffff_ffff), // bit 255 = 0 → positive
        ];
        for (n, v) in cases.iter().copied() {
            let value = Int253::from(v);
            let bytes = writebits_bytes(&value, n);
            // build script: pushstr(bytes), push n, readbits
            let mut script = pushstr_bytes(&bytes);
            push_small_uint(&mut script, n as u32);
            script.push(0x40);
            let mut vm = vm_with_script(script);
            run_to_end(&mut vm).unwrap_or_else(|e| panic!("n={} v={} err={:?}", n, v, e));
            assert_int(&vm.current_call.stack[1], value);
            assert_int(&vm.current_call.stack[2], Int253::from(1u64));
            assert_str(&vm.current_call.stack[0], &[]);
        }
    }

    #[test]
    fn read_bits_roundtrip_negative_at_n_256() {
        // Only n=256 carries the sign bit. Pick a small negative value.
        let value = Int253::from_parts(true, Scalar::from(12345u64));
        let bytes = value.to_bytes();
        let mut script = pushstr_bytes(&bytes);
        push_small_uint(&mut script, 256);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], value);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    // ── readint (0x41) ───────────────────────────────────────────

    #[test]
    fn read_int_positive_roundtrip() {
        let value = Int253::from(1u64);
        let mut script = pushstr_bytes(&value.to_bytes());
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], value);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn read_int_negative_roundtrip() {
        let value = Int253::from_parts(true, Scalar::from(1u64));
        let mut script = pushstr_bytes(&value.to_bytes());
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], value);
    }

    #[test]
    fn read_int_zero_roundtrip() {
        let value = Int253::zero();
        let mut script = pushstr_bytes(&value.to_bytes());
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], value);
    }

    #[test]
    fn read_int_large_magnitude_roundtrip() {
        // ℓ - 1 (max canonical magnitude), positive.
        let ell_minus_1: [u8; 32] = [
            0xec, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
            0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ];
        let mut script = pushstr_bytes(&ell_minus_1);
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[1] {
            Value::Int253(i) => assert_eq!(i.to_bytes(), ell_minus_1),
            other => panic!("expected Int253, got {}", value_kind(other)),
        }
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn read_int_too_short_preserves_string() {
        let mut script = pushstr_bytes(&[0xaa; 31]);
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_str(&vm.current_call.stack[0], &[0xaa; 31]);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn read_int_negative_zero_soft_fails() {
        let mut bytes = [0u8; 32];
        bytes[31] = 0x80;
        let mut script = pushstr_bytes(&bytes);
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &bytes);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── readstr (0x42) ───────────────────────────────────────────

    #[test]
    fn read_str_success() {
        let mut script = pushstr_bytes(&[1, 2, 3, 4, 5]);
        script.push(0x02);
        script.push(0x42);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[3, 4, 5]);
        assert_str(&vm.current_call.stack[1], &[1, 2]);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn read_str_too_short_preserves() {
        let mut script = pushstr_bytes(&[1]);
        script.push(0x05);
        script.push(0x42);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[1]);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── readpoint (0x43) ─────────────────────────────────────────

    #[test]
    fn read_point_success() {
        let mut bytes = vec![0x55u8; 32];
        bytes.push(0xaa);
        let mut script = pushstr_bytes(&bytes);
        script.push(0x43);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xaa]);
        match &vm.current_call.stack[1] {
            Value::Point(p) => assert_eq!(p.as_bytes(), &[0x55u8; 32]),
            other => panic!("expected Point, got {}", value_kind(other)),
        }
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn read_point_too_short() {
        let mut script = pushstr_bytes(&[0; 31]);
        script.push(0x43);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── writebits (0x44) ─────────────────────────────────────────

    #[test]
    fn write_bits_full_byte() {
        let mut script = pushstr_bytes(&[0xaa]);
        script.push(0x10);
        script.push(0xab);
        push_small_uint(&mut script, 8);
        script.push(0x44);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xaa, 0xab]);
    }

    #[test]
    fn write_bits_non_aligned_hard_fails() {
        // n=5 is not a multiple of 8 → hard-fail with BitCountOutOfRange.
        let mut script = pushstr_bytes(&[]);
        script.push(0x10);
        script.push(0xff);
        push_small_uint(&mut script, 5);
        script.push(0x44);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::BitCountOutOfRange
        ));
    }

    #[test]
    fn write_bits_n_zero_is_noop() {
        let mut script = pushstr_bytes(&[0xaa]);
        script.push(0x10);
        script.push(0x07);
        push_small_uint(&mut script, 0);
        script.push(0x44);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xaa]);
    }

    #[test]
    fn write_bits_n_257_hard_fails() {
        let mut script = pushstr_bytes(&[]);
        script.push(0x10);
        script.push(0x07);
        push_small_uint(&mut script, 257);
        script.push(0x44);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }

    #[test]
    fn write_then_read_bits_roundtrip_nonneg() {
        let cases: &[(usize, u64)] = &[
            (1, 1),
            (8, 0xab),
            (64, 0x0123_4567_89ab_cdef),
            (252, 0xdead_beef_cafe_babe),
            (253, 0xfeed_face_0123_4567),
            (255, 0x5555_5555_5555_5555),
            (256, 0x7fff_ffff_ffff_ffff),
        ];
        for (n, v) in cases.iter().copied() {
            // writebits requires n to be a multiple of 8; skip the others
            // for this roundtrip (sub-byte n is exercised in the readbits
            // round-trip tests which do not write via the opcode).
            if n % 8 != 0 {
                continue;
            }
            let value = Int253::from(v);
            let mut script = pushstr_bytes(&[]);
            // push v (≤ u64::MAX), LE per opcode spec.
            script.push(0x14);
            script.extend_from_slice(&v.to_le_bytes());
            push_small_uint(&mut script, n as u32);
            script.push(0x44); // writebits → stack: [s']
            push_small_uint(&mut script, n as u32);
            script.push(0x40); // readbits → stack: [s'' x 1]
            let mut vm = vm_with_script(script);
            run_to_end(&mut vm).unwrap_or_else(|e| panic!("n={} v={} err={:?}", n, v, e));
            assert_str(&vm.current_call.stack[0], &[]);
            assert_int(&vm.current_call.stack[1], value);
            assert_int(&vm.current_call.stack[2], Int253::from(1u64));
        }
    }

    #[test]
    fn write_then_read_bits_roundtrip_negative_n_256() {
        // For n=256, sign bit at position 255 is preserved.
        let value = Int253::from_parts(true, Scalar::from(12345u64));
        let bytes = value.to_bytes();
        // Build manually to avoid relying on a "push negative int" path.
        let mut script = pushstr_bytes(&[]);
        // pushstr the int's encoding, then append to the empty string.
        script.extend_from_slice(&pushstr_bytes(&bytes));
        script.push(0x46); // append (s s' → s'')
        push_small_uint(&mut script, 256);
        script.push(0x40); // readbits
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], value);
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    // ── writeint (0x45) ──────────────────────────────────────────

    #[test]
    fn write_int_appends_full_32_bytes() {
        let mut script = pushstr_bytes(&[]);
        script.push(0x10);
        script.push(0x07);
        script.push(0x45);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = Int253::from(7u64).to_bytes();
        assert_str(&vm.current_call.stack[0], &expected);
    }

    #[test]
    fn write_then_read_int_roundtrip_signs_and_extremes() {
        // ±1, ±(ℓ-1), zero, and a moderately large positive magnitude.
        let ell_minus_1: [u8; 32] = [
            0xec, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
            0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ];
        let mut neg_ell_minus_1 = ell_minus_1;
        neg_ell_minus_1[31] |= 0x80;
        let values: Vec<Int253> = vec![
            Int253::from(1u64),
            Int253::from_parts(true, Scalar::from(1u64)),
            Int253::from_bytes(ell_minus_1).unwrap(),
            Int253::from_bytes(neg_ell_minus_1).unwrap(),
            Int253::zero(),
            Int253::from(0xdead_beef_cafe_babe_u64),
        ];
        for v in values {
            let bytes = v.to_bytes();
            // pushstr(empty), pushstr(bytes), append, readint
            let mut script = pushstr_bytes(&[]);
            script.extend_from_slice(&pushstr_bytes(&bytes));
            script.push(0x46);
            script.push(0x41);
            let mut vm = vm_with_script(script);
            run_to_end(&mut vm)
                .unwrap_or_else(|e| panic!("value={:?} err={:?}", v, e));
            match &vm.current_call.stack[1] {
                Value::Int253(i) => assert_eq!(
                    i.to_bytes(),
                    v.to_bytes(),
                    "roundtrip differed for {:?}",
                    v
                ),
                other => panic!("expected Int253, got {}", value_kind(other)),
            }
            assert_int(&vm.current_call.stack[2], Int253::from(1u64));
        }
    }

    // ── append (0x46) ────────────────────────────────────────────

    #[test]
    fn append_concatenates() {
        let mut script = pushstr_bytes(&[1, 2]);
        script.extend_from_slice(&pushstr_bytes(&[3, 4, 5]));
        script.push(0x46);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[1, 2, 3, 4, 5]);
    }

    // ── writezeros (0x47) ────────────────────────────────────────

    #[test]
    fn write_zeros_appends_n_zero_bytes() {
        let mut script = pushstr_bytes(&[0xaa]);
        script.push(0x03);
        script.push(0x47);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xaa, 0, 0, 0]);
    }

    // ── bit ops (0x48..=0x4b) ────────────────────────────────────

    #[test]
    fn bit_not_inverts() {
        let mut script = pushstr_bytes(&[0x00, 0xff, 0xa5]);
        script.push(0x48);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xff, 0x00, 0x5a]);
    }

    #[test]
    fn bit_or_basic() {
        let mut script = pushstr_bytes(&[0xa0, 0x0f]);
        script.extend_from_slice(&pushstr_bytes(&[0x05, 0xf0]));
        script.push(0x49);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xa5, 0xff]);
    }

    #[test]
    fn bit_and_basic() {
        let mut script = pushstr_bytes(&[0xff, 0xf0]);
        script.extend_from_slice(&pushstr_bytes(&[0xa5, 0xa5]));
        script.push(0x4a);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xa5, 0xa0]);
    }

    #[test]
    fn bit_xor_basic() {
        let mut script = pushstr_bytes(&[0xff, 0x00]);
        script.extend_from_slice(&pushstr_bytes(&[0xa5, 0xa5]));
        script.push(0x4b);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0x5a, 0xa5]);
    }

    #[test]
    fn bit_or_size_mismatch_errors() {
        let mut script = pushstr_bytes(&[0xa0]);
        script.extend_from_slice(&pushstr_bytes(&[0x05, 0xf0]));
        script.push(0x49);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::BitwiseSizeMismatch
        ));
    }

    // ── shiftleft (0x4c) ─────────────────────────────────────────

    #[test]
    fn shift_left_by_byte() {
        let mut script = pushstr_bytes(&[0xa0, 0xb1, 0xc2, 0xd3]);
        script.push(0x10);
        script.push(8);
        script.push(0x4c);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xb1, 0xc2, 0xd3, 0x00]);
        assert_str(&vm.current_call.stack[1], &[0xa0]);
    }

    #[test]
    fn shift_left_by_4_bits_left_pads_removed() {
        let mut script = pushstr_bytes(&[0xab]);
        script.push(0x04);
        script.push(0x4c);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xb0]);
        assert_str(&vm.current_call.stack[1], &[0x0a]);
    }

    #[test]
    fn shift_left_zero_is_noop() {
        let mut script = pushstr_bytes(&[0xab, 0xcd]);
        script.push(0x00);
        script.push(0x4c);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xab, 0xcd]);
        assert_str(&vm.current_call.stack[1], &[]);
    }

    // ── shiftright (0x4d) ────────────────────────────────────────

    #[test]
    fn shift_right_by_byte() {
        let mut script = pushstr_bytes(&[0xa0, 0xb1, 0xc2, 0xd3]);
        script.push(0x10);
        script.push(8);
        script.push(0x4d);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0x00, 0xa0, 0xb1, 0xc2]);
        assert_str(&vm.current_call.stack[1], &[0xd3]);
    }

    #[test]
    fn shift_right_by_4_bits_right_pads_removed() {
        let mut script = pushstr_bytes(&[0xab]);
        script.push(0x04);
        script.push(0x4d);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0x0a]);
        assert_str(&vm.current_call.stack[1], &[0xb0]);
    }

    #[test]
    fn shift_too_large_errors() {
        let mut script = pushstr_bytes(&[0xab]);
        script.push(0x12); // pushint16 positive
        script.extend_from_slice(&257u16.to_le_bytes()); // 257 > 256
        script.push(0x4c);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }

    // ── Phase 5 ──────────────────────────────────────────────────

    fn assert_dict_keys(v: &Value, expected: &[Int253]) {
        match v {
            Value::Dict(d) => {
                let keys: Vec<Int253> = d.entries().map(|(k, _)| *k).collect();
                assert_eq!(keys, expected);
            }
            other => panic!("expected Dict, got {}", value_kind(other)),
        }
    }

    // ── dict (0x60) ──────────────────────────────────────────────

    #[test]
    fn dict_construction_zero_pairs() {
        // push:0, dict — empty dict
        let mut vm = vm_with_script(vec![0x00, 0x60]);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::Dict(d) => assert!(d.is_empty()),
            other => panic!("expected Dict, got {}", value_kind(other)),
        }
    }

    #[test]
    fn dict_construction_two_pairs() {
        // Stack order: val key val key n
        // Build {5: 50, 1: 10}: push 50, push 5, push 10, push 1, push 2, dict
        // (Pairs popped top-first: pair1 = (1, 10), pair2 = (5, 50).)
        // After construction, keys sorted: [1, 5].
        let mut vm = vm_with_script(vec![
            0x10, 50, // val for first pair (will end up at key 5)
            0x05,     // key 5
            0x10, 10, // val for second pair (key 1)
            0x01,     // key 1
            0x02,     // push:2
            0x60,     // dict
        ]);
        run_to_end(&mut vm).unwrap();
        assert_dict_keys(
            &vm.current_call.stack[0],
            &[Int253::from(1u64), Int253::from(5u64)],
        );
    }

    #[test]
    fn dict_construction_duplicate_keys_errors() {
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, // (5, 50)
            0x10, 60, 0x05, // (5, 60)  duplicate!
            0x02, 0x60,
        ]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::DictKeyOccupied
        ));
    }

    // ── put (0x61) ───────────────────────────────────────────────

    #[test]
    fn put_inserts_into_empty() {
        // push:0, dict (empty)  →  put k=3, v=99
        let mut vm = vm_with_script(vec![
            0x00, 0x60,       // empty dict
            0x03,             // key 3
            0x10, 99,         // value 99
            0x61,
        ]);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::Dict(d) => {
                assert_eq!(d.len(), 1);
                match d.get(&Int253::from(3u64)) {
                    Some(Value::Int253(i)) => assert_eq!(*i, Int253::from(99u64)),
                    _ => panic!("expected Int253"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn put_on_occupied_key_errors() {
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, // (5, 50)
            0x01, 0x60,     // dict (1 pair)
            0x05,           // key 5
            0x10, 99,
            0x61,           // put → conflict
        ]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::DictKeyOccupied
        ));
    }

    // ── replace (0x62) ───────────────────────────────────────────

    #[test]
    fn replace_existing_returns_prev() {
        // Build {5: 50}, then replace v at key 5 with 99.
        // Spec stack: dict k v → dict' {prev 1 | 0}
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, // (5, 50)
            0x01, 0x60,     // dict
            0x05,           // k
            0x10, 99,       // v
            0x62,           // replace
        ]);
        run_to_end(&mut vm).unwrap();
        // Stack: [dict', 50, 1]
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[1], Int253::from(50u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn replace_absent_returns_zero() {
        let mut vm = vm_with_script(vec![
            0x00, 0x60, // empty dict
            0x05,       // k
            0x10, 99,   // v
            0x62,
        ]);
        run_to_end(&mut vm).unwrap();
        // Stack: [dict', 0]
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── get (0x63) ───────────────────────────────────────────────

    #[test]
    fn get_existing_returns_dict_k_v() {
        // {5: 50}, get key 5.
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x01, 0x60, // dict
            0x05,                       // k
            0x63,
        ]);
        run_to_end(&mut vm).unwrap();
        // Stack: [dict', k=5, v=50]
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[1], Int253::from(5u64));
        assert_int(&vm.current_call.stack[2], Int253::from(50u64));
        // Dict should now be empty.
        match &vm.current_call.stack[0] {
            Value::Dict(d) => assert!(d.is_empty()),
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn get_missing_errors() {
        let mut vm = vm_with_script(vec![0x00, 0x60, 0x05, 0x63]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::DictKeyNotFound
        ));
    }

    // ── getopt (0x64) ────────────────────────────────────────────

    #[test]
    fn getopt_existing() {
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x01, 0x60, // dict {5: 50}
            0x05,                       // k
            0x64,
        ]);
        run_to_end(&mut vm).unwrap();
        // Stack: [dict', 50, 1]
        assert_int(&vm.current_call.stack[1], Int253::from(50u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn getopt_missing() {
        let mut vm = vm_with_script(vec![0x00, 0x60, 0x05, 0x64]);
        run_to_end(&mut vm).unwrap();
        // Stack: [dict', 0]
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── getdup (0x65) ────────────────────────────────────────────

    #[test]
    fn getdup_copyable() {
        // {5: 50}; getdup k=5 → dict unchanged + 50 + 1
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x01, 0x60, 0x05, 0x65,
        ]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[1], Int253::from(50u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
        // Dict still has the entry.
        match &vm.current_call.stack[0] {
            Value::Dict(d) => assert_eq!(d.len(), 1),
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn getdup_missing_pushes_zero() {
        let mut vm = vm_with_script(vec![0x00, 0x60, 0x05, 0x65]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn getdup_noncopyable_errors() {
        // {5: ClearToken(0, 7)}; getdup k=5 → TypeNotCopyable
        // push:7 (flavor), pushtoken (value), push:5 (key), push:1 (count),
        //   dict, push:5 (k), getdup.
        let mut vm = vm_with_script(vec![
            0x07, 0x1b, // value = ClearToken with flavor 7
            0x05,       // key 5
            0x01,       // count 1
            0x60,       // dict
            0x05,       // k
            0x65,       // getdup
        ]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotCopyable
        ));
    }

    // ── first/last/next (0x66-0x68) ──────────────────────────────

    #[test]
    fn first_of_empty_pushes_zero() {
        let mut vm = vm_with_script(vec![0x00, 0x60, 0x66]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn first_returns_smallest_key() {
        // Build dict {5: 50, 1: 10}.
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x10, 10, 0x01, 0x02, 0x60, // dict
            0x66,                                       // first
        ]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(1u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn last_returns_largest_key() {
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x10, 10, 0x01, 0x02, 0x60, // dict
            0x67,                                       // last
        ]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(5u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn next_finds_strictly_greater_key() {
        // {1: 10, 5: 50}; next of 1 → 5.
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x10, 10, 0x01, 0x02, 0x60, // dict
            0x01,                                       // k = 1
            0x68,
        ]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(5u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
    }

    #[test]
    fn next_past_last_pushes_zero() {
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x01, 0x60, // {5: 50}
            0x05,                       // k = 5
            0x68,
        ]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    // ── Flag propagation ─────────────────────────────────────────

    #[test]
    fn dict_with_token_is_noncopyable() {
        // Build {5: ClearToken(0, flavor=7)}; the dict should be marked
        // non-copyable.  push:7 (flavor), pushtoken, push:5, push:1, dict
        let mut vm = vm_with_script(vec![0x07, 0x1b, 0x05, 0x01, 0x60]);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::Dict(d) => {
                assert!(!d.is_copyable());
                assert!(d.is_portable()); // zero-qty ClearToken is portable
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn dup_of_copyable_dict_succeeds() {
        // {5: 50}, dup:0 — should copy the dict.
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x01, 0x60, // dict
            0x20,                       // dup:0
        ]);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        // Both stack entries should be dicts of length 1.
        for v in &vm.current_call.stack {
            match v {
                Value::Dict(d) => assert_eq!(d.len(), 1),
                _ => panic!("expected Dict"),
            }
        }
    }

    #[test]
    fn dup_of_noncopyable_dict_errors() {
        // push:7, pushtoken, push:5, push:1, dict, dup:0
        let mut vm = vm_with_script(vec![0x07, 0x1b, 0x05, 0x01, 0x60, 0x20]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotCopyable
        ));
    }

    #[test]
    fn empty_dict_is_droppable() {
        let mut vm = vm_with_script(vec![0x00, 0x60, 0x1c]); // empty dict, drop
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
    }

    #[test]
    fn nonempty_dict_is_not_droppable() {
        let mut vm = vm_with_script(vec![
            0x10, 50, 0x05, 0x01, 0x60, // {5: 50}
            0x1c,                       // drop
        ]);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotDroppable
        ));
    }

    // ── Phase 6 ──────────────────────────────────────────────────

    // ── merlin (0x69) ────────────────────────────────────────────

    #[test]
    fn merlin_creates_transcript() {
        // pushstr [], merlin → Merlin on top
        let mut script = pushstr_bytes(&[]);
        script.push(0x69);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::Merlin(_) => {}
            other => panic!("expected Merlin, got {}", value_kind(other)),
        }
    }

    #[test]
    fn merlin_is_noncopyable_and_nondroppable() {
        // pushstr [], merlin, dup:0 → TypeNotCopyable
        let mut script = pushstr_bytes(&[]);
        script.push(0x69);
        script.push(0x20);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotCopyable
        ));

        // pushstr [], merlin, drop → TypeNotDroppable
        let mut script = pushstr_bytes(&[]);
        script.push(0x69);
        script.push(0x1c);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotDroppable
        ));
    }

    // ── merlinwrite / merlinread (0x6a, 0x6b) ────────────────────

    #[test]
    fn merlin_write_then_read_produces_bytes() {
        // pushstr "init", merlin            -- transcript
        // pushstr "lbl", pushstr "data", merlinwrite  -- absorb data
        // pushstr "rd", push:8, merlinread  -- squeeze 8 bytes
        let mut script = pushstr_bytes(b"init");
        script.push(0x69);
        script.extend_from_slice(&pushstr_bytes(b"lbl"));
        script.extend_from_slice(&pushstr_bytes(b"data"));
        script.push(0x6a);
        script.extend_from_slice(&pushstr_bytes(b"rd"));
        script.push(0x08); // push:8
        script.push(0x6b);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        // Stack: [Merlin, String(8 bytes)]
        assert_eq!(vm.current_call.stack.len(), 2);
        match &vm.current_call.stack[0] {
            Value::Merlin(_) => {}
            other => panic!("expected Merlin, got {}", value_kind(other)),
        }
        match &vm.current_call.stack[1] {
            Value::String(s) => assert_eq!(s.len(), 8),
            other => panic!("expected String, got {}", value_kind(other)),
        }
    }

    #[test]
    fn merlin_read_is_deterministic() {
        // Two scripts that absorb the same input should produce the same
        // challenge bytes.
        fn build_script() -> Vec<u8> {
            let mut script = pushstr_bytes(b"label");
            script.push(0x69);
            script.extend_from_slice(&pushstr_bytes(b"k"));
            script.extend_from_slice(&pushstr_bytes(b"value"));
            script.push(0x6a);
            script.extend_from_slice(&pushstr_bytes(b"r"));
            script.push(0x10); // pushint8
            script.push(16);
            script.push(0x6b);
            script
        }
        let mut a = vm_with_script(build_script());
        run_to_end(&mut a).unwrap();
        let mut b = vm_with_script(build_script());
        run_to_end(&mut b).unwrap();
        let bytes_a = match &a.current_call.stack[1] {
            Value::String(s) => s.as_bytes().to_vec(),
            _ => panic!("expected String"),
        };
        let bytes_b = match &b.current_call.stack[1] {
            Value::String(s) => s.as_bytes().to_vec(),
            _ => panic!("expected String"),
        };
        assert_eq!(bytes_a, bytes_b);
    }

    #[test]
    fn merlin_read_diverges_on_different_label() {
        // Same data, different label → different challenge bytes.
        fn build(label: &[u8]) -> Vec<u8> {
            let mut script = pushstr_bytes(b"l");
            script.push(0x69);
            script.extend_from_slice(&pushstr_bytes(b"k"));
            script.extend_from_slice(&pushstr_bytes(b"data"));
            script.push(0x6a);
            script.extend_from_slice(&pushstr_bytes(label));
            script.push(0x10);
            script.push(16);
            script.push(0x6b);
            script
        }
        let mut a = vm_with_script(build(b"A"));
        let mut b = vm_with_script(build(b"B"));
        run_to_end(&mut a).unwrap();
        run_to_end(&mut b).unwrap();
        let ba = match &a.current_call.stack[1] {
            Value::String(s) => s.as_bytes().to_vec(),
            _ => panic!(),
        };
        let bb = match &b.current_call.stack[1] {
            Value::String(s) => s.as_bytes().to_vec(),
            _ => panic!(),
        };
        assert_ne!(ba, bb);
    }

    #[test]
    fn merlin_write_requires_merlin_on_bottom() {
        // push:5 (wrong type), pushstr "lbl", pushstr "data", merlinwrite
        let mut script = vec![0x05];
        script.extend_from_slice(&pushstr_bytes(b"lbl"));
        script.extend_from_slice(&pushstr_bytes(b"data"));
        script.push(0x6a);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotMerlin
        ));
    }

    // ── sha256 (0x6c) ────────────────────────────────────────────

    #[test]
    fn sha256_empty() {
        let mut script = pushstr_bytes(b"");
        script.push(0x6c);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    #[test]
    fn sha256_abc() {
        let mut script = pushstr_bytes(b"abc");
        script.push(0x6c);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    // ── sha512 (0x6d) ────────────────────────────────────────────

    #[test]
    fn sha512_empty() {
        let mut script = pushstr_bytes(b"");
        script.push(0x6d);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    // ── sha3 (0x6e) ──────────────────────────────────────────────

    #[test]
    fn sha3_empty() {
        let mut script = pushstr_bytes(b"");
        script.push(0x6e);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    #[test]
    fn sha3_abc() {
        let mut script = pushstr_bytes(b"abc");
        script.push(0x6e);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    // ── keccak256 (0x4e) ─────────────────────────────────────────

    #[test]
    fn keccak256_empty() {
        let mut script = pushstr_bytes(b"");
        script.push(0x4e);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    #[test]
    fn keccak256_abc() {
        let mut script = pushstr_bytes(b"abc");
        script.push(0x4e);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        let expected = hex_to_bytes(
            "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45",
        );
        assert_str(&vm.current_call.stack[0], &expected);
    }

    #[test]
    fn keccak256_differs_from_sha3() {
        // SHA3-256 and Keccak-256 of the same input must differ
        // (FIPS-202 added a domain separator).
        let mut script_a = pushstr_bytes(b"abc");
        script_a.push(0x6e); // sha3
        let mut script_b = pushstr_bytes(b"abc");
        script_b.push(0x4e); // keccak256
        let mut a = vm_with_script(script_a);
        run_to_end(&mut a).unwrap();
        let mut b = vm_with_script(script_b);
        run_to_end(&mut b).unwrap();
        match (&a.current_call.stack[0], &b.current_call.stack[0]) {
            (Value::String(sa), Value::String(sb)) => {
                assert_ne!(sa.as_bytes(), sb.as_bytes());
            }
            _ => panic!(),
        }
    }

    fn hex_to_bytes(h: &str) -> Vec<u8> {
        let h: std::string::String = h.chars().filter(|c| !c.is_whitespace()).collect();
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    // ── Phase 9: anchor ratchet ──────────────────────────────────

    #[test]
    fn anchor_ratchet_changes_value_and_is_deterministic() {
        let a = Anchor([0xaa; 32]);
        let b = a.ratchet();
        assert_ne!(a.0, b.0);
        // deterministic: same input → same output
        let b2 = a.ratchet();
        assert_eq!(b.0, b2.0);
        // ratcheting again diverges further
        let c = b.ratchet();
        assert_ne!(b.0, c.0);
    }

    // ── Phase 9: cell construction & opcodes ─────────────────────

    use crate::cell::{CallProof, Cell, Predicate, PredicateTree};
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;

    /// Fixed blinding-key seed for tests — keeps tree construction
    /// deterministic across runs.
    const TEST_BLINDING_KEY: [u8; 32] = [0u8; 32];

    /// Helper: builds a single-leaf `PredicateTree` from a program and a
    /// known internal-key scalar, plus the `CallProof` that opens it.
    fn build_predicate_with_program(
        program: &[u8],
        internal_secret: u64,
    ) -> (PredicateTree, CallProof) {
        let secret = Scalar::from(internal_secret);
        let x_point = &secret * &RISTRETTO_BASEPOINT_TABLE;
        let internal_key = x_point.compress();
        let tree = PredicateTree::new(
            Some(internal_key),
            vec![program.to_vec()],
            TEST_BLINDING_KEY,
        )
        .unwrap();
        let cp = tree.callproof_for(0).unwrap();
        (tree, cp)
    }

    /// Helper: builds a `PredicateTree` with multiple programs and the
    /// `CallProof` that opens the `program_index`-th one.
    fn build_multi_leaf_predicate(
        programs: Vec<Vec<u8>>,
        program_index: usize,
        internal_secret: u64,
    ) -> (PredicateTree, CallProof) {
        let secret = Scalar::from(internal_secret);
        let x_point = &secret * &RISTRETTO_BASEPOINT_TABLE;
        let internal_key = x_point.compress();
        let tree = PredicateTree::new(
            Some(internal_key),
            programs,
            TEST_BLINDING_KEY,
        )
        .unwrap();
        let cp = tree.callproof_for(program_index).unwrap();
        (tree, cp)
    }

    /// Helper: appends script bytes that push a CallProof's four pieces
    /// onto the stack in the order `open` expects: internal_key (Point),
    /// neighbors (Dict), position (String), program (String).
    ///
    /// Builds the neighbors Dict via the `dict` opcode: for n entries,
    /// pushes `(val_0, key_0, val_1, key_1, … , n)` then `dict`. Keys
    /// are the indices `0..n-1`, so the resulting Dict is canonical
    /// list-style.
    fn push_callproof_pieces(script: &mut Vec<u8>, cp: &CallProof) {
        push_point_bytes(script, cp.internal_key.as_bytes());
        for (i, h) in cp.neighbors.iter().enumerate() {
            push_string_bytes(script, h); // val
            push_small_uint(script, i as u32); // key
        }
        push_small_uint(script, cp.neighbors.len() as u32);
        script.push(0x60); // dict
        push_string_bytes(script, &cp.position);
        push_string_bytes(script, &cp.program);
    }

    /// Helper: builds a script that pushes a `String` value of the given bytes.
    /// Uses the sub-varint length-prefix the `pushstr` opcode expects.
    fn push_string_bytes(script: &mut Vec<u8>, bytes: &[u8]) {
        script.push(0x19); // pushstr
        if bytes.len() <= 255 {
            script.push(0x00); // sub-varint tag 0
            script.push(bytes.len() as u8);
        } else {
            // tag 1: 2 LE bytes; value = 256 + w. We support up to 65791.
            script.push(0x01);
            let w = (bytes.len() - 256) as u16;
            script.extend_from_slice(&w.to_le_bytes());
        }
        script.extend_from_slice(bytes);
    }

    /// Helper: builds a script that pushes a Point value (1a + 32 bytes).
    fn push_point_bytes(script: &mut Vec<u8>, bytes: &[u8; 32]) {
        script.push(0x1a);
        script.extend_from_slice(bytes);
    }

    #[test]
    fn cell_opcode_requires_seeded_anchor() {
        // push:7 (payload), push:1 (count), pushpoint(some), cell
        let mut script = vec![0x07, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91);
        let mut vm = vm_with_script(script);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::AnchorMissing));
    }

    #[test]
    fn cell_opcode_builds_a_cell_and_ratchets_anchor() {
        // Seed an anchor, then build a cell.
        let mut script = vec![0x07, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91);
        let mut vm = vm_with_script(script);
        let seed = Anchor([0x42; 32]);
        vm.last_anchor = Some(seed);
        run_to_end(&mut vm).unwrap();
        // Stack should have a single Cell.
        assert_eq!(vm.current_call.stack.len(), 1);
        match &vm.current_call.stack[0] {
            Value::Cell(c) => {
                // The cell took the seed as its anchor.
                assert_eq!(c.anchor.0, seed.0);
                // last_anchor advanced.
                let next = vm.last_anchor.unwrap();
                assert_ne!(next.0, seed.0);
            }
            other => panic!("expected Cell, got {}", value_kind(other)),
        }
    }

    #[test]
    fn cell_opcode_rejects_non_portable_payload() {
        // Build a ClearToken (linear, can be non-portable if qty < 0;
        // even zero-qty cleartoken is non-portable only if qty < 0 — but
        // ClearToken is still considered non-portable when we push it
        // because is_portable returns true for zero-qty.
        // Instead test with a `Merlin` (always non-portable).
        let mut script = Vec::new();
        push_string_bytes(&mut script, b""); // empty label
        script.push(0x69); // merlin → pushes Merlin (non-portable)
        script.push(0x01); // push:1 (count)
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::NonPortableInOutput
        ));
    }

    #[test]
    fn cell_is_noncopyable_and_nondroppable() {
        // build cell, then dup → TypeNotCopyable
        let mut script = vec![0x07, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91);
        script.push(0x20); // dup:0
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotCopyable
        ));

        // build cell, then drop → TypeNotDroppable
        let mut script = vec![0x07, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91);
        script.push(0x1c); // drop
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::TypeNotDroppable
        ));
    }

    #[test]
    fn output_opcode_emits_to_txlog_without_pushing() {
        let mut script = vec![0x07, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x92); // output
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        // Stack is empty.
        assert!(vm.current_call.stack.is_empty());
        // Txlog has one Output entry.
        assert_eq!(vm.txlog.len(), 1);
        match &vm.txlog[0] {
            crate::tx::TxEntry::Output(_) => {}
            _ => panic!("expected Output entry"),
        }
    }

    #[test]
    fn open_with_valid_callproof_runs_program() {
        // Cell payload: 5. Program: drop. After open: payload poured to
        // stack, program drops it → empty stack.
        let inner_program = vec![0x1c]; // drop
        let (tree, cp) = build_predicate_with_program(&inner_program, 7);
        let pred_point = tree.compute_point();

        // Script: push payload(5), push count(1), pushpoint(pred), cell,
        //         push callproof pieces (internal_key, neighbors, pos, prog),
        //         push k=0 (no args), open.
        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, pred_point.as_bytes());
        script.push(0x91); // cell
        push_callproof_pieces(&mut script, &cp);
        script.push(0x00); // k=0 args
        script.push(0x93); // open
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
    }

    #[test]
    fn open_with_wrong_program_hard_fails() {
        // Predicate commits to `drop`; callproof claims `nop` instead.
        let real_program = vec![0x1c];
        let fake_program = vec![0x1d];
        let (tree, _real_cp) = build_predicate_with_program(&real_program, 7);
        let cp = CallProof {
            internal_key: tree.internal_key,
            neighbors: Vec::new(),
            position: Vec::new(),
            program: fake_program,
        };
        let pred_point = tree.compute_point();

        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, pred_point.as_bytes());
        script.push(0x91);
        push_callproof_pieces(&mut script, &cp);
        script.push(0x00);
        script.push(0x93);
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::CallProofMismatch
        ));
    }

    #[test]
    fn open_passes_args_after_payload() {
        // Cell payload: [10]. args: [20, 30]. Program: stack must end with
        // exactly the args + payload arrangement; cleanup leaves stack
        // empty. Inside the cell-run, stack = [10, 20, 30]. Program: drop
        // three items.
        let inner_program = vec![0x1c, 0x1c, 0x1c]; // drop, drop, drop
        let (tree, cp) = build_predicate_with_program(&inner_program, 11);
        let pred_point = tree.compute_point();

        let mut script = vec![0x0a, 0x01]; // payload=10, count=1
        push_point_bytes(&mut script, pred_point.as_bytes());
        script.push(0x91);                  // cell
        push_callproof_pieces(&mut script, &cp);
        // push args 20, 30 (deepest first) and k=2
        script.push(0x14);                  // pushint64 positive
        script.extend_from_slice(&20u64.to_le_bytes());
        script.push(0x14);
        script.extend_from_slice(&30u64.to_le_bytes());
        script.push(0x02);                  // k=2
        script.push(0x93);                  // open
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
    }

    #[test]
    fn signtx_pours_payload_and_records_txbound_sig() {
        // Build cell with payload [5, 7], then signtx.
        let mut script = vec![0x05, 0x07, 0x02];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell
        script.push(0x98); // signtx
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        // Stack now has [5, 7, count=2].
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[2], Int253::from(2u64));
        // Exactly one TxBound deferred sig recorded. No message, no sig
        // bytes — those come from the tx envelope at finalize.
        assert_eq!(vm.deferred_sigs.len(), 1);
        match &vm.deferred_sigs[0] {
            DeferredSig::TxBound { verification_key } => {
                assert_eq!(verification_key.as_bytes(), &[0xaa; 32]);
            }
            DeferredSig::Explicit { .. } => panic!("expected TxBound, got Explicit"),
        }
    }

    #[test]
    fn signrun_records_explicit_sig_and_runs_program() {
        let prog = vec![0x1c]; // drop
        let sig_bytes = [0u8; 64];

        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell
        push_string_bytes(&mut script, &prog);
        push_string_bytes(&mut script, &sig_bytes);
        script.push(0x00); // m=0 args
        script.push(0x99); // signrun
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
        assert_eq!(vm.deferred_sigs.len(), 1);
        match &vm.deferred_sigs[0] {
            DeferredSig::Explicit {
                verification_key,
                message,
                signature,
            } => {
                assert_eq!(verification_key.as_bytes(), &[0xaa; 32]);
                assert_eq!(signature, &sig_bytes);
                // message must be the program-only Merlin transcript output.
                assert_eq!(message.len(), 32);
            }
            DeferredSig::TxBound { .. } => panic!("expected Explicit, got TxBound"),
        }
    }

    #[test]
    fn signrun_message_binds_only_to_program_not_to_cell() {
        // Two different cells running the same program produce
        // identical deferred-sig messages — confirms architect's
        // intent that signrun binds only to the program.
        fn run_signrun(predicate_byte: u8) -> DeferredSig {
            let prog = vec![0x1c];
            let sig = [0u8; 64];
            let mut script = vec![0x05, 0x01];
            push_point_bytes(&mut script, &[predicate_byte; 32]);
            script.push(0x91);
            push_string_bytes(&mut script, &prog);
            push_string_bytes(&mut script, &sig);
            script.push(0x00);
            script.push(0x99);
            let mut vm = vm_with_script(script);
            vm.last_anchor = Some(Anchor([0x42; 32]));
            run_to_end(&mut vm).unwrap();
            vm.deferred_sigs.into_iter().next().unwrap()
        }
        let s1 = run_signrun(0xaa);
        let s2 = run_signrun(0xbb);
        let (m1, m2) = match (&s1, &s2) {
            (
                DeferredSig::Explicit { message: m1, .. },
                DeferredSig::Explicit { message: m2, .. },
            ) => (m1.clone(), m2.clone()),
            _ => panic!("expected Explicit on both"),
        };
        assert_eq!(m1, m2, "signrun message must be program-only");
    }

    // ── Multi-leaf PredicateTree (item 9.10) ─────────────────────

    #[test]
    fn predicate_tree_new_validates_inputs() {
        // Empty programs → EmptyPredicateTree.
        let secret = Scalar::from(1u64);
        let ik = (&secret * &RISTRETTO_BASEPOINT_TABLE).compress();
        assert!(matches!(
            PredicateTree::new(Some(ik), Vec::new(), TEST_BLINDING_KEY).unwrap_err(),
            VMError::EmptyPredicateTree
        ));
        // Garbage internal_key bytes → InvalidPoint.
        let bad = CompressedRistretto([0xff; 32]); // not a valid Ristretto point
        assert!(matches!(
            PredicateTree::new(Some(bad), vec![vec![0x1d]], TEST_BLINDING_KEY).unwrap_err(),
            VMError::InvalidPoint
        ));
        // None → unspendable internal key (B_blinding). Succeeds and
        // produces a tree whose internal key is the unspendable point.
        let tree = PredicateTree::new(None, vec![vec![0x1d]], TEST_BLINDING_KEY).unwrap();
        assert_eq!(*tree.internal_key(), Predicate::unspendable_key());
        // scripts_only is the documented convenience wrapper for the same
        // pattern; it must produce an identical tree.
        let via_helper = PredicateTree::scripts_only(
            vec![vec![0x1d]],
            TEST_BLINDING_KEY,
        )
        .unwrap();
        assert_eq!(via_helper.compute_point(), tree.compute_point());
    }

    #[test]
    fn scripts_only_predicate_opens_via_program_path() {
        // End-to-end: build a scripts-only predicate, lock a cell under
        // it, and unlock via `open` with the script-path proof. Verifies
        // the unspendable-internal-key construction is wire-compatible
        // with the existing `open` opcode.
        let program = vec![0x1c]; // drop (cell payload is one item)
        let tree = PredicateTree::scripts_only(
            vec![program.clone()],
            TEST_BLINDING_KEY,
        )
        .unwrap();
        let cp = tree.callproof_for(0).unwrap();
        let pred_point = tree.compute_point();

        let mut script = vec![0x05, 0x01]; // payload: push:5; k=1
        push_point_bytes(&mut script, pred_point.as_bytes());
        script.push(0x91); // cell
        push_callproof_pieces(&mut script, &cp);
        script.push(0x00); // 0 args
        script.push(0x93); // open
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        run_to_end(&mut vm).unwrap();
        assert!(vm.current_call.stack.is_empty());
    }

    #[test]
    fn multi_leaf_predicate_each_program_unlocks_via_its_path() {
        // Three programs; build a CallProof for each and confirm open succeeds.
        let programs: Vec<Vec<u8>> = vec![
            vec![0x1c],           // drop
            vec![0x1c, 0x1c],     // drop, drop
            vec![0x1c, 0x1c, 0x1c], // drop, drop, drop
        ];
        // payload size must match the program's drop count so the cell-open
        // run leaves an empty stack. Test each program with that exact payload.
        for i in 0..programs.len() {
            let (tree, cp) =
                build_multi_leaf_predicate(programs.clone(), i, 11 + i as u64);
            let pred_point = tree.compute_point();

            // payload = i+1 copies of push:5 (so program of length i+1
            // can drop them all and end with empty stack)
            let payload_count = i + 1;
            let mut script = Vec::new();
            for _ in 0..payload_count {
                script.push(0x05); // push:5
            }
            push_small_uint(&mut script, payload_count as u32);
            push_point_bytes(&mut script, pred_point.as_bytes());
            script.push(0x91); // cell
            push_callproof_pieces(&mut script, &cp);
            script.push(0x00); // k=0 args
            script.push(0x93); // open
            let mut vm = vm_with_script(script);
            vm.last_anchor = Some(Anchor([0x42; 32]));
            run_to_end(&mut vm).unwrap_or_else(|e| {
                panic!("program index {} did not open cleanly: {:?}", i, e)
            });
            assert!(
                vm.current_call.stack.is_empty(),
                "program index {} left stack non-empty",
                i
            );
        }
    }

    #[test]
    fn multi_leaf_predicate_wrong_leaf_path_hard_fails() {
        // Build a 3-leaf tree. Construct a CallProof claiming program[0]
        // but with the path that opens program[1]. Verification must fail.
        let programs: Vec<Vec<u8>> = vec![
            vec![0x1c],
            vec![0x1c, 0x1c],
            vec![0x1c, 0x1c, 0x1c],
        ];
        let (tree, valid_cp_for_1) =
            build_multi_leaf_predicate(programs.clone(), 1, 7);
        // Forge: use program[0]'s bytes but program[1]'s path/neighbors.
        let forged = CallProof {
            internal_key: valid_cp_for_1.internal_key,
            neighbors: valid_cp_for_1.neighbors.clone(),
            position: valid_cp_for_1.position.clone(),
            program: programs[0].clone(),
        };
        let pred_point = tree.compute_point();

        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, pred_point.as_bytes());
        script.push(0x91);
        push_callproof_pieces(&mut script, &forged);
        script.push(0x00);
        script.push(0x93);
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::CallProofMismatch
        ));
    }

    #[test]
    fn callproof_for_out_of_range_index_errors() {
        let secret = Scalar::from(1u64);
        let ik = (&secret * &RISTRETTO_BASEPOINT_TABLE).compress();
        let tree = PredicateTree::new(
            Some(ik),
            vec![vec![0x1d], vec![0x1c]],
            TEST_BLINDING_KEY,
        )
        .unwrap();
        assert!(matches!(
            tree.callproof_for(5).unwrap_err(),
            VMError::ProgramIndexOutOfRange
        ));
    }

    // ── Re-packaging guard: cells can't be sealed into other cells ───
    //
    // A cell's payload bytes can only return to the stack via `open`,
    // `signtx`, or `signrun` — each of which consumes the source cell
    // and records (open) or defers (signtx/signrun) an authorization
    // check. There is no "transfer the cell handle into a new output"
    // shortcut, because `Value::Cell` is non-portable and the cell
    // construction opcodes (`cell`, `output`) reject non-portable
    // payload items.

    #[test]
    fn output_rejects_cell_as_payload_item() {
        // Build cell A on stack. Then try: count=1, predicate_point, output
        //   — the output op pops pred + count + 1 payload item (cell A)
        //     and `pop_n_portable` must reject cell A.
        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell A → on stack
        // Now build the outer: 1-item payload = [cell A], pred, output.
        script.push(0x01); // count = 1
        push_point_bytes(&mut script, &[0xbb; 32]);
        script.push(0x92); // output
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::NonPortableInOutput
        ));
    }

    #[test]
    fn cell_opcode_rejects_cell_as_payload_item() {
        // Symmetric protection on the `cell` construction op.
        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell A
        script.push(0x01); // count=1
        push_point_bytes(&mut script, &[0xbb; 32]);
        script.push(0x91); // cell (attempted outer)
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::NonPortableInOutput
        ));
    }

    #[test]
    fn output_rejects_dict_containing_a_cell() {
        // Even if a script hides a cell inside a Dict and puts the Dict
        // (otherwise portable) into the payload, the Dict's sticky
        // portability flag rejects it.
        //
        //   build cell A
        //   push key=0, push count=1, dict        // Dict { 0: cellA }
        //   push count=1, pushpoint, output
        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell A on stack
        script.push(0x00); // key = 0
        script.push(0x01); // count = 1 pair
        script.push(0x60); // dict — pops (cellA, 0, 1) → Dict { 0: cellA }
        script.push(0x01); // outer count = 1
        push_point_bytes(&mut script, &[0xbb; 32]);
        script.push(0x92); // output
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::NonPortableInOutput
        ));
    }

    #[test]
    fn cell_id_changes_when_payload_value_changes() {
        // Two cells with the same predicate + same anchor + same payload
        // type-shape but different values must have different ids.
        // Per architect response 9.3: payload bytes are bound via the
        // canonical encoding API.
        let pred = Predicate::Opaque(CompressedRistretto([0xaa; 32]));
        let a = Anchor([0x42; 32]);
        let c1 = Cell::new(
            pred.clone(),
            a,
            vec![Value::Int253(Int253::from(5u64))],
        );
        let c2 = Cell::new(
            pred,
            a,
            vec![Value::Int253(Int253::from(99u64))],
        );
        assert_ne!(c1.id(), c2.id());
    }

    #[test]
    fn signrun_rejects_wrong_signature_length() {
        // payload(5), count(1), predicate, cell, then signrun-with-bad-sig.
        let mut script = vec![0x05, 0x01];
        push_point_bytes(&mut script, &[0xaa; 32]);
        script.push(0x91); // cell
        push_string_bytes(&mut script, &[0x1c]); // prog = drop
        push_string_bytes(&mut script, &[0u8; 63]); // sig of wrong length
        script.push(0x00);
        script.push(0x99);
        let mut vm = vm_with_script(script);
        vm.last_anchor = Some(Anchor([0x42; 32]));
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::BadSignatureBytes
        ));
    }

    // ── Phase 8: tokens (port from zkvm) ─────────────────────────

    use crate::token::flavor_from_actor as test_flavor_from_actor;
    use crate::{Commitment, Token};

    /// Convenience: builds a Token via the cleartext constructor for tests.
    fn make_cleartext_token(qty: u64, flv: u64) -> Token {
        Token::cleartext(Int253::from(qty), Int253::from(flv))
    }

    // ── Type-shape tests ─────────────────────────────────────────

    #[test]
    fn token_cleartext_constructor_packs_unblinded_commitments() {
        let t = make_cleartext_token(123, 7);
        assert_eq!(t.qty.assignment(), Some(Int253::from(123u64)));
        assert_eq!(t.flv.assignment(), Some(Int253::from(7u64)));
        // Witness uses zero blinding.
        let (_, b) = t.qty.witness().expect("open commitment");
        assert_eq!(b, Scalar::zero());
    }

    #[test]
    fn token_is_noncopyable_and_nondroppable() {
        let v = Value::Token(make_cleartext_token(1, 2));
        assert!(!v.is_copyable(), "Token must not be copyable");
        assert!(!v.is_droppable(), "Token must not be droppable");
        assert!(v.is_portable(), "Token must be portable");
        assert!(matches!(v.try_clone(), Err(VMError::TypeNotCopyable)));
    }

    #[test]
    fn cleartoken_zero_qty_is_droppable() {
        let v = Value::ClearToken(ClearToken::new(Int253::zero(), Int253::from(7u64)));
        assert!(v.is_droppable());
    }

    #[test]
    fn cleartoken_nonzero_qty_is_not_droppable() {
        let v = Value::ClearToken(ClearToken::new(Int253::from(1u64), Int253::from(7u64)));
        assert!(!v.is_droppable());
    }

    #[test]
    fn cleartoken_negative_qty_is_non_portable() {
        let v = Value::ClearToken(ClearToken::new(Int253::from(-1i64), Int253::from(7u64)));
        assert!(!v.is_portable());
        // Still non-copyable.
        assert!(!v.is_copyable());
    }

    #[test]
    fn cleartoken_positive_qty_is_portable() {
        let v = Value::ClearToken(ClearToken::new(Int253::from(5u64), Int253::from(7u64)));
        assert!(v.is_portable());
    }

    #[test]
    fn flavor_from_actor_is_deterministic_and_diverges_on_inputs() {
        let actor1 = ActorID([0x11; 32]);
        let actor2 = ActorID([0x22; 32]);
        let tag_a = String::from(b"gold".to_vec());
        let tag_b = String::from(b"silver".to_vec());

        let f_aa = test_flavor_from_actor(&actor1, &tag_a);
        let f_aa_2 = test_flavor_from_actor(&actor1, &tag_a);
        assert_eq!(f_aa, f_aa_2, "deterministic for identical inputs");

        let f_ab = test_flavor_from_actor(&actor1, &tag_b);
        let f_ba = test_flavor_from_actor(&actor2, &tag_a);
        assert_ne!(f_aa, f_ab, "tag change must change flavor");
        assert_ne!(f_aa, f_ba, "actor change must change flavor");

        // Non-negative by construction (mod-order wide reduction).
        assert!(!f_aa.is_negative());
    }

    // ── ClearToken arithmetic tests ──────────────────────────────

    #[test]
    fn cleartoken_merge_into_same_flavor_sums_qtys() {
        let a = ClearToken::new(Int253::from(3u64), Int253::from(7u64));
        let b = ClearToken::new(Int253::from(4u64), Int253::from(7u64));
        let c = a.merge_into(b).expect("same flavor merges");
        assert_eq!(c.qty(), Int253::from(7u64));
        assert_eq!(c.flv(), Int253::from(7u64));
    }

    #[test]
    fn cleartoken_merge_into_mismatched_flavor_returns_originals() {
        let a = ClearToken::new(Int253::from(3u64), Int253::from(7u64));
        let b = ClearToken::new(Int253::from(4u64), Int253::from(8u64));
        let (a2, b2) = a.merge_into(b).expect_err("mismatch returns Err");
        assert_eq!(a2.qty(), Int253::from(3u64));
        assert_eq!(b2.qty(), Int253::from(4u64));
    }

    #[test]
    fn cleartoken_split_within_qty() {
        let a = ClearToken::new(Int253::from(10u64), Int253::from(7u64));
        let (rem, b) = a.split(Int253::from(3u64)).expect("split ok");
        assert_eq!(rem.qty(), Int253::from(7u64));
        assert_eq!(rem.flv(), Int253::from(7u64));
        assert_eq!(b.qty(), Int253::from(3u64));
        assert_eq!(b.flv(), Int253::from(7u64));
    }

    #[test]
    fn cleartoken_split_above_qty_returns_none() {
        let a = ClearToken::new(Int253::from(2u64), Int253::from(7u64));
        assert!(a.split(Int253::from(3u64)).is_none());
    }

    #[test]
    fn cleartoken_split_negative_q_returns_none() {
        let a = ClearToken::new(Int253::from(5u64), Int253::from(7u64));
        assert!(a.split(Int253::from(-1i64)).is_none());
    }

    #[test]
    fn cleartoken_negated_flips_qty_sign() {
        let a = ClearToken::new(Int253::from(5u64), Int253::from(7u64));
        let n = a.negated();
        assert_eq!(n.qty(), Int253::from(-5i64));
        assert_eq!(n.flv(), Int253::from(7u64));
    }

    // ── Opcode tests ─────────────────────────────────────────────

    /// Builds a VM running `script` under InternalRoot with a specific
    /// actor identity (so `op_issue` has actor context).
    fn vm_internal_with_actor(script: Vec<u8>, actor: ActorID) -> VM {
        let kind = CallKind::InternalRoot {
            actor,
            method: MethodKey(0),
            caller: None,
            anchor: Anchor([0u8; 32]),
        };
        VM::new(
            dummy_header(),
            CallFrame::new(script, kind, 1_000_000, 0, 0),
        )
    }

    #[test]
    fn amount_on_cleartoken_pushes_qty_and_flv() {
        // Pre-load a ClearToken on the stack, run `amount`, verify the
        // shape `cleartoken(qty,flv) → cleartoken qty flv`.
        let mut vm = vm_with_script(vec![0x70]); // amount
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(11u64),
            Int253::from(22u64),
        )));
        vm.step_internal().expect("step ok");
        assert_eq!(vm.current_call.stack.len(), 3);
        // Bottom: the original cleartoken.
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => {
                assert_eq!(t.qty(), Int253::from(11u64));
                assert_eq!(t.flv(), Int253::from(22u64));
            }
            _ => panic!("bottom must be original ClearToken"),
        }
        // Middle: qty.
        assert_int(&vm.current_call.stack[1], Int253::from(11u64));
        // Top: flv.
        assert_int(&vm.current_call.stack[2], Int253::from(22u64));
    }

    #[test]
    fn amount_on_token_pushes_points() {
        let mut vm = vm_with_script(vec![0x70]);
        vm.push_value(Value::Token(make_cleartext_token(33, 44)));
        vm.step_internal().expect("step ok");
        assert_eq!(vm.current_call.stack.len(), 3);
        match &vm.current_call.stack[0] {
            Value::Token(_) => {}
            _ => panic!("bottom must be original Token"),
        }
        match &vm.current_call.stack[1] {
            Value::Point(_) => {}
            _ => panic!("middle must be Point (qty commitment)"),
        }
        match &vm.current_call.stack[2] {
            Value::Point(_) => {}
            _ => panic!("top must be Point (flv commitment)"),
        }
    }

    #[test]
    fn amount_on_non_token_errors_typenottoken() {
        let mut vm = vm_with_script(vec![0x70]);
        vm.push_value(Value::Int253(Int253::from(5u64)));
        let err = vm.step_internal().unwrap_err();
        assert!(matches!(err, VMError::TypeNotToken));
        // Original value is restored on error.
        assert_eq!(vm.current_call.stack.len(), 1);
    }

    #[test]
    fn issue_clear_path_emits_txlog_and_returns_cleartoken() {
        // Script: pushint8(7), pushstr "gold", issue.
        // Run under InternalRoot with a known actor identity so
        // `op_issue` can resolve a flavor.
        let actor = ActorID([0x55; 32]);
        let mut script = vec![0x10, 7];
        push_string_bytes(&mut script, b"gold");
        script.push(0x71); // issue
        let mut vm = vm_internal_with_actor(script, actor);
        run_to_end(&mut vm).expect("issue ok");

        // Stack: [ClearToken(7, flavor)].
        assert_eq!(vm.current_call.stack.len(), 1);
        let expected_flv =
            test_flavor_from_actor(&actor, &String::from(b"gold".to_vec()));
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => {
                assert_eq!(t.qty(), Int253::from(7u64));
                assert_eq!(t.flv(), expected_flv);
            }
            _ => panic!("expected ClearToken"),
        }

        // Txlog has Issue entry with unblinded commitments.
        assert_eq!(vm.txlog.len(), 1);
        let expected_qty_pt = Commitment::unblinded(Int253::from(7u64)).to_point();
        let expected_flv_pt = Commitment::unblinded(expected_flv).to_point();
        match &vm.txlog[0] {
            crate::tx::TxEntry::Issue(q, f) => {
                assert_eq!(*q, expected_qty_pt);
                assert_eq!(*f, expected_flv_pt);
            }
            _ => panic!("expected TxEntry::Issue"),
        }
    }

    #[test]
    fn issue_with_point_qty_errors_tokenrequirescs() {
        // Pushpoint then pushstr then issue → encrypted branch (deferred).
        let actor = ActorID([0x55; 32]);
        let mut script = vec![0x1a]; // pushpoint
        script.extend_from_slice(&[0u8; 32]);
        push_string_bytes(&mut script, b"gold");
        script.push(0x71);
        let mut vm = vm_internal_with_actor(script, actor);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::TokenRequiresCS));
    }

    #[test]
    fn issue_at_external_root_errors_actor_context() {
        // ExternalRoot has no actor identity.
        let mut script = vec![0x10, 7];
        push_string_bytes(&mut script, b"gold");
        script.push(0x71);
        let mut vm = VM::new(
            dummy_header(),
            CallFrame::new(script, CallKind::ExternalRoot, 1_000_000, 0, 0),
        );
        let mut delegate = make_stub_delegate();
        let err = drive_external(&mut vm, &mut delegate).unwrap_err();
        assert!(matches!(err, VMError::OpcodeRequiresActorContext));
    }

    #[test]
    fn retire_cleartoken_emits_txlog() {
        // Pre-load a ClearToken, run `retire`.
        let mut vm = vm_with_script(vec![0x72]);
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(11u64),
            Int253::from(22u64),
        )));
        vm.step_internal().expect("retire ok");
        assert!(vm.current_call.stack.is_empty());
        assert_eq!(vm.txlog.len(), 1);
        let q_pt = Commitment::unblinded(Int253::from(11u64)).to_point();
        let f_pt = Commitment::unblinded(Int253::from(22u64)).to_point();
        match &vm.txlog[0] {
            crate::tx::TxEntry::Retire(q, f) => {
                assert_eq!(*q, q_pt);
                assert_eq!(*f, f_pt);
            }
            _ => panic!("expected TxEntry::Retire"),
        }
    }

    #[test]
    fn retire_token_emits_txlog_with_commitment_points() {
        let token = make_cleartext_token(11, 22);
        let q_pt = token.qty.to_point();
        let f_pt = token.flv.to_point();
        let mut vm = vm_with_script(vec![0x72]);
        vm.push_value(Value::Token(token));
        vm.step_internal().expect("retire ok");
        match &vm.txlog[0] {
            crate::tx::TxEntry::Retire(q, f) => {
                assert_eq!(*q, q_pt);
                assert_eq!(*f, f_pt);
            }
            _ => panic!("expected TxEntry::Retire"),
        }
    }

    #[test]
    fn retire_non_token_errors_typenottoken() {
        let mut vm = vm_with_script(vec![0x72]);
        vm.push_value(Value::Int253(Int253::from(5u64)));
        let err = vm.step_internal().unwrap_err();
        assert!(matches!(err, VMError::TypeNotToken));
    }

    #[test]
    fn borrow_clear_path_returns_neg_pos_pair() {
        // Stack: [qty=5, flv=7] then `borrow` → [neg5, pos5].
        let mut vm = vm_with_script(vec![0x10, 5, 0x10, 7, 0x73]);
        run_to_end(&mut vm).expect("borrow ok");
        assert_eq!(vm.current_call.stack.len(), 2);
        // Bottom: negative qty.
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => {
                assert_eq!(t.qty(), Int253::from(-5i64));
                assert_eq!(t.flv(), Int253::from(7u64));
            }
            _ => panic!("bottom must be -ClearToken"),
        }
        // Top: positive qty.
        match &vm.current_call.stack[1] {
            Value::ClearToken(t) => {
                assert_eq!(t.qty(), Int253::from(5u64));
                assert_eq!(t.flv(), Int253::from(7u64));
            }
            _ => panic!("top must be +ClearToken"),
        }
    }

    #[test]
    fn borrow_with_point_errors_tokenrequirescs() {
        // pushpoint, pushint8(7), borrow → Point qty → CS required.
        let mut script = vec![0x1a];
        script.extend_from_slice(&[0u8; 32]);
        script.extend_from_slice(&[0x10, 7]);
        script.push(0x73);
        let mut vm = vm_with_script(script);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::TokenRequiresCS));
    }

    #[test]
    fn merge_same_flavor_combines_qtys() {
        // Push two cleartokens with same flavor, merge → (merged, 1).
        let mut vm = vm_with_script(vec![0x74]);
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(3u64),
            Int253::from(7u64),
        )));
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(4u64),
            Int253::from(7u64),
        )));
        vm.step_internal().expect("merge ok");
        // Stack: [merged_cleartoken, 1].
        assert_eq!(vm.current_call.stack.len(), 2);
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => assert_eq!(t.qty(), Int253::from(7u64)),
            _ => panic!("bottom must be merged ClearToken"),
        }
        assert_int(&vm.current_call.stack[1], Int253::from(1u64));
    }

    #[test]
    fn merge_flavor_mismatch_soft_fails() {
        let mut vm = vm_with_script(vec![0x74]);
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(3u64),
            Int253::from(7u64),
        )));
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(4u64),
            Int253::from(8u64),
        )));
        vm.step_internal().expect("merge ok (soft-fail)");
        // Stack: [a, b, 0].
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[2], Int253::zero());
    }

    #[test]
    fn split_within_qty_returns_two_cleartokens() {
        // ClearToken(10, 7), pushint8(3), split.
        let mut vm = vm_with_script(vec![0x10, 3, 0x75]);
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(10u64),
            Int253::from(7u64),
        )));
        // Need to move stack so the cleartoken is below the int. The
        // pushint8 runs first, pushing 3 on top, then split pops 3 and
        // the cleartoken below.
        //
        // Reorder: push cleartoken first, then run the script.
        run_to_end(&mut vm).expect("split ok");
        assert_eq!(vm.current_call.stack.len(), 2);
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => assert_eq!(t.qty(), Int253::from(7u64)),
            _ => panic!("bottom must be remainder"),
        }
        match &vm.current_call.stack[1] {
            Value::ClearToken(t) => assert_eq!(t.qty(), Int253::from(3u64)),
            _ => panic!("top must be new ClearToken"),
        }
    }

    #[test]
    fn split_above_qty_hard_fails() {
        let mut vm = vm_with_script(vec![0x10, 9, 0x75]);
        vm.push_value(Value::ClearToken(ClearToken::new(
            Int253::from(2u64),
            Int253::from(7u64),
        )));
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::TokenSplitOutOfRange));
    }

    #[test]
    fn issueflv_pushes_correct_flavor() {
        // pushstr <32-byte cid>, pushstr "gold", issueflv.
        let actor_bytes = [0xab; 32];
        let mut script = Vec::new();
        push_string_bytes(&mut script, &actor_bytes);
        push_string_bytes(&mut script, b"gold");
        script.push(0x78); // issueflv
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).expect("issueflv ok");
        let expected = test_flavor_from_actor(
            &ActorID(actor_bytes),
            &String::from(b"gold".to_vec()),
        );
        assert_eq!(vm.current_call.stack.len(), 1);
        assert_int(&vm.current_call.stack[0], expected);
    }

    #[test]
    fn issueflv_rejects_non_32_byte_cid() {
        let mut script = Vec::new();
        push_string_bytes(&mut script, &[0xab; 16]); // 16-byte cid
        push_string_bytes(&mut script, b"gold");
        script.push(0x78);
        let mut vm = vm_with_script(script);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::IndexOutOfRange));
    }

    /// Quick helper: builds a stub delegate for external-context tests
    /// that need to step through `step_external` once. Defined as a
    /// free function so the borrowing patterns in
    /// `issue_at_external_root_errors_actor_context` work.
    fn make_stub_delegate() -> StubDelegate {
        StubDelegate::new()
    }

    /// Drives `vm.step_external(delegate)` until done or error,
    /// returning the error if any. Doesn't call finalize.
    fn drive_external(
        vm: &mut VM,
        delegate: &mut StubDelegate,
    ) -> Result<(), VMError> {
        while vm.step_external(delegate)? {}
        Ok(())
    }

    // ── Phase 10: input opcode ───────────────────────────────────

    /// Builds a VM running `script` under `ExternalRoot`. Mirror of
    /// `vm_with_script` for the external-context opcode tests.
    fn vm_external_with_script(script: Vec<u8>) -> VM {
        VM::new(
            dummy_header(),
            CallFrame::new(script, CallKind::ExternalRoot, 1_000_000, 0, 0),
        )
    }

    /// Builds a wire-encoded cell as a `Vec<u8>` so tests can feed it
    /// to the `input` opcode (which pops a `String` and decodes it).
    fn encode_cell_to_bytes(cell: &Cell) -> Vec<u8> {
        let mut buf = Vec::new();
        cell.encode(&mut buf).expect("cell encodes");
        buf
    }

    /// Builds a non-trivial test cell — opaque predicate, fixed anchor,
    /// two-item portable payload. Used by both the round-trip and the
    /// `input` opcode tests.
    fn fixture_cell() -> Cell {
        let predicate = Predicate::Opaque(CompressedRistretto([0xaa; 32]));
        let anchor = Anchor([0x42; 32]);
        let payload = vec![
            Value::Int253(Int253::from(7u64)),
            Value::String(crate::String::from(b"hello".to_vec())),
        ];
        Cell::new(predicate, anchor, payload)
    }

    #[test]
    fn cell_encode_decode_roundtrip() {
        let original = fixture_cell();
        let bytes = encode_cell_to_bytes(&original);

        // Decode and confirm equivalence by cell id (the canonical
        // identity hash binds predicate point + anchor + payload bytes).
        let mut reader: &[u8] = &bytes;
        let decoded = Cell::decode(&mut reader).expect("decodes");
        assert!(reader.is_empty(), "decoder must consume the full input");
        assert_eq!(original.id(), decoded.id());
        assert_eq!(original.anchor.0, decoded.anchor.0);
        assert_eq!(
            original.predicate.to_point().as_bytes(),
            decoded.predicate.to_point().as_bytes()
        );
        assert_eq!(original.payload.len(), decoded.payload.len());
    }

    /// Helper: `Cell::decode` returns a `Cell` on success, which lacks
    /// `Debug`. This wrapper drops the cell so tests can use the usual
    /// `.unwrap_err()` shape on a `Debug`-able result.
    fn decode_cell_dropping_ok(bytes: &[u8]) -> Result<(), VMError> {
        let mut r: &[u8] = bytes;
        match Cell::decode(&mut r) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    #[test]
    fn cell_decode_rejects_empty_input() {
        let err = decode_cell_dropping_ok(&[]).unwrap_err();
        assert!(matches!(err, VMError::MalformedCellEncoding));
    }

    #[test]
    fn cell_decode_rejects_wrong_outer_count() {
        // Outer list-Dict with two entries instead of three (anchor +
        // payload prefix, no predicate). Bytes are hand-rolled to make
        // the outer prefix valid but the inner shape wrong.
        // Outer count = 2 (immediate small list-Dict tag in the encoding).
        // We exploit the fact that any prefix that successfully reads as
        // a list-Dict with count != 3 must fail.
        // Construct a real cell and then patch the outer count.
        let mut bytes = encode_cell_to_bytes(&fixture_cell());
        // First byte encodes the outer list-Dict prefix; just rewrite the
        // top-level prefix byte to a list-Dict of count 2. We use the
        // round-trip helper: build a 2-element list-Dict by hand.
        // Simpler: replace the *whole* string with a list-Dict of count 0,
        // which is canonical but wrong arity.
        bytes.clear();
        crate::encoding::write_list_prefix(&mut bytes, 0)
            .expect("write prefix");
        let err = decode_cell_dropping_ok(&bytes).unwrap_err();
        assert!(matches!(err, VMError::MalformedCellEncoding));
    }

    #[test]
    fn cell_decode_rejects_wrong_anchor_length() {
        // Build an outer list-Dict of 3 entries by hand: Point predicate,
        // a String of wrong (31-byte) anchor, then an empty payload list.
        let mut bytes = Vec::new();
        crate::encoding::write_list_prefix(&mut bytes, 3)
            .expect("write outer prefix");
        crate::encoding::write_value(
            &mut bytes,
            &Value::Point(Point::from_bytes([0xaa; 32])),
        )
        .expect("write point");
        crate::encoding::write_value(
            &mut bytes,
            &Value::String(crate::String::from(vec![0u8; 31])),
        )
        .expect("write short anchor");
        crate::encoding::write_list_prefix(&mut bytes, 0)
            .expect("write payload prefix");
        let err = decode_cell_dropping_ok(&bytes).unwrap_err();
        assert!(matches!(err, VMError::MalformedCellEncoding));
    }

    #[test]
    fn cell_decode_rejects_predicate_not_a_point() {
        // First entry is a String where a Point is expected.
        let mut bytes = Vec::new();
        crate::encoding::write_list_prefix(&mut bytes, 3)
            .expect("write outer prefix");
        crate::encoding::write_value(
            &mut bytes,
            &Value::String(crate::String::from(vec![0u8; 32])),
        )
        .expect("write wrong predicate");
        crate::encoding::write_value(
            &mut bytes,
            &Value::String(crate::String::from(vec![0u8; 32])),
        )
        .expect("write anchor");
        crate::encoding::write_list_prefix(&mut bytes, 0)
            .expect("write payload prefix");
        let err = decode_cell_dropping_ok(&bytes).unwrap_err();
        assert!(matches!(err, VMError::MalformedCellEncoding));
    }

    #[test]
    fn input_pushes_cell_seeds_anchor_and_emits_txlog() {
        let cell = fixture_cell();
        let expected_id = cell.id();
        let expected_anchor = cell.to_anchor();
        let bytes = encode_cell_to_bytes(&cell);

        // Build an ExternalRoot VM with the wire bytes on the stack as a String.
        let mut vm = vm_external_with_script(Vec::new());
        vm.push_value(Value::String(crate::String::from(bytes)));
        vm.op_input().expect("input succeeds");

        // Top of stack is the decoded Cell.
        assert_eq!(vm.current_call.stack.len(), 1);
        match &vm.current_call.stack[0] {
            Value::Cell(c) => {
                assert_eq!(c.id(), expected_id);
            }
            other => panic!("expected Cell on stack, got {}", value_kind(other)),
        }

        // last_anchor seeded to the cell's ratcheted anchor.
        assert_eq!(vm.last_anchor.expect("anchor seeded").0, expected_anchor.0);

        // Txlog has exactly one Input entry committing the cell id.
        assert_eq!(vm.txlog.len(), 1);
        match &vm.txlog[0] {
            crate::tx::TxEntry::Input(id) => assert_eq!(*id, expected_id),
            _ => panic!("expected TxEntry::Input"),
        }
    }

    #[test]
    fn input_requires_string_on_top() {
        // Non-String top → TypeNotString. (Use an Int253.)
        let mut vm = vm_external_with_script(Vec::new());
        vm.push_value(Value::Int253(Int253::from(7u64)));
        let err = vm.op_input().unwrap_err();
        assert!(matches!(err, VMError::TypeNotString));
    }

    #[test]
    fn input_rejects_malformed_bytes() {
        // Random non-canonical bytes on the stack.
        let mut vm = vm_external_with_script(Vec::new());
        vm.push_value(Value::String(crate::String::from(vec![0xffu8; 8])));
        let err = vm.op_input().unwrap_err();
        assert!(matches!(err, VMError::MalformedCellEncoding));
    }

    #[test]
    fn input_rejects_trailing_bytes_after_cell() {
        // Append a stray byte after a canonical encoding so the inner
        // reader leaves bytes unread → MalformedCellEncoding.
        let cell = fixture_cell();
        let mut bytes = encode_cell_to_bytes(&cell);
        bytes.push(0x00); // trailing garbage

        let mut vm = vm_external_with_script(Vec::new());
        vm.push_value(Value::String(crate::String::from(bytes)));
        let err = vm.op_input().unwrap_err();
        assert!(matches!(err, VMError::MalformedCellEncoding));
    }

    #[test]
    fn input_in_internal_context_errors_external_only() {
        // Drive `0x90` through `step_internal` — dispatch must surface
        // `ExternalOnly` because internal transactions cannot consume
        // Utreexo entries.
        let mut vm = vm_with_script(vec![0x90]);
        // Even with a well-formed string on the stack, internal context
        // rejects the opcode before any decoding happens.
        let cell_bytes = encode_cell_to_bytes(&fixture_cell());
        vm.push_value(Value::String(crate::String::from(cell_bytes)));
        let err = vm.step_internal().unwrap_err();
        assert!(matches!(err, VMError::ExternalOnly));
    }

    #[test]
    fn input_then_output_anchor_chain() {
        // Round-trip: input a cell, then output a new cell whose anchor
        // is derived from the consumed cell. Confirms `last_anchor` is
        // wired through `input` so a subsequent `output` doesn't need an
        // external seed.
        let cell = fixture_cell();
        let bytes = encode_cell_to_bytes(&cell);
        let expected_anchor_after_input = cell.to_anchor();

        let mut vm = vm_external_with_script(Vec::new());

        // Step 1: feed cell bytes into op_input.
        vm.push_value(Value::String(crate::String::from(bytes)));
        vm.op_input().expect("input ok");
        // Stack: [Cell]. last_anchor: Some(ratcheted anchor from input).
        assert_eq!(
            vm.last_anchor.expect("anchor").0,
            expected_anchor_after_input.0
        );

        // The consumed cell's handle is still on the stack. For a stand-alone
        // anchor-chain test we don't care about authorizing it — drop it
        // directly so we can exercise `op_output` against the seeded anchor.
        let _consumed = vm.pop_cell().expect("pop cell handle");

        // Step 2: build an output through the real op_output handler.
        // Stack pre-output: [payload(5), count(1), predicate(Point)].
        vm.push_value(Value::Int253(Int253::from(5u64)));
        vm.push_value(Value::Int253(Int253::from(1u64)));
        vm.push_value(Value::Point(Point::from_bytes([0xbb; 32])));
        vm.op_output().expect("output ok");

        // Txlog now has: Input(consumed_id), Output(new_cell).
        assert_eq!(vm.txlog.len(), 2);
        match &vm.txlog[0] {
            crate::tx::TxEntry::Input(_) => {}
            _ => panic!("first entry must be Input"),
        }
        match &vm.txlog[1] {
            crate::tx::TxEntry::Output(_) => {}
            _ => panic!("second entry must be Output"),
        }
        // last_anchor advanced again past the output cell.
        assert_ne!(
            vm.last_anchor.expect("anchor").0,
            expected_anchor_after_input.0
        );
    }

    #[test]
    fn input_via_step_external_dispatch() {
        // Build a one-byte external script `[0x90]` and dispatch a single
        // step through `step_external` to confirm 0x90 routes to op_input.
        // Use a stub delegate that never actually runs (we only step once,
        // and the input opcode does not consult the delegate).
        let cell = fixture_cell();
        let expected_id = cell.id();
        let bytes = encode_cell_to_bytes(&cell);

        let mut vm = vm_external_with_script(vec![0x90]);
        vm.push_value(Value::String(crate::String::from(bytes)));

        let mut delegate = StubDelegate::new();
        let cont = vm.step_external(&mut delegate).expect("step ok");
        assert!(cont, "still running (script not exhausted)");

        // Stack now has the decoded cell; txlog has the Input entry.
        match &vm.current_call.stack[0] {
            Value::Cell(c) => assert_eq!(c.id(), expected_id),
            other => panic!("expected Cell, got {}", value_kind(other)),
        }
        assert_eq!(vm.txlog.len(), 1);
        match &vm.txlog[0] {
            crate::tx::TxEntry::Input(id) => assert_eq!(*id, expected_id),
            _ => panic!("expected TxEntry::Input"),
        }
    }

    // ── Phase 10: end-to-end external-tx workflow ───────────────
    //
    // The tests below assemble small but complete external-tx programs
    // — input → authorize → output — and drive them through the full
    // `step_external` dispatch loop plus `Delegate::finalize`. They are
    // the first tests that exercise the VM's external-context API as a
    // unit and serve as ground truth for the Phase-10 cell life-cycle.

    /// Drives `script` through `step_external` to completion using a
    /// `StubDelegate`, returns the resulting VM (so the test can inspect
    /// txlog, deferred_sigs, last_anchor, etc.). Mirrors the body of
    /// `VM::execute_external` minus the `into_result()` consumption.
    fn run_external_workflow(script: Vec<u8>) -> VM {
        let mut vm = VM::new(
            dummy_header(),
            CallFrame::new(script, CallKind::ExternalRoot, 1_000_000, 0, 0),
        );
        let mut delegate = StubDelegate::new();
        while vm.step_external(&mut delegate).expect("step_external ok") {}
        // Finalize accepts whatever sigs we accumulated (stub does nothing).
        let sigs = std::mem::replace(&mut vm.deferred_sigs, Vec::new());
        // Keep a copy in the VM for the test to inspect.
        let sigs_copy: Vec<DeferredSig> = sigs.iter().cloned().collect();
        delegate.finalize(sigs).expect("finalize ok");
        vm.deferred_sigs = sigs_copy;
        vm
    }

    #[test]
    fn external_tx_one_input_one_output_via_signtx() {
        // ── Scenario ─────────────────────────────────────────────
        // A single external transaction consumes one cell (authorized
        // via signtx — the cell holder signs the whole tx via the
        // envelope) and emits a single fresh cell.
        //
        // Cell life-cycle traced end-to-end:
        //   bytes  → input  → cell on stack → signtx (deferred sig +
        //   payload poured) → drop payload → push fresh payload →
        //   output → TxEntry::Output → finalize.

        // 1. Construct the input cell, capture its identity, encode it.
        let input_cell = fixture_cell();
        let input_id = input_cell.id();
        let input_predicate_point =
            input_cell.predicate.to_point();
        let input_anchor_post = input_cell.to_anchor();
        let input_bytes = encode_cell_to_bytes(&input_cell);

        // 2. Assemble the script.
        //
        // Stack diagram (top of stack on the right):
        //   pushstr <bytes>       []                  → [String]
        //   input                 [String]            → [Cell]
        //   signtx                [Cell]              → [Int253(7), String, Int253(2)]
        //                          (payload + count poured; TxBound recorded)
        //   drop                  [..7, "hello", 2]   → [..7, "hello"]
        //   drop                  [..7, "hello"]      → [..7]
        //   drop                  [..7]               → []
        //   push:42               []                  → [Int253(42)]
        //   push:1                [Int253(42)]        → [Int253(42), Int253(1)]
        //   pushpoint <P_out>     [..1]               → [..1, Point]
        //   output                [..Point]           → []  (Output effect emitted)
        let mut script = Vec::new();
        push_string_bytes(&mut script, &input_bytes);
        script.push(0x90); // input
        script.push(0x98); // signtx
        script.push(0x1c); // drop count
        script.push(0x1c); // drop "hello"
        script.push(0x1c); // drop 7
        script.push(0x10); // pushint8 positive
        script.push(42);
        script.push(0x01); // count = 1
        let out_pred_bytes = [0xbb; 32];
        push_point_bytes(&mut script, &out_pred_bytes);
        script.push(0x92); // output

        // 3. Run through `step_external` to completion + finalize.
        let vm = run_external_workflow(script);

        // 4. Assertions on the final VM state.

        // 4a. Clean exit: stack must be empty.
        assert!(
            vm.current_call.stack.is_empty(),
            "leftover stack at end of tx: {:?}",
            vm.current_call.stack.len()
        );

        // 4b. Txlog has exactly two entries in order: Input(cell_in_id),
        //     Output(cell_out).
        assert_eq!(vm.txlog.len(), 2, "expected Input + Output txlog");
        match &vm.txlog[0] {
            crate::tx::TxEntry::Input(id) => assert_eq!(*id, input_id),
            _ => panic!("txlog[0] must be Input"),
        }
        let output_cell_anchor = match &vm.txlog[1] {
            crate::tx::TxEntry::Output(c) => {
                // Output payload was [Int253(42)].
                assert_eq!(c.payload.len(), 1);
                match &c.payload[0] {
                    Value::Int253(i) => assert_eq!(*i, Int253::from(42u64)),
                    _ => panic!("output payload[0] must be Int253(42)"),
                }
                // Output predicate is the point we pushed.
                assert_eq!(
                    c.predicate.to_point().as_bytes(),
                    &out_pred_bytes
                );
                c.anchor
            }
            _ => panic!("txlog[1] must be Output"),
        };

        // 4c. Anchor chain: the output's anchor is the post-input anchor
        //     (i.e. cell_in.to_anchor()), since no other cell was created
        //     between input and output.
        assert_eq!(output_cell_anchor.0, input_anchor_post.0);

        // 4d. Deferred sigs: exactly one TxBound entry, with verification
        //     key matching the input cell's predicate point.
        assert_eq!(vm.deferred_sigs.len(), 1);
        match &vm.deferred_sigs[0] {
            DeferredSig::TxBound { verification_key } => {
                assert_eq!(
                    verification_key.as_bytes(),
                    input_predicate_point.as_bytes()
                );
            }
            DeferredSig::Explicit { .. } => {
                panic!("expected TxBound, got Explicit")
            }
        }

        // 4e. last_anchor has advanced past the output cell's own
        //     ratcheted anchor (so a hypothetical subsequent output
        //     would land at a different anchor).
        assert!(vm.last_anchor.is_some());
        assert_ne!(vm.last_anchor.unwrap().0, output_cell_anchor.0);
    }

    #[test]
    fn external_tx_two_inputs_two_outputs_via_open() {
        // ── Scenario ─────────────────────────────────────────────
        // External tx consumes two distinct cells via `open` (each
        // unlocked by a valid Taproot CallProof against its predicate
        // tree), then emits two fresh output cells. No `signtx` /
        // `signrun` here, so `deferred_sigs` stays empty.
        //
        // Each input cell's program is `drop` — it consumes the single
        // payload item the cell-open pours onto the stack.

        let prog = vec![0x1c]; // drop

        // ── Cell 1 ────────────────────────────────────────────────
        let (tree1, cp1) = build_predicate_with_program(&prog, 11);
        let cell1 = Cell::new(
            Predicate::Opaque(tree1.compute_point()),
            Anchor([0xa1; 32]),
            vec![Value::Int253(Int253::from(11u64))],
        );
        let cell1_id = cell1.id();
        let cell1_bytes = encode_cell_to_bytes(&cell1);

        // ── Cell 2 ────────────────────────────────────────────────
        let (tree2, cp2) = build_predicate_with_program(&prog, 22);
        let cell2 = Cell::new(
            Predicate::Opaque(tree2.compute_point()),
            Anchor([0xa2; 32]),
            vec![Value::Int253(Int253::from(22u64))],
        );
        let cell2_id = cell2.id();
        let cell2_anchor_post = cell2.to_anchor();
        let cell2_bytes = encode_cell_to_bytes(&cell2);

        // ── Script ────────────────────────────────────────────────
        //
        //   ┌─── consume cell 1 ─────────────────────────────────┐
        //   │ pushstr <cell1_bytes>                              │
        //   │ input                — pops String → pushes Cell1  │
        //   │ <callproof1 pieces>                                │
        //   │ push:0               — k = 0 args                  │
        //   │ open                 — verifies cp1, pours [11],   │
        //   │                       enters Run over `drop`;      │
        //   │                       inner Run pops the 11        │
        //   └────────────────────────────────────────────────────┘
        //   ┌─── consume cell 2 ─────────────────────────────────┐
        //   │ pushstr <cell2_bytes>                              │
        //   │ input                                              │
        //   │ <callproof2 pieces>                                │
        //   │ push:0                                             │
        //   │ open                                               │
        //   └────────────────────────────────────────────────────┘
        //   ┌─── emit output 1 ──────────────────────────────────┐
        //   │ push:9   push:1   pushpoint <P_out1>   output      │
        //   └────────────────────────────────────────────────────┘
        //   ┌─── emit output 2 ──────────────────────────────────┐
        //   │ push:10  push:1   pushpoint <P_out2>   output      │
        //   └────────────────────────────────────────────────────┘
        let mut script = Vec::new();

        // Consume cell 1
        push_string_bytes(&mut script, &cell1_bytes);
        script.push(0x90); // input
        push_callproof_pieces(&mut script, &cp1);
        script.push(0x00); // k = 0 args
        script.push(0x93); // open

        // Consume cell 2
        push_string_bytes(&mut script, &cell2_bytes);
        script.push(0x90); // input
        push_callproof_pieces(&mut script, &cp2);
        script.push(0x00); // k = 0 args
        script.push(0x93); // open

        // Emit output 1
        script.push(0x09); // push:9
        script.push(0x01); // count = 1
        let out1_pred_bytes = [0xc1; 32];
        push_point_bytes(&mut script, &out1_pred_bytes);
        script.push(0x92); // output

        // Emit output 2
        script.push(0x0a); // push:10
        script.push(0x01); // count = 1
        let out2_pred_bytes = [0xc2; 32];
        push_point_bytes(&mut script, &out2_pred_bytes);
        script.push(0x92); // output

        // ── Run + assert ─────────────────────────────────────────
        let vm = run_external_workflow(script);

        // Clean stack.
        assert!(vm.current_call.stack.is_empty());

        // Txlog: 2 × Input, 2 × Output, in that order.
        assert_eq!(vm.txlog.len(), 4, "expected 2 inputs + 2 outputs");
        match &vm.txlog[0] {
            crate::tx::TxEntry::Input(id) => assert_eq!(*id, cell1_id),
            _ => panic!("txlog[0] must be Input(cell1)"),
        }
        match &vm.txlog[1] {
            crate::tx::TxEntry::Input(id) => assert_eq!(*id, cell2_id),
            _ => panic!("txlog[1] must be Input(cell2)"),
        }
        let (out1, out2) = match (&vm.txlog[2], &vm.txlog[3]) {
            (
                crate::tx::TxEntry::Output(o1),
                crate::tx::TxEntry::Output(o2),
            ) => (o1, o2),
            _ => panic!("txlog[2..4] must be Output entries"),
        };

        // Output 1's payload is [Int253(9)], predicate matches what we
        // pushed.
        assert_eq!(out1.payload.len(), 1);
        match &out1.payload[0] {
            Value::Int253(i) => assert_eq!(*i, Int253::from(9u64)),
            _ => panic!("out1.payload[0] must be Int253(9)"),
        }
        assert_eq!(out1.predicate.to_point().as_bytes(), &out1_pred_bytes);
        assert_eq!(out2.payload.len(), 1);
        match &out2.payload[0] {
            Value::Int253(i) => assert_eq!(*i, Int253::from(10u64)),
            _ => panic!("out2.payload[0] must be Int253(10)"),
        }
        assert_eq!(out2.predicate.to_point().as_bytes(), &out2_pred_bytes);

        // Anchor chain:
        //   - cell1 input ratchets last_anchor → cell1.to_anchor()
        //   - cell2 input overwrites last_anchor → cell2.to_anchor()
        //   - output1 consumes last_anchor → out1.anchor == cell2.to_anchor()
        //   - output1 ratchets → out1.to_anchor()
        //   - output2 consumes last_anchor → out2.anchor == out1.to_anchor()
        //   - output2 ratchets → final last_anchor
        assert_eq!(out1.anchor.0, cell2_anchor_post.0);
        assert_eq!(out2.anchor.0, out1.to_anchor().0);
        assert_ne!(out1.anchor.0, out2.anchor.0);
        let final_anchor = vm.last_anchor.expect("anchor set after output 2");
        assert_eq!(final_anchor.0, out2.to_anchor().0);

        // No `signtx` / `signrun` were used → no deferred sigs.
        assert!(
            vm.deferred_sigs.is_empty(),
            "open does not record deferred sigs"
        );
    }

    /// Test-only `Delegate` impl backed by `r1cs::Verifier`. None of its
    /// methods are exercised by the Phase-10 tests; it exists purely so
    /// `step_external` is satisfiable.
    struct StubDelegate {
        cs: bulletproofs::r1cs::Verifier<merlin::Transcript>,
    }

    impl StubDelegate {
        fn new() -> Self {
            Self {
                cs: bulletproofs::r1cs::Verifier::new(
                    merlin::Transcript::new(b"flamevm.test.stub"),
                ),
            }
        }
    }

    impl Delegate for StubDelegate {
        type CS = bulletproofs::r1cs::Verifier<merlin::Transcript>;

        fn cs(&mut self) -> &mut Self::CS {
            &mut self.cs
        }

        fn commit_variable(
            &mut self,
            _commitment: &crate::Commitment,
        ) -> Result<(CompressedRistretto, bulletproofs::r1cs::Variable), VMError> {
            unreachable!("StubDelegate::commit_variable should not be called in Phase-10 tests");
        }

        fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
            // Stub: don't actually verify a proof.
            Ok(())
        }
    }

    // ── Phase 11: Prover/Verifier end-to-end ─────────────────────

    use crate::program::Program;
    use crate::{Prover, Verifier};
    use bulletproofs::PedersenGens;

    #[test]
    fn instruction_alloc_witness_roundtrip() {
        // Alloc(Some(7)) encodes to exactly one byte; its witness is
        // tracked separately via the queue.
        let p = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(None)
            .alloc(Some(Int253::from(3u64)));
        let bytecode = p.to_bytecode();
        assert_eq!(bytecode, vec![0x5c, 0x5c, 0x5c]);
        let witnesses: Vec<_> = p.to_witnesses().into();
        assert_eq!(witnesses.len(), 3);
        assert!(matches!(witnesses[0], Some(_)));
        assert!(matches!(witnesses[1], None));
        assert!(matches!(witnesses[2], Some(_)));
    }

    #[test]
    fn program_builder_emits_expected_bytecode() {
        // alloc(7) alloc(3) add alloc(10) eq verify
        let p = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(3u64)))
            .add()
            .alloc(Some(Int253::from(10u64)))
            .eq()
            .verify();
        assert_eq!(
            p.to_bytecode(),
            vec![0x5c, 0x5c, 0x53, 0x5c, 0x51, 0x79]
        );
        let wits: Vec<_> = p.to_witnesses().into();
        assert_eq!(wits.len(), 3);
    }

    /// End-to-end Phase 11: prove `alloc(7) + alloc(3) == alloc(10)`
    /// then verify the proof. This is the bootstrap milestone — once
    /// this works, all later CS-touching opcodes wire onto the same
    /// machinery.
    #[test]
    fn prove_then_verify_alloc_arithmetic_equality() {
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(3u64)))
            .add()
            .alloc(Some(Int253::from(10u64)))
            .eq()
            .verify();

        let (bytecode, proof, _result, _sigs) = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("prove succeeds");

        // Verifier walks the same bytecode and accepts the proof.
        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn prove_succeeds_but_verify_fails_on_tampered_proof() {
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(3u64)))
            .add()
            .alloc(Some(Int253::from(10u64)))
            .eq()
            .verify();
        let (bytecode, proof, _, _) = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("prove succeeds");

        // Flip a byte deep in the proof body.
        let mut proof_bytes = proof.to_bytes();
        let last = proof_bytes.len() - 1;
        proof_bytes[last] ^= 0x01;
        let tampered = bulletproofs::r1cs::R1CSProof::from_bytes(&proof_bytes)
            .expect("re-parses");

        let pc_gens_v = PedersenGens::default();
        let err = Verifier::verify(
            &pc_gens_v,
            bytecode,
            &tampered,
            dummy_header(),
            1_000_000,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, VMError::InvalidR1CSProof));
    }

    #[test]
    fn prove_fails_for_unsatisfiable_equality() {
        // alloc(7) + alloc(3) == alloc(99) — constraint is false at
        // witness level. Bulletproofs' Prover happily emits a proof
        // (the constraint is unsatisfiable but the prover constructs
        // *something*); the verifier MUST reject.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(3u64)))
            .add()
            .alloc(Some(Int253::from(99u64)))
            .eq()
            .verify();
        let (bytecode, proof, _, _) = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("prover doesn't refuse construction");

        let pc_gens_v = PedersenGens::default();
        let err = Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, VMError::InvalidR1CSProof));
    }

    #[test]
    fn alloc_pushes_expression_with_witness() {
        // Build a single-alloc program and stop after the alloc to
        // inspect the produced Expression.
        let pc_gens = PedersenGens::default();
        let program = Program::new().alloc(Some(Int253::from(42u64)));
        let mut prover = Prover::new(&pc_gens);
        // We bypass the public `Prover::prove` so we can inspect VM
        // state mid-flight. Build a Run::Queue from the Program so the
        // Alloc instruction's witness survives dispatch.
        let kind = CallKind::ExternalRoot;
        let mut vm = VM::new(
            dummy_header(),
            CallFrame::new_with_run(
                Run::from_program(program),
                kind,
                1_000_000,
                0,
                0,
            ),
        );
        // One step → executes the alloc.
        vm.step_external(&mut prover).expect("alloc step ok");

        assert_eq!(vm.current_call.stack.len(), 1);
        match &vm.current_call.stack[0] {
            Value::Expression(crate::Expression::LinearCombination(terms, witness)) => {
                assert_eq!(terms.len(), 1);
                assert_eq!(*witness, Some(Int253::from(42u64)));
            }
            _ => panic!("expected Expression with witness"),
        }
    }

    #[test]
    fn prove_then_verify_alloc_multiplication() {
        // alloc(4) alloc(5) mul alloc(20) eq verify
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(4u64)))
            .alloc(Some(Int253::from(5u64)))
            .mul()
            .alloc(Some(Int253::from(20u64)))
            .eq()
            .verify();
        let (bytecode, proof, _, _) = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("prove succeeds");

        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn prove_then_verify_alloc_with_negation() {
        // alloc(5) neg alloc(-5) eq verify  →  -5 == -5
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(5u64)))
            .neg()
            .alloc(Some(Int253::from(-5i64)))
            .eq()
            .verify();
        let (bytecode, proof, _, _) = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("prove succeeds");

        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn alloc_without_witness_works_in_verifier_path() {
        // Verifier feeds bytecode that contains an alloc — the
        // verifier's `next_alloc_witness` returns None, so the variable
        // is allocated without an assignment. We can't verify a proof
        // here (the prover has a witness), but we can check the path
        // doesn't error before proof verification.
        //
        // Build a trivially-true constraint: alloc * 0 == 0.
        // Concrete sub-test: just confirm the verifier walks an alloc
        // opcode without erroring on the witness-missing path.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(0u64)))
            .alloc(Some(Int253::from(0u64)))
            .eq()
            .verify();
        let (bytecode, proof, _, _) = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("prove succeeds");

        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    // ── Phase 12: range proofs + Constraint composition ──────────

    #[test]
    fn range_proof_accepts_in_range_value() {
        // alloc(42) push:64 range — 42 fits in 64 bits.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(42u64)))
            .push_int(64u64)
            .range()
            // Constrain that the same alloc equals 42 to close the proof
            // with a non-trivial constraint (so verification has
            // something to check beyond the range gadget).
            .alloc(Some(Int253::from(42u64)))
            .eq()
            .verify();
        let (bytecode, proof, _, _) =
            Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
                .expect("prove succeeds");
        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn range_proof_rejects_out_of_range_value() {
        // alloc(2^9) push:8 range — 512 does NOT fit in 8 bits, so the
        // prover-side range_proof gadget rejects the witness or the
        // verifier rejects the proof.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(512u64)))
            .push_int(8u64)
            .range()
            .alloc(Some(Int253::from(512u64)))
            .eq()
            .verify();
        let result = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        );
        // The prover may succeed (constructs a proof with bad witness)
        // and the verifier rejects, OR the prover errors directly.
        // Either way, the full pipeline must reject. Cover both
        // outcomes for robustness.
        match result {
            Err(_) => {
                // Prover refused — good.
            }
            Ok((bytecode, proof, _, _)) => {
                let pc_gens_v = PedersenGens::default();
                let err = Verifier::verify(
                    &pc_gens_v,
                    bytecode,
                    &proof,
                    dummy_header(),
                    1_000_000,
                    0,
                )
                .expect_err("verifier must reject out-of-range proof");
                assert!(matches!(err, VMError::InvalidR1CSProof));
            }
        }
    }

    #[test]
    fn range_proof_constant_in_range_skips_cs() {
        // push:7 (constant Expression after no alloc), but we don't
        // have a way to get an Expression::Constant onto the stack
        // without `scalar` (Phase 13). Skip this until Phase 13.
        //
        // For now exercise `range` only via alloc-produced Expressions.
    }

    #[test]
    fn range_bit_count_zero_rejected() {
        // push:0 — zero-bit range proof is degenerate, rejected at the
        // opcode level.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(0u64)))
            .push_int(0u64)
            .range()
            .alloc(Some(Int253::from(0u64)))
            .eq()
            .verify();
        let err = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, VMError::BitCountOutOfRange));
    }

    #[test]
    fn range_bit_count_above_64_rejected() {
        // push:65 — bit count exceeds BitRange::max() (64).
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(1u64)))
            .push_int(65u64)
            .range()
            .alloc(Some(Int253::from(1u64)))
            .eq()
            .verify();
        let err = Prover::prove(
            &pc_gens,
            program,
            dummy_header(),
            1_000_000,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, VMError::BitCountOutOfRange));
    }

    #[test]
    fn constraint_and_overload_combines_two_constraints() {
        // (alloc(7) == alloc(7)) AND (alloc(3) == alloc(3))
        //   → Constraint composition — verify succeeds (both true).
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            // Constraint 1: alloc(7) == alloc(7) — pushes Constraint
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(7u64)))
            .eq()
            // Constraint 2: alloc(3) == alloc(3) — pushes Constraint
            .alloc(Some(Int253::from(3u64)))
            .alloc(Some(Int253::from(3u64)))
            .eq()
            // AND the two Constraints
            .and()
            .verify();
        let (bytecode, proof, _, _) =
            Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
                .expect("prove succeeds");
        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn constraint_or_overload_combines_two_constraints() {
        // (alloc(7) == alloc(8)) OR (alloc(3) == alloc(3))
        //   → first is false, second is true; OR yields true. Verify ok.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(8u64)))
            .eq()
            .alloc(Some(Int253::from(3u64)))
            .alloc(Some(Int253::from(3u64)))
            .eq()
            .or()
            .verify();
        let (bytecode, proof, _, _) =
            Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
                .expect("prove succeeds");
        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn constraint_not_overload_negates_constraint() {
        // NOT (alloc(7) == alloc(8))  → NOT false → true.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(8u64)))
            .eq()
            .not()
            .verify();
        let (bytecode, proof, _, _) =
            Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
                .expect("prove succeeds");
        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn constraint_and_with_false_branch_rejected() {
        // (alloc(7) == alloc(7)) AND (alloc(3) == alloc(99))
        //   → first true, second false; AND is false. Verifier rejects.
        let pc_gens = PedersenGens::default();
        let program = Program::new()
            .alloc(Some(Int253::from(7u64)))
            .alloc(Some(Int253::from(7u64)))
            .eq()
            .alloc(Some(Int253::from(3u64)))
            .alloc(Some(Int253::from(99u64)))
            .eq()
            .and()
            .verify();
        let (bytecode, proof, _, _) =
            Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
                .expect("prove succeeds (constructs proof of unsatisfiable constraint)");
        let pc_gens_v = PedersenGens::default();
        let err = Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, VMError::InvalidR1CSProof));
    }

    #[test]
    fn dispatch_falls_through_to_int_path_when_no_constraint_on_top() {
        // Pure Int253 path for `and` — must NOT route to Constraint
        // overload when both operands are Int253. push:1 push:1 and
        // → push:1.
        let mut vm = vm_with_script(vec![0x01, 0x01, 0x58]); // push:1, push:1, and
        run_to_end(&mut vm).expect("int and ok");
        assert_eq!(vm.current_call.stack.len(), 1);
        assert_int(&vm.current_call.stack[0], Int253::from(1u64));
    }

    #[test]
    fn instruction_range_roundtrip() {
        let mut buf = Vec::new();
        crate::ops::Instruction::Range.encode(&mut buf);
        assert_eq!(buf, vec![0x5e]);
        let mut r: &[u8] = &buf;
        let parsed = crate::ops::Instruction::parse(&mut r).expect("parses");
        assert!(matches!(parsed, crate::ops::Instruction::Range));
    }

    #[test]
    fn range_in_internal_context_errors_external_only() {
        // Internal context dispatches `range` to ExternalOnly.
        let mut vm = vm_with_script(vec![
            0x10, 0x01, // pushint8(1)
            0x10, 0x40, // pushint8(64)
            0x5e, // range
        ]);
        // Push an Expression manually so dispatch_internal hits range.
        // Actually we can't construct an Expression in internal context
        // (alloc is ExternalOnly too). The simpler test: just step until
        // the `range` opcode is dispatched — it should error ExternalOnly
        // before consuming any stack operands.
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::ExternalOnly));
    }

    // ── Phase 13: rich String + scalar / commit / decrypt ────────

    #[test]
    fn string_witness_commitment_encodes_to_point() {
        // String::Commitment(witness) serializes to the 32-byte
        // compressed Pedersen point — identical to what an Opaque
        // String wrapping the same bytes would yield.
        let c = crate::Commitment::unblinded(Int253::from(42u64));
        let point_bytes = c.to_point().as_bytes().to_vec();
        let s = String::commitment(c);
        assert_eq!(s.to_bytes_vec(), point_bytes);
        assert_eq!(s.len(), 32);
    }

    #[test]
    fn string_witness_commitment_downcasts() {
        let c = crate::Commitment::unblinded(Int253::from(42u64));
        let s = String::commitment(c.clone());
        let recovered = s.to_commitment().expect("downcast");
        // The Open commitment is preserved on the witness-bearing path
        // (not collapsed to Closed).
        assert!(matches!(recovered, crate::Commitment::Open(_)));
        assert_eq!(recovered.assignment(), Some(Int253::from(42u64)));
    }

    #[test]
    fn string_opaque_downcast_to_commitment_gives_closed() {
        // 32 bytes of opaque data → Commitment::Closed(point).
        let c = crate::Commitment::unblinded(Int253::from(42u64));
        let opaque = String::from(c.to_point().as_bytes().to_vec());
        let recovered = opaque.to_commitment().expect("downcast");
        assert!(matches!(recovered, crate::Commitment::Closed(_)));
        assert_eq!(recovered.to_point(), c.to_point());
    }

    #[test]
    fn string_scalar_downcast() {
        let i = Int253::from(123u64);
        let s = String::scalar(i);
        let recovered = s.to_scalar().expect("downcast");
        assert_eq!(recovered, i);
    }

    #[test]
    fn op_scalar_pushes_constant_expression() {
        // Pre-load a 32-byte String on the stack, dispatch `scalar`,
        // confirm the result is Expression::Constant.
        let mut vm = vm_external_with_script(vec![0x5a]); // scalar opcode
        let s = String::scalar(Int253::from(99u64));
        vm.push_value(Value::String(s));
        let mut delegate = StubDelegate::new();
        vm.step_external(&mut delegate).expect("scalar ok");
        assert_eq!(vm.current_call.stack.len(), 1);
        match &vm.current_call.stack[0] {
            Value::Expression(crate::Expression::Constant(i)) => {
                assert_eq!(*i, Int253::from(99u64));
            }
            _ => panic!("expected Expression::Constant"),
        }
    }

    #[test]
    fn op_commit_pushes_variable() {
        // Pre-load a witness-bearing String::Commitment, dispatch
        // `commit`, confirm the result is a Variable with the open
        // commitment preserved.
        let mut vm = vm_external_with_script(vec![0x5b]); // commit opcode
        let c = crate::Commitment::unblinded(Int253::from(42u64));
        vm.push_value(Value::String(String::commitment(c.clone())));
        let mut delegate = StubDelegate::new();
        vm.step_external(&mut delegate).expect("commit ok");
        assert_eq!(vm.current_call.stack.len(), 1);
        match &vm.current_call.stack[0] {
            Value::Variable(v) => {
                assert_eq!(v.commitment.assignment(), Some(Int253::from(42u64)));
            }
            _ => panic!("expected Variable"),
        }
    }

    #[test]
    fn prove_then_verify_with_commit_expr_eq() {
        // pushstr <open commitment witness> ; commit ; expr ;
        // alloc(42) ; eq ; verify.
        // Both the commit-side and alloc-side Expressions point to
        // value 42 → eq holds → verify succeeds.
        let pc_gens = PedersenGens::default();
        let witness_int = Int253::from(42u64);
        // Use a blinding factor that we'll need to encode into the
        // Program as a witness-bearing String.
        let blinding = curve25519_dalek::scalar::Scalar::from(7u64);
        let c = crate::Commitment::blinded_with_factor(witness_int, blinding);
        let program = Program::new()
            // Push the witness-bearing Commitment String. The bytecode
            // will encode it as 32 bytes (the point); the prover's
            // Run::Queue preserves the witness; the verifier walks
            // bytecode and sees String::Opaque, which downcasts to
            // Commitment::Closed(point) — sufficient for the CS to
            // bind to the same point the prover used.
            .push_str(String::commitment(c))
            .commit()
            .expr()
            .alloc(Some(witness_int))
            .eq()
            .verify();
        let (bytecode, proof, _, _) =
            Prover::prove(&pc_gens, program, dummy_header(), 1_000_000, 0)
                .expect("prove succeeds");
        let pc_gens_v = PedersenGens::default();
        Verifier::verify(
            &pc_gens_v,
            bytecode,
            &proof,
            dummy_header(),
            1_000_000,
            0,
        )
        .expect("verify succeeds");
    }

    #[test]
    fn op_decrypt_succeeds_on_matching_witness() {
        // Build a Token from cleartext (q, f); decrypt with the
        // correct (q, f, q', f') quartet succeeds and pushes
        // ClearToken(q, f).
        let q = Int253::from(100u64);
        let f = Int253::from(7u64);
        let q_blind = Int253::from(11u64);
        let f_blind = Int253::from(13u64);
        let qty_commit = crate::Commitment::blinded_with_factor(
            q,
            curve25519_dalek::scalar::Scalar::from(11u64),
        );
        let flv_commit = crate::Commitment::blinded_with_factor(
            f,
            curve25519_dalek::scalar::Scalar::from(13u64),
        );
        let token = crate::Token::new(qty_commit, flv_commit);

        let mut vm = vm_external_with_script(vec![0x77]); // decrypt
        vm.push_value(Value::Token(token));
        vm.push_value(Value::Int253(f));
        vm.push_value(Value::Int253(f_blind));
        vm.push_value(Value::Int253(q));
        vm.push_value(Value::Int253(q_blind));
        let mut delegate = StubDelegate::new();
        vm.step_external(&mut delegate).expect("decrypt ok");

        assert_eq!(vm.current_call.stack.len(), 1);
        match &vm.current_call.stack[0] {
            Value::ClearToken(ct) => {
                assert_eq!(ct.qty(), q);
                assert_eq!(ct.flv(), f);
            }
            _ => panic!("expected ClearToken"),
        }
    }

    #[test]
    fn op_decrypt_rejects_wrong_witness() {
        // Mismatched blinding → commitment opens to a different point
        // → CleartextConstraintFalse.
        let q = Int253::from(100u64);
        let f = Int253::from(7u64);
        let qty_commit = crate::Commitment::blinded_with_factor(
            q,
            curve25519_dalek::scalar::Scalar::from(11u64),
        );
        let flv_commit = crate::Commitment::blinded_with_factor(
            f,
            curve25519_dalek::scalar::Scalar::from(13u64),
        );
        let token = crate::Token::new(qty_commit, flv_commit);
        let mut vm = vm_external_with_script(vec![0x77]);
        vm.push_value(Value::Token(token));
        vm.push_value(Value::Int253(f));
        vm.push_value(Value::Int253(Int253::from(99u64))); // wrong f_blind
        vm.push_value(Value::Int253(q));
        vm.push_value(Value::Int253(Int253::from(11u64)));
        let mut delegate = StubDelegate::new();
        let err = vm.step_external(&mut delegate).unwrap_err();
        assert!(matches!(err, VMError::CleartextConstraintFalse));
    }

    #[test]
    fn instruction_scalar_commit_decrypt_mix_roundtrip() {
        // Round-trip the new Phase-13 Instruction variants.
        use crate::ops::Instruction;
        for variant in [
            Instruction::Scalar,
            Instruction::Commit,
            Instruction::Decrypt,
            Instruction::Mix,
        ] {
            let mut buf = Vec::new();
            variant.encode(&mut buf);
            assert_eq!(buf.len(), 1);
            let mut r: &[u8] = &buf;
            let parsed = Instruction::parse(&mut r).expect("parses");
            assert_eq!(format!("{:?}", parsed), format!("{:?}", variant));
        }
    }

    #[test]
    fn scalar_in_internal_context_errors_external_only() {
        let mut vm = vm_with_script(vec![0x5a]);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::ExternalOnly));
    }

    #[test]
    fn commit_in_internal_context_errors_external_only() {
        let mut vm = vm_with_script(vec![0x5b]);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::ExternalOnly));
    }

    #[test]
    fn decrypt_in_internal_context_errors_external_only() {
        let mut vm = vm_with_script(vec![0x77]);
        let err = run_to_end(&mut vm).unwrap_err();
        assert!(matches!(err, VMError::ExternalOnly));
    }
}
