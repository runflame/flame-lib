//! FlameVM execution engine: Tx → CallFrame → Run nesting + dispatch loop.

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

// Re-export from canonical homes so vm.rs callers (notably the
// test helpers, which inherit `super::super::*`) keep their
// existing import shape. The types themselves live in `actor.rs`
// and `send.rs`; vm.rs just plumbs them.
pub use crate::actor::{ActorID, ActorRegistry, ActorState};
pub use crate::send::Message;

/// 32-byte anchor. Chained via `ratchet` to make outputs unique
/// within a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor(pub [u8; 32]);

impl Anchor {
    /// Ratchets the anchor into a new anchor.
    pub fn ratchet(&self) -> Anchor {
        let mut t = Transcript::new(b"flamevm.anchor.ratchet.v1");
        t.append_message(b"prev", &self.0);
        let mut next = [0u8; 32];
        t.challenge_bytes(b"next", &mut next);
        Anchor(next)
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
    type CS: r1cs::RandomizableConstraintSystem;
    /// Per-side batched scalar-point check accumulator.
    type BatchVerifier: musig::BatchVerification;

    /// Returns the delegate's underlying constraint system.
    fn cs(&mut self) -> &mut Self::CS;

    /// Returns the delegate's batch verifier.
    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier;

    /// Adds a Commitment to the CS, producing a high-level variable.
    fn commit_variable(
        &mut self,
        commitment: &crate::Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError>;

    /// Consumes the delegate after VM execution finishes cleanly.
    /// Prover builds the proof; verifier checks it.
    fn finalize(self, deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError>;
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
    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        Ok(())
    }
}

/// One executable script slice: a decoded instruction stream plus a
/// cursor. Multiple Runs nest within one CallFrame (`run` / `loop` /
/// `switch`); each new Run pushes the old one onto `run_stack`.
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

    /// Returns the next instruction; `Ok(None)` at end of program.
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

    /// True iff the Run has reached its end. Test-only — production
    /// code drives the run to completion via the dispatch loop.
    #[cfg(test)]
    pub(crate) fn is_finished(&self) -> bool {
        self.cursor >= self.instructions.len()
    }

    /// Resets the cursor to the start of the Run. Used by `loop`.
    fn rewind(&mut self) {
        self.cursor = 0;
    }

    /// Jumps past the end of the Run. Used by `break:k`.
    fn jump_to_end(&mut self) {
        self.cursor = self.instructions.len();
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

/// Iterates over the actor ids of every live frame — current call
/// Walks actor ids of every live frame — current first, then suspended
/// innermost-out. Used by the re-entrancy guard inside `op_call`.
fn iter_actor_ids_on_stack<'a>(
    current: &'a CallFrame,
    suspended: &'a [CallFrame],
) -> impl Iterator<Item = &'a ActorID> {
    core::iter::once(&current.kind)
        .chain(suspended.iter().map(|f| &f.kind))
        .filter_map(|k| k.actor())
}

/// Hashes an `ActorState` to its canonical 32-byte root via the
/// `flamevm.actor.state.root` transcript domain. Used by `op_call`
/// to bind the callee's pre-call state into `TxEntry::Call`.
fn state_root(state: &ActorState) -> Result<[u8; 32], VMError> {
    let mut buf = Vec::new();
    state.encode(&mut buf).map_err(|_| VMError::MalformedActorState)?;
    let mut t = Transcript::new(b"flamevm.actor.state.root");
    t.append_message(b"state", &buf);
    let mut h = [0u8; 32];
    t.challenge_bytes(b"root", &mut h);
    Ok(h)
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

    /// Per-frame load/save pairing flag. Set by `op_load`, cleared by
    /// `op_save`. Unmatched load self-destructs the actor at tx commit.
    pub(crate) loaded: bool,
}

impl CallFrame {
    /// Builds a fresh CallFrame whose Run walks `instructions`.
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
            loaded: false,
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

    /// Outbound message sends recorded by `op_send`. Drained by
    /// the consensus layer after external-tx commit to instantiate
    /// each one as an internal transaction.
    pub sends: Vec<Message>,
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
            .field("sends.len", &self.sends.len())
            .finish()
    }
}

pub(crate) struct VM {
    #[allow(dead_code)]
    header: TxHeader,
    pub(crate) last_anchor: Option<Anchor>,

    gas_used: u64,
    vbytes_used: u64,

    current_call: CallFrame,
    call_stack: Vec<CallFrame>,

    /// Effects emitted during execution; used to compute TxID.
    pub(crate) txlog: Vec<crate::tx::TxEntry>,

    /// Running per-tx fee accumulator (overflow → `FeeTooHigh`).
    total_fee: crate::fees::CheckedFee,

    /// Signature checks deferred to `Delegate::finalize`.
    deferred_sigs: Vec<DeferredSig>,

    /// Outbound messages queued by `op_send`.
    sends: Vec<Message>,
}

impl VM {
    /// Executes an external transaction script with the given delegate,
    /// then calls `delegate.finalize`.
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
        let sigs = mem::take(&mut vm.deferred_sigs);
        delegate.finalize(sigs.clone())?;
        vm.deferred_sigs = sigs;
        Ok(vm.into_result(bytecode, None))
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

    /// Executes an internal transaction. On clean exit runs the tx-end
    /// self-destruct sweep against `registry`.
    pub fn execute_internal(
        header: TxHeader,
        message: Message,
        registry: &mut dyn ActorRegistry,
        block: &BlockContext,
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
        while vm.step_internal_with_registry(registry)? {}
        let _cleared = registry.commit_tx_destructions(block.height);
        Ok(vm.into_result(Vec::new(), None))
    }

    fn new(header: TxHeader, initial_call: CallFrame) -> Self {
        // Header is the first txlog entry so TxID binds to version + locktime.
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
            sends: Vec::new(),
        }
    }

    /// Drains the VM into a `TxResult`, computing TxID from the txlog.
    fn into_result(
        mut self,
        bytecode: Vec<u8>,
        proof: Option<R1CSProof>,
    ) -> TxResult {
        let txlog = mem::take(&mut self.txlog);
        let deferred_sigs = mem::take(&mut self.deferred_sigs);
        let sends = mem::take(&mut self.sends);
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
            sends,
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
    /// `Ok(false)` to stop.
    fn step<D: Delegate>(
        &mut self,
        delegate: &mut D,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<bool, VMError> {
        let Some(instr) = self.current_call.current_run.next_instruction()? else {
            return self.finish_run();
        };
        use crate::ops::Instruction as I;
        match instr {

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

            I::Merlin => self.op_merlin(),
            I::MerlinWrite => self.op_merlin_write(),
            I::MerlinRead => self.op_merlin_read(),
            I::Sha256 => self.op_sha256(),
            I::Sha512 => self.op_sha512(),
            I::Sha3 => self.op_sha3(),
            I::Log => self.op_log(),

            I::Amount => self.op_amount(),
            I::Issue => self.op_issue(),
            I::Retire => self.op_retire(),
            I::Borrow => self.op_borrow(delegate),
            I::Merge => self.op_merge(),
            I::Split => self.op_split(),
            I::IssueFlv => self.op_issueflv(),

            I::Alloc(w) => self.op_alloc(w, delegate),
            I::Expr => self.op_expr(delegate),
            I::Range => self.op_range(delegate),
            I::Scalar => self.op_scalar(),
            I::Commit => self.op_commit(),
            I::Decrypt => self.op_decrypt(),
            I::Mix => self.op_mix(delegate),
            I::Fee => self.op_fee(delegate),
            I::Verify => self.op_verify(delegate),

            I::Run => self.op_run(),
            I::Loop => self.op_loop(),
            I::Switch => self.op_switch(),
            I::Return => self.op_return(),
            I::Type => self.op_type(),
            I::BreakK(k) => self.op_break_k(k as usize),

            I::Input => self.op_input(),
            I::Cell => self.op_cell(),
            I::Output => self.op_output(),
            I::Open => self.op_open(),
            I::Send => self.op_send(),
            I::Call => self.op_call(registry),
            I::Load => self.op_load(registry),
            I::Save => self.op_save(registry),
            I::Signtx => self.op_signtx(),
            I::Signcall => self.op_signcall(),

            I::Actorid => self.op_actorid(),
            I::Anchor => self.op_anchor(),
            I::Callerid => self.op_callerid(),
            I::Method => self.op_method(),

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

    /// Pops the current frame back to its caller on clean exit. Stack
    /// must be empty (use `return k` to send values across the boundary).
    /// Leftover gas is refunded to the parent.
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

    /// `0x69` `merlin` — `label → merlin`. Pops a label string, creates
    /// a fresh transcript bound to it.
    fn op_merlin(&mut self) -> Result<(), VMError> {
        let label = self.pop_value()?.to_string()?;
        self.push_value(Value::Merlin(Merlin::new(label.as_bytes())));
        Ok(())
    }

    /// `0x6a` `merlinwrite` — `merlin label str → merlin`. Pops `str`
    /// (top), `label`, and `merlin`; absorbs `(label, str)` into the
    /// transcript; pushes merlin back.
    fn op_merlin_write(&mut self) -> Result<(), VMError> {
        let data = self.pop_value()?.to_string()?;
        let label = self.pop_value()?.to_string()?;
        let mut m = self.pop_value()?.to_merlin()?;
        m.write_bytes(label.as_bytes(), data.as_bytes());
        self.push_value(Value::Merlin(m));
        Ok(())
    }

    /// `0x6b` `merlinread` — `merlin label n → merlin str`. Squeezes
    /// `n` bytes of challenge from the transcript under `label`; pushes
    /// the merlin back, then the new String.
    fn op_merlin_read(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
        let label = self.pop_value()?.to_string()?;
        let mut m = self.pop_value()?.to_merlin()?;
        let out = m.read_bytes(label.as_bytes(), n);
        self.push_value(Value::Merlin(m));
        self.push_value(Value::String(String::from(out)));
        Ok(())
    }

    /// `0x6c` `sha256` — pops a String, pushes the 32-byte SHA-256 digest.
    fn op_sha256(&mut self) -> Result<(), VMError> {
        use sha2::{Digest, Sha256};
        let s = self.pop_value()?.to_string()?;
        let digest = Sha256::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x6d` `sha512` — pops a String, pushes the 64-byte SHA-512 digest.
    fn op_sha512(&mut self) -> Result<(), VMError> {
        use sha2::{Digest, Sha512};
        let s = self.pop_value()?.to_string()?;
        let digest = Sha512::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x6e` `sha3` — pops a String, pushes the 32-byte SHA3-256 digest
    /// (FIPS-202; padding `0x06`).
    fn op_sha3(&mut self) -> Result<(), VMError> {
        use sha3::{Digest, Sha3_256};
        let s = self.pop_value()?.to_string()?;
        let digest = Sha3_256::digest(s.as_bytes());
        self.push_value(Value::String(String::from(digest.to_vec())));
        Ok(())
    }

    /// `0x4e` `keccak256` — pops a String, pushes the 32-byte Keccak-256
    /// digest (pre-FIPS Keccak; padding `0x01`). Distinct from `sha3` for
    /// Ethereum compatibility.
    fn op_keccak256(&mut self) -> Result<(), VMError> {
        use sha3::{Digest, Keccak256};
        let s = self.pop_value()?.to_string()?;
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
                self.push_value(Value::Int253(Int253::zero()));
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
        let k = self.pop_value()?.to_int253()?;
        let dict = self.pop_value()?.to_dict()?;
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
        let dict = self.pop_value()?.to_dict()?;
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
        let dict = self.pop_value()?.to_dict()?;
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
        let k = self.pop_value()?.to_int253()?;
        let dict = self.pop_value()?.to_dict()?;
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
        self.push_value(Value::Int253(Int253::zero()));
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
        let s = self.pop_value()?.to_string()?;
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
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&s.as_bytes()[..32]);
        let point = Point::from_bytes(arr);
        let (remainder, _consumed) = s.split_at(32).expect("length checked");
        self.push_value(Value::String(remainder));
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
        self.push_value(Value::String(s1.append(&s2)));
        Ok(())
    }

    /// `0x47` `writezeros` — `s n → s'`. Appends `n` zero bytes.
    fn op_write_zeros(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(usize::MAX)?;
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
        let two_have_non_int = !matches!(self.current_call.stack[n - 1], Value::Int253(_))
            || !matches!(self.current_call.stack[n - 2], Value::Int253(_));
        if self.is_external() && two_have_non_int {
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

    /// _x y_ **mul** → _z_  (cleartext modulo ℓ, or CS multiplier)
    fn op_mul<D: Delegate>(&mut self, delegate: &mut D) -> Result<(), VMError> {
        let b = self.pop_value()?;
        let a = self.pop_value()?;
        match (a, b) {
            (Value::Int253(x), Value::Int253(y)) => {
                self.push_value(Value::Int253(x * y));
                Ok(())
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

    /// _prog_ **run** → _results…_
    fn op_run(&mut self) -> Result<(), VMError> {
        let s = self.pop_value()?.to_string()?;
        let instrs = s.to_instructions()?;
        self.enter_run(instrs)
    }

    /// **loop** → ø — rewinds the current Run to the start.
    fn op_loop(&mut self) -> Result<(), VMError> {
        self.current_call.current_run.rewind();
        Ok(())
    }

    /// _x a b_ **switch** → enters `a` if `x != 0`, else `b`.
    fn op_switch(&mut self) -> Result<(), VMError> {
        let b = self.pop_value()?.to_string()?;
        let a = self.pop_value()?.to_string()?;
        let x = self.pop_value()?.to_int253()?;
        let chosen = if x.is_zero() { b } else { a };
        let instrs = chosen.to_instructions()?;
        self.enter_run(instrs)
    }

    /// _x(k-1) … x(0) k_ **return** → ø
    ///
    /// Pops the frame, refunds leftover gas, pushes the `k` items onto
    /// the caller's stack. Errors `ReturnAtRoot` at the outermost frame.
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

    /// Pops a 32-byte String and returns it as a fixed array. Used by
    /// opcodes that consume canonical-hash payloads (`send`, `call`).
    fn pop_string_32(&mut self) -> Result<[u8; 32], VMError> {
        let s = self.pop_value()?.to_string()?;
        if s.len() != 32 {
            return Err(VMError::MalformedAddress);
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&s.bytes_view());
        Ok(out)
    }

    /// Pushes the current Run onto the run-stack, switches to a
    /// fresh Run over `instructions`.
    fn enter_run(&mut self, instructions: Vec<crate::ops::Instruction>) -> Result<(), VMError> {
        let new_run = Run::new(instructions);
        let old_run = mem::replace(&mut self.current_call.current_run, new_run);
        self.current_call.run_stack.push(old_run);
        Ok(())
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

    /// _qty tag_ **issue** → _token_
    ///
    /// Cleartext branch only; encrypted `qty: Point` errors `TokenRequiresCS`.
    fn op_issue(&mut self) -> Result<(), VMError> {
        let tag = self.pop_value()?.to_string()?;
        let qty = match self.pop_value()? {
            Value::Int253(i) => i,
            Value::Point(_) => return Err(VMError::TokenRequiresCS),
            _ => return Err(VMError::TypeNotInt253),
        };
        let actor = self.require_actor()?.clone();
        let flv = flavor_from_actor(&actor, &tag);
        self.txlog.push(crate::tx::TxEntry::Issue(
            Commitment::unblinded(qty).to_point(),
            Commitment::unblinded(flv).to_point(),
        ));
        self.push_value(Value::ClearToken(ClearToken::new(qty, flv)));
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
                self.push_value(Value::Int253(Int253::zero()));
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

    /// _cid tag_ **issueflv** → _int_
    ///
    /// Pure helper: no CS, no txlog effect, no actor-context requirement.
    fn op_issueflv(&mut self) -> Result<(), VMError> {
        let tag = self.pop_value()?.to_string()?;
        let cid = self.pop_value()?.to_string()?;
        if cid.len() != 32 {
            return Err(VMError::IndexOutOfRange);
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(cid.as_bytes());
        let flv = flavor_from_actor(&ActorID::Hash(bytes), &tag);
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
        let mut t = Transcript::new(b"flamevm.signcall.v1");
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
    /// stack with the value, matching zkvm's `to_output()` pattern.
    fn op_input(&mut self) -> Result<(), VMError> {
        self.require_external()?;
        let cell = self.pop_value()?.to_string()?.to_cell()?;
        self.txlog.push(crate::tx::TxEntry::Input(cell.id()));
        self.last_anchor = Some(cell.to_anchor());
        self.push_value(Value::Cell(cell));
        Ok(())
    }

    /// _args… k pred_ **cell** → _cell_
    fn op_cell(&mut self) -> Result<(), VMError> {
        let pred_point = self.pop_value()?.to_point()?;
        let k = self.pop_byte_count(usize::MAX)?;
        let payload = self.pop_n_portable(k)?;
        let anchor = self.last_anchor.take().ok_or(VMError::AnchorMissing)?;
        let cell = Cell::new(Predicate::Opaque(pred_point.inner), anchor, payload);
        self.last_anchor = Some(cell.to_anchor());
        self.push_value(Value::Cell(cell));
        Ok(())
    }

    /// _args… k pred_ **output** → ø
    fn op_output(&mut self) -> Result<(), VMError> {
        let pred_point = self.pop_value()?.to_point()?;
        let k = self.pop_byte_count(usize::MAX)?;
        let payload = self.pop_n_portable(k)?;
        let anchor = self.last_anchor.take().ok_or(VMError::AnchorMissing)?;
        let cell = Cell::new(Predicate::Opaque(pred_point.inner), anchor, payload);
        self.last_anchor = Some(cell.to_anchor());
        self.txlog.push(crate::tx::TxEntry::Output(cell));
        Ok(())
    }

    /// _cell internal_key neighbors position program args… k_ **open** → _results…_
    ///
    /// Verifies the call-proof against the cell's predicate, pours the
    /// payload + args onto the current stack, and enters a new Run
    /// over the unlocked program. Run-level (no new CallFrame).
    fn op_open(&mut self) -> Result<(), VMError> {
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;
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
        // verify_callproof succeeds iff prog's bytes match the leaf,
        // so we can use the witness-bearing prog directly.
        let _ = cell.predicate.verify_callproof(&cp)?;
        let instrs = prog.to_instructions()?;

        for v in cell.payload {
            self.push_value(v);
        }
        for v in args {
            self.push_value(v);
        }
        self.enter_run(instrs)
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

    /// _args… k refund gas bytes method addr_ **send** → ø
    ///
    /// Queues a [`Message`] for the consensus layer to instantiate as a
    /// future internal tx and emits a `TxEntry::Send`. The anchor is
    /// ratcheted from `last_anchor` before the entry is appended.
    fn op_send(&mut self) -> Result<(), VMError> {
        let target = ActorID::Hash(self.pop_string_32()?);
        let method = Int253::from(self.pop_value()?.to_int253()?);
        let vbytes = self.pop_value()?.to_int253()?.to_u64().ok_or(VMError::InvalidBitrange)?;
        let gas = self.pop_value()?.to_int253()?.to_u64().ok_or(VMError::InvalidBitrange)?;
        let refund_predicate = Predicate::Opaque(
            curve25519_dalek::ristretto::CompressedRistretto(self.pop_string_32()?),
        );
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;

        for v in &args {
            if !v.is_portable() {
                return Err(VMError::NonPortableInSend);
            }
        }

        let anchor = self.last_anchor.unwrap_or(Anchor([0u8; 32])).ratchet();
        self.last_anchor = Some(anchor);

        let payload_hash = {
            let mut t = merlin::Transcript::new(b"flamevm.send.payload");
            t.append_message(b"len", &(args.len() as u64).to_le_bytes());
            let mut buf = Vec::new();
            for v in &args {
                buf.clear();
                crate::encoding::write_value(&mut buf, v)
                    .map_err(|_| VMError::NonPortableInSend)?;
                t.append_message(b"item", &buf);
            }
            let mut h = [0u8; 32];
            t.challenge_bytes(b"payload_hash", &mut h);
            h
        };

        self.txlog.push(crate::tx::TxEntry::Send {
            anchor,
            target: target.clone(),
            method,
            refund_predicate: refund_predicate.clone(),
            gas,
            vbytes,
            payload_hash,
        });
        let caller = self.current_call.kind.actor().cloned();
        self.sends.push(Message {
            target,
            method,
            caller,
            anchor,
            payload: args,
            gas,
            vbytes,
            refund_predicate,
        });
        Ok(())
    }

    /// _args… k gas bytes method addr_ **call** → _results…_
    ///
    /// Synchronous actor-to-actor call. Re-entrancy guard rejects direct
    /// or indirect cycles. Emits `TxEntry::Call` binding the callee's
    /// pre-state hash and ratcheted anchor into the Internal TxID.
    fn op_call(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let callee = ActorID::Hash(self.pop_string_32()?);
        let method = Int253::from(self.pop_value()?.to_int253()?);
        let vbytes = self.pop_value()?.to_int253()?.to_u64().ok_or(VMError::InvalidBitrange)?;
        let gas = self.pop_value()?.to_int253()?.to_u64().ok_or(VMError::InvalidBitrange)?;
        let k = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(k)?;

        if iter_actor_ids_on_stack(&self.current_call, &self.call_stack)
            .any(|id| id == &callee)
        {
            return Err(VMError::ReentrancyDetected);
        }

        let script = registry.resolve_method(&callee, method)?;
        let pre_state_root = state_root(&registry.load_state(&callee)?)?;

        let callee_anchor = self.last_anchor.unwrap_or(Anchor([0u8; 32])).ratchet();
        self.last_anchor = Some(callee_anchor);
        self.txlog.push(crate::tx::TxEntry::Call {
            callee: callee.clone(),
            method,
            pre_state_root,
            callee_anchor,
        });

        let mem_limit = registry.actor_vbytes(&callee)?.saturating_mul(4);
        let caller = self
            .current_call
            .kind
            .actor()
            .cloned()
            .unwrap_or(ActorID::Hash([0u8; 32]));
        let program = crate::program::Program::parse(&script)?;
        let mut frame = CallFrame::new(
            program.into_instructions(),
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
        for v in args {
            frame.stack.push(v);
        }

        let parent = core::mem::replace(&mut self.current_call, frame);
        self.call_stack.push(parent);
        Ok(())
    }

    /// **load** → _dict_
    ///
    /// Loads the current actor's state, marks the actor for destruction
    /// (re-entry blocked until `save`), and pushes the wrapper Dict.
    /// An unmatched load destroys the actor at tx commit (Q6).
    fn op_load(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let actor = self.require_actor()?.clone();
        if self.current_call.loaded || registry.is_marked_for_destruction(&actor) {
            return Err(VMError::LoadAlreadyMarked);
        }
        let state = registry.load_state(&actor)?;
        registry.mark_for_destruction(&actor);
        self.current_call.loaded = true;
        self.push_value(Value::Dict(state.to_wrapper_dict()));
        Ok(())
    }

    /// _dict_ **save** → ø
    ///
    /// Pops a wrapper Dict, persists it as the current actor's state,
    /// and clears the re-entrancy mark set by `load`.
    fn op_save(
        &mut self,
        registry: Option<&mut dyn ActorRegistry>,
    ) -> Result<(), VMError> {
        let registry = registry.ok_or(VMError::RegistryUnavailable)?;
        let actor = self.require_actor()?.clone();
        if !self.current_call.loaded {
            return Err(VMError::SaveWithoutLoad);
        }
        let state = ActorState::from_wrapper_dict(self.pop_value()?.to_dict()?)?;
        registry.save_state(&actor, state)?;
        registry.unmark_for_destruction(&actor);
        self.current_call.loaded = false;
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

    /// _cell prog sig args… m_ **signcall** → _items… k_
    ///
    /// Defers an Explicit signature over `prog`, pours payload + args
    /// onto the stack, enters a new Run over `prog`.
    fn op_signcall(&mut self) -> Result<(), VMError> {
        let m = self.pop_byte_count(usize::MAX)?;
        let args = self.pop_n_values(m)?;
        let sig_str = self.pop_value()?.to_string()?;
        let prog_str = self.pop_value()?.to_string()?;
        let cell = self.pop_value()?.to_cell()?;
        if sig_str.as_bytes().len() != 64 {
            return Err(VMError::BadSignatureBytes);
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(sig_str.as_bytes());
        let msg = Self::signcall_message(&prog_str.bytes_view().into_owned());
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
        let instrs = prog_str.to_instructions()?;
        self.enter_run(instrs)
    }

    /// **actorid** → _string_
    fn op_actorid(&mut self) -> Result<(), VMError> {
        let actor = self.require_actor()?.clone();
        self.push_value(Value::String(String::from(actor.to_hash().to_vec())));
        Ok(())
    }

    /// **anchor** → _string_
    fn op_anchor(&mut self) -> Result<(), VMError> {
        let a = self.current_call.kind.anchor().ok_or(VMError::OpcodeRequiresActorContext)?;
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
        let var = self.pop_value()?.to_variable()?;
        let (_point, r1cs_var) = delegate.commit_variable(&var.commitment)?;
        let witness = var.commitment.assignment();
        let expr = crate::Expression::LinearCombination(
            vec![(r1cs_var, Scalar::one())],
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
        let q_blind = self.pop_value()?.to_int253()?;
        let q_value = self.pop_value()?.to_int253()?;
        let f_blind = self.pop_value()?.to_int253()?;
        let f_value = self.pop_value()?.to_int253()?;
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
