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
    fn commit_variable(
        &mut self,
        commitment: &CompressedRistretto,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError>;

    /// Consumes the delegate after VM execution finishes cleanly.
    ///
    /// Prover: builds the Bulletproofs proof, processes deferred sigs as
    /// signing material. Verifier: verifies the supplied proof, processes
    /// deferred sigs as a batched check.
    fn finalize(self, deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError>;
}

// ── Run ──────────────────────────────────────────────────────────

/// A single bytecode script being interpreted. Multiple runs may nest
/// within one call (via `run`, `loop`, `switch`); each pushes onto
/// `CallFrame.run_stack` and is resumed on `break`/`return`/end-of-script.
pub struct Run {
    script: Vec<u8>,
    pc: usize,
}

impl Run {
    pub fn new(script: Vec<u8>) -> Self {
        Self { script, pc: 0 }
    }

    /// Reads the next opcode byte and advances the program counter.
    /// Returns `None` at end of script.
    fn next_byte(&mut self) -> Option<u8> {
        let b = self.script.get(self.pc).copied()?;
        self.pc += 1;
        Some(b)
    }

    /// True iff PC has reached or passed the end of the script.
    pub(crate) fn is_finished(&self) -> bool {
        self.pc >= self.script.len()
    }

    /// Reads one inline byte and advances PC. Errors if at end of script.
    /// Used by opcodes that consume immediate operands (`pushint8`, etc.).
    fn read_u8(&mut self) -> Result<u8, VMError> {
        self.next_byte().ok_or(VMError::UnexpectedEndOfScript)
    }

    /// Reads `n` inline bytes and advances PC. Errors if fewer than `n`
    /// bytes remain.
    fn read_bytes(&mut self, n: usize) -> Result<&[u8], VMError> {
        let start = self.pc;
        let end = start.checked_add(n).ok_or(VMError::UnexpectedEndOfScript)?;
        if end > self.script.len() {
            return Err(VMError::UnexpectedEndOfScript);
        }
        self.pc = end;
        Ok(&self.script[start..end])
    }

    /// Reads `n` ≤ 16 inline bytes as a little-endian unsigned magnitude.
    /// `pushint8/16/64/128` use this for their magnitude operand.
    fn read_le_uint(&mut self, n: usize) -> Result<u128, VMError> {
        debug_assert!(n <= 16);
        let bytes = self.read_bytes(n)?;
        let mut acc: u128 = 0;
        for (i, &b) in bytes.iter().enumerate() {
            acc |= (b as u128) << (8 * i);
        }
        Ok(acc)
    }

    /// Reads a sub-varint (the 1+payload-byte length prefix used by
    /// `pushstr` and the wire format for dicts/strings).
    ///
    /// Matches the canonical sub-varint format from `encoding.rs`:
    ///
    /// ```text
    /// tag 0  + 1 LE byte    → 0..=255
    /// tag 1  + 2 LE bytes   → 256..=65_791   (value = 256 + w)
    /// tag 2  + 4 LE bytes   → 65_792..       (value = 65_792 + w)
    /// tag 3  + 8 LE bytes   → 4_295_033_088.. (value = 4_295_033_088 + w)
    /// ```
    fn read_sub_varint(&mut self) -> Result<u64, VMError> {
        const SUBVAR_U16_BASE: u64 = 256;
        const SUBVAR_U32_BASE: u64 = 65_792;
        const SUBVAR_U64_BASE: u64 = 4_295_033_088;
        let tag = self.read_u8()?;
        match tag {
            0 => Ok(self.read_u8()? as u64),
            1 => {
                let bytes = self.read_bytes(2)?;
                let mut arr = [0u8; 2];
                arr.copy_from_slice(bytes);
                Ok(SUBVAR_U16_BASE + u16::from_le_bytes(arr) as u64)
            }
            2 => {
                let bytes = self.read_bytes(4)?;
                let mut arr = [0u8; 4];
                arr.copy_from_slice(bytes);
                Ok(SUBVAR_U32_BASE + u32::from_le_bytes(arr) as u64)
            }
            3 => {
                let bytes = self.read_bytes(8)?;
                let mut arr = [0u8; 8];
                arr.copy_from_slice(bytes);
                Ok(SUBVAR_U64_BASE.wrapping_add(u64::from_le_bytes(arr)))
            }
            _ => Err(VMError::UnexpectedEndOfScript),
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
        Self {
            stack: Vec::new(),
            current_run: Run::new(script),
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

    /// Executes one opcode in external context. Returns `Ok(true)` to
    /// keep running, `Ok(false)` to stop (entire tx finished).
    fn step_external<D: Delegate>(&mut self, _delegate: &mut D) -> Result<bool, VMError> {
        let Some(op) = self.current_call.current_run.next_byte() else {
            return self.finish_run();
        };
        if self.try_common(op)? {
            return Ok(true);
        }
        // External-only opcodes go here (extvar, intvar, range, ...).
        Err(VMError::UnknownOpcode(op))
    }

    /// Executes one opcode in internal context. Returns `Ok(true)` to
    /// keep running, `Ok(false)` to stop.
    fn step_internal(&mut self) -> Result<bool, VMError> {
        let Some(op) = self.current_call.current_run.next_byte() else {
            return self.finish_run();
        };
        if self.try_common(op)? {
            return Ok(true);
        }
        // Internal-only opcodes go here (call, send, load, save, ...).
        Err(VMError::UnknownOpcode(op))
    }

    /// Dispatches opcodes whose behavior is identical in both contexts.
    /// Returns `Ok(true)` if handled, `Ok(false)` if not (callers fall
    /// through to context-specific dispatch), `Err` on failure.
    fn try_common(&mut self, op: u8) -> Result<bool, VMError> {
        match op {
            // ── Phase 1: stack literals & manipulation ─────────────
            // push:k — small immediate
            0x00..=0x0f => {
                self.push_value(Value::Int253(Int253::from(op as u64)));
                Ok(true)
            }
            // pushint{8,16,64,128} — magnitude width × sign pair
            0x10 | 0x11 => {
                self.op_pushint_magnitude(1, op == 0x11)?;
                Ok(true)
            }
            0x12 | 0x13 => {
                self.op_pushint_magnitude(2, op == 0x13)?;
                Ok(true)
            }
            0x14 | 0x15 => {
                self.op_pushint_magnitude(8, op == 0x15)?;
                Ok(true)
            }
            0x16 | 0x17 => {
                self.op_pushint_magnitude(16, op == 0x17)?;
                Ok(true)
            }
            // pushint — full 32-byte sign-magnitude
            0x18 => {
                self.op_pushint_full()?;
                Ok(true)
            }
            0x19 => {
                self.op_pushstr()?;
                Ok(true)
            }
            0x1a => {
                self.op_pushpoint()?;
                Ok(true)
            }
            0x1b => {
                self.op_pushtoken()?;
                Ok(true)
            }
            0x1c => {
                self.op_drop()?;
                Ok(true)
            }
            0x1d => {
                self.op_nop()?;
                Ok(true)
            }
            0x1e => {
                self.op_dup()?;
                Ok(true)
            }
            0x1f => {
                self.op_roll()?;
                Ok(true)
            }
            // dup:k — k encoded in the low nibble
            0x20..=0x2f => {
                self.op_dup_k((op - 0x20) as usize)?;
                Ok(true)
            }
            // roll:k
            0x30..=0x3f => {
                self.op_roll_k((op - 0x30) as usize)?;
                Ok(true)
            }
            // ── Phase 4: string ops ────────────────────────────────
            0x40 => {
                self.op_read_bits()?;
                Ok(true)
            }
            0x41 => {
                self.op_read_int()?;
                Ok(true)
            }
            0x42 => {
                self.op_read_str()?;
                Ok(true)
            }
            0x43 => {
                self.op_read_point()?;
                Ok(true)
            }
            0x44 => {
                self.op_write_bits()?;
                Ok(true)
            }
            0x45 => {
                self.op_write_int()?;
                Ok(true)
            }
            0x46 => {
                self.op_append()?;
                Ok(true)
            }
            0x47 => {
                self.op_write_zeros()?;
                Ok(true)
            }
            0x48 => {
                self.op_bit_not()?;
                Ok(true)
            }
            0x49 => {
                self.op_bit_or()?;
                Ok(true)
            }
            0x4a => {
                self.op_bit_and()?;
                Ok(true)
            }
            0x4b => {
                self.op_bit_xor()?;
                Ok(true)
            }
            0x4c => {
                self.op_shift_left()?;
                Ok(true)
            }
            0x4d => {
                self.op_shift_right()?;
                Ok(true)
            }
            0x4e => {
                self.op_keccak256()?;
                Ok(true)
            }
            // ── Phase 3: Int253 arithmetic, logic, size ────────────
            0x50 => {
                self.op_abs()?;
                Ok(true)
            }
            0x51 => {
                self.op_eq()?;
                Ok(true)
            }
            0x52 => {
                self.op_neg()?;
                Ok(true)
            }
            0x53 => {
                self.op_add()?;
                Ok(true)
            }
            0x54 => {
                self.op_mul()?;
                Ok(true)
            }
            0x55 => {
                self.op_divmod()?;
                Ok(true)
            }
            0x56 => {
                self.op_mod252()?;
                Ok(true)
            }
            0x57 => {
                self.op_not()?;
                Ok(true)
            }
            0x58 => {
                self.op_and()?;
                Ok(true)
            }
            0x59 => {
                self.op_or()?;
                Ok(true)
            }
            0x5f => {
                self.op_size()?;
                Ok(true)
            }
            // ── Phase 5: Dict ops ──────────────────────────────────
            0x60 => {
                self.op_dict()?;
                Ok(true)
            }
            0x61 => {
                self.op_put()?;
                Ok(true)
            }
            0x62 => {
                self.op_replace()?;
                Ok(true)
            }
            0x63 => {
                self.op_get()?;
                Ok(true)
            }
            0x64 => {
                self.op_getopt()?;
                Ok(true)
            }
            0x65 => {
                self.op_getdup()?;
                Ok(true)
            }
            0x66 => {
                self.op_first()?;
                Ok(true)
            }
            0x67 => {
                self.op_last()?;
                Ok(true)
            }
            0x68 => {
                self.op_next()?;
                Ok(true)
            }
            // ── Phase 9: cells & cell-open opcodes ─────────────────
            0x91 => {
                self.op_cell()?;
                Ok(true)
            }
            0x92 => {
                self.op_output()?;
                Ok(true)
            }
            0x93 => {
                self.op_open()?;
                Ok(true)
            }
            0x98 => {
                self.op_signtx()?;
                Ok(true)
            }
            0x99 => {
                self.op_signrun()?;
                Ok(true)
            }
            // ── Phase 6: Hash & Merlin ─────────────────────────────
            0x69 => {
                self.op_merlin()?;
                Ok(true)
            }
            0x6a => {
                self.op_merlin_write()?;
                Ok(true)
            }
            0x6b => {
                self.op_merlin_read()?;
                Ok(true)
            }
            0x6c => {
                self.op_sha256()?;
                Ok(true)
            }
            0x6d => {
                self.op_sha512()?;
                Ok(true)
            }
            0x6e => {
                self.op_sha3()?;
                Ok(true)
            }
            // ── Phase 2: control flow ──────────────────────────────
            0x79 => {
                self.op_verify()?;
                Ok(true)
            }
            0x7b => {
                self.op_run()?;
                Ok(true)
            }
            0x7c => {
                self.op_loop()?;
                Ok(true)
            }
            0x7d => {
                self.op_switch()?;
                Ok(true)
            }
            0x7e => {
                self.op_return()?;
                Ok(true)
            }
            0x7f => {
                self.op_type()?;
                Ok(true)
            }
            0x80..=0x8f => {
                self.op_break_k((op - 0x80) as usize)?;
                Ok(true)
            }
            _ => Ok(false),
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

    /// `0x10..=0x17` `pushint{8,16,64,128}` — reads `n_bytes` little-endian
    /// inline bytes as an unsigned magnitude, attaches the opcode-encoded
    /// sign, pushes an `Int253`.
    fn op_pushint_magnitude(&mut self, n_bytes: usize, negative: bool) -> Result<(), VMError> {
        let magnitude = self.current_call.current_run.read_le_uint(n_bytes)?;
        // All u128 values are valid scalars (well below ℓ ≈ 2²⁵²).
        let mut scalar_bytes = [0u8; 32];
        scalar_bytes[..16].copy_from_slice(&magnitude.to_le_bytes());
        let scalar = Scalar::from_canonical_bytes(scalar_bytes)
            .ok_or(VMError::InvalidInt253Encoding)?;
        let int = Int253::from_parts(negative, scalar);
        self.push_value(Value::Int253(int));
        Ok(())
    }

    /// `0x18` `pushint` — reads 32 inline bytes as a canonical
    /// sign-magnitude `Int253`.
    fn op_pushint_full(&mut self) -> Result<(), VMError> {
        let bytes = self.current_call.current_run.read_bytes(32)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        let int = Int253::from_bytes(arr).ok_or(VMError::InvalidInt253Encoding)?;
        self.push_value(Value::Int253(int));
        Ok(())
    }

    /// `0x19` `pushstr` — reads sub-varint length, then that many inline
    /// bytes, pushes a `String`.
    fn op_pushstr(&mut self) -> Result<(), VMError> {
        let len = self.current_call.current_run.read_sub_varint()?;
        let len = usize::try_from(len).map_err(|_| VMError::UnexpectedEndOfScript)?;
        let bytes = self.current_call.current_run.read_bytes(len)?;
        let s = String::from(bytes.to_vec());
        self.push_value(Value::String(s));
        Ok(())
    }

    /// `0x1a` `pushpoint` — reads 32 inline bytes as a compressed
    /// Ristretto encoding (decompressability is not validated here).
    fn op_pushpoint(&mut self) -> Result<(), VMError> {
        let bytes = self.current_call.current_run.read_bytes(32)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        self.push_value(Value::Point(Point::from_bytes(arr)));
        Ok(())
    }

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

    /// `0x7c` `loop` — resets the current Run's PC to the start.
    /// Without a `break`/`return` reachable from inside, this is an
    /// unbounded loop; gas metering (Phase 17) is the long-term cap.
    fn op_loop(&mut self) -> Result<(), VMError> {
        self.current_call.current_run.pc = 0;
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
        // End the current Run by jumping its PC to the script end. The
        // dispatch loop's `finish_run` will pop the next saved Run (or
        // call `finish_call` if none).
        let run = &mut self.current_call.current_run;
        run.pc = run.script.len();
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
    fn run_advances_pc() {
        let mut run = Run::new(vec![0x10, 0x20, 0x30]);
        assert_eq!(run.next_byte(), Some(0x10));
        assert_eq!(run.next_byte(), Some(0x20));
        assert_eq!(run.next_byte(), Some(0x30));
        assert_eq!(run.next_byte(), None);
    }

    #[test]
    fn dirty_stack_at_call_exit_is_an_error() {
        // Strict cross-call semantics: a script that leaves anything on the
        // callee's stack must use `return` to ship those values explicitly.
        // Reaching end-of-script with a non-empty stack is a script bug.
        use crate::Int253;
        let mut reg = StubRegistry { script: vec![] };
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
    fn loop_resets_pc_to_zero() {
        // nop, loop — after nop pc=1; after loop pc=0.
        let mut vm = vm_with_script(vec![0x1d, 0x7c]);
        vm.step_internal().unwrap();
        assert_eq!(vm.current_call.current_run.pc, 1);
        vm.step_internal().unwrap();
        assert_eq!(vm.current_call.current_run.pc, 0);
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
        let mut script = vec![0x19, 0x01, 0x05]; // sub-varint tag=1, payload 5 → length 256+5=261 — wrong
        // Use length 64. sub-varint tag 0, byte 64
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
        let mut script = vec![0x01]; // payload count = 0 cell? No, cells need ≥0 — let me just put one item.
        // payload, count, predicate, cell
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
}
