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

use crate::errors::VMError;
use crate::tx::TxHeader;
use crate::{ClearToken, Int253, Point, String, Value};

// ── Identifiers and metadata ──────────────────────────────────────

/// 32-byte actor identifier (hash of initial state or constructor script).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorID(pub [u8; 32]);

/// Method index within an actor's `public` dict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodKey(pub u64);

/// 32-byte anchor unique to a tx-initiated send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor(pub [u8; 32]);

/// Unlock predicate of a cell (compressed Ristretto point).
#[derive(Clone, Copy, Debug)]
pub struct Predicate(pub CompressedRistretto);

// ── Deferred signature records ────────────────────────────────────

/// Signature check whose verification is deferred to `Delegate::finalize`.
///
/// Both prover and verifier append to a `Vec<DeferredSig>` during VM
/// execution. The delegate consumes the list at finalize and either signs
/// the missing entries (prover) or batch-verifies them (verifier).
#[derive(Clone, Debug)]
pub struct DeferredSig {
    pub verification_key: CompressedRistretto,
    pub message: Vec<u8>,
    /// `None` on the prover side until signing fills it in.
    pub signature: Option<[u8; 64]>,
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

    /// Reads `n` ≤ 16 inline bytes as a big-endian unsigned magnitude.
    /// `pushint16/64/128` use this for their magnitude operand.
    fn read_be_uint(&mut self, n: usize) -> Result<u128, VMError> {
        debug_assert!(n <= 16);
        let bytes = self.read_bytes(n)?;
        let mut acc: u128 = 0;
        for &b in bytes {
            acc = (acc << 8) | (b as u128);
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
    #[allow(dead_code)]
    last_anchor: Option<Anchor>,

    gas_used: u64,
    vbytes_used: u64,

    current_call: CallFrame,
    call_stack: Vec<CallFrame>,

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
                self.op_read_uint()?;
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

    /// `0x10..=0x17` `pushint{8,16,64,128}` — reads `n_bytes` big-endian
    /// inline bytes as an unsigned magnitude, attaches the opcode-encoded
    /// sign, pushes an `Int253`.
    fn op_pushint_magnitude(&mut self, n_bytes: usize, negative: bool) -> Result<(), VMError> {
        let magnitude = self.current_call.current_run.read_be_uint(n_bytes)?;
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

    /// `0x1b` `pushtoken` — reads 32 inline bytes as the flavor's
    /// canonical scalar, pushes a zero-qty `ClearToken`.
    ///
    /// The spec says "0-qty token of any flavor"; we materialize this as
    /// a `ClearToken { qty: 0, flv: Int253(scalar) }`. The bytes are
    /// interpreted as a canonical Ristretto scalar (no sign bit) —
    /// flavors are typically hash outputs, which are positive scalars.
    fn op_pushtoken(&mut self) -> Result<(), VMError> {
        let bytes = self.current_call.current_run.read_bytes(32)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        let scalar = Scalar::from_canonical_bytes(arr)
            .ok_or(VMError::InvalidInt253Encoding)?;
        let flv = Int253::from_parts(false, scalar);
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

    /// `0x40` `readuint` — `s n → s' x 1 | s 0`. Consume first `n ≤ 32`
    /// bytes of `s` as a little-endian unsigned integer.
    fn op_read_uint(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(32)?;
        let s = self.pop_string()?;
        if s.len() < n {
            self.push_read_failure(s);
            return Ok(());
        }
        let mut scalar_bytes = [0u8; 32];
        scalar_bytes[..n].copy_from_slice(&s.as_bytes()[..n]);
        let scalar = Scalar::from_canonical_bytes(scalar_bytes)
            .ok_or(VMError::InvalidInt253Encoding)?;
        let value = Int253::from_parts(false, scalar);
        let (remainder, _consumed) = s.split_at(n).expect("length checked");
        self.push_value(Value::String(remainder));
        self.push_value(Value::Int253(value));
        self.push_value(Value::Int253(Int253::from(1u64)));
        Ok(())
    }

    /// `0x41` `readint` — `s n → s' x 1 | s 0`. Same as `readuint` but
    /// the high bit of byte `n-1` is interpreted as the sign bit;
    /// remaining bits form the magnitude.
    fn op_read_int(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(32)?;
        let s = self.pop_string()?;
        if s.len() < n {
            self.push_read_failure(s);
            return Ok(());
        }
        if n == 0 {
            // Reading zero bytes yields value 0.
            self.push_value(Value::String(s));
            self.push_value(Value::Int253(Int253::zero()));
            self.push_value(Value::Int253(Int253::from(1u64)));
            return Ok(());
        }
        let mut magnitude_bytes = [0u8; 32];
        magnitude_bytes[..n].copy_from_slice(&s.as_bytes()[..n]);
        let sign_neg = magnitude_bytes[n - 1] & 0x80 != 0;
        magnitude_bytes[n - 1] &= 0x7f;
        let scalar = Scalar::from_canonical_bytes(magnitude_bytes)
            .ok_or(VMError::InvalidInt253Encoding)?;
        let value = Int253::from_parts(sign_neg, scalar);
        let (remainder, _consumed) = s.split_at(n).expect("length checked");
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
    /// the magnitude of `x` to `s`. Phase 4 constraint: `n` must be a
    /// multiple of 8 and `≤ 256` (byte-aligned strings).
    fn op_write_bits(&mut self) -> Result<(), VMError> {
        let n = self.pop_byte_count(256)?;
        if n % 8 != 0 {
            return Err(VMError::BitCountOutOfRange);
        }
        let n_bytes = n / 8;
        let x = self.pop_int253()?;
        let s = self.pop_string()?;
        let magnitude_bytes = x.abs().to_bytes();
        // magnitude_bytes is 32-byte LE of magnitude. Low n bits = first n_bytes.
        let appended = s.append_bytes(&magnitude_bytes[..n_bytes]);
        self.push_value(Value::String(appended));
        Ok(())
    }

    /// `0x45` `writeint` — `s x → s'`. Appends the full 32-byte
    /// sign-magnitude representation of `x` to `s`.
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
    /// Phase 3 limitation: magnitudes must fit `u64`; larger operands
    /// produce `MagnitudeTooLarge` until big-int division lands.
    fn op_divmod(&mut self) -> Result<(), VMError> {
        let z = self.pop_int253()?;
        let x = self.pop_int253()?;
        if z.is_zero() {
            return Err(VMError::DivByZero);
        }
        let (d, r) = x.div_rem(z).ok_or(VMError::MagnitudeTooLarge)?;
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
    /// 2. assert the callee's stack has *exactly* `k` items left,
    /// 3. pop the call frame,
    /// 4. refund leftover gas to the parent,
    /// 5. push the `k` items onto the parent's stack.
    ///
    /// At the outermost frame, `return 0` exits the transaction cleanly.
    /// `return k` with `k > 0` at the outermost frame is an error
    /// (`BadReturnArity`) — there is no parent to receive the values.
    fn op_return(&mut self) -> Result<(), VMError> {
        let k_int = self.pop_int253()?;
        let k_u64 = k_int.to_u64().ok_or(VMError::BadReturnArity)?;
        let k = usize::try_from(k_u64).map_err(|_| VMError::BadReturnArity)?;

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

        if let Some(parent) = self.call_stack.pop() {
            self.current_call = parent;
            self.current_call.gas_limit = self
                .current_call
                .gas_limit
                .saturating_add(leftover_gas);
            self.current_call.stack.extend(return_values);
            return Ok(());
        }

        // Outermost frame. No parent to receive values.
        if k != 0 {
            return Err(VMError::BadReturnArity);
        }
        // Signal end of execution by emptying the current Run and the
        // run-stack. The dispatch loop will then call finish_call which
        // sees an empty stack and exits.
        self.current_call.run_stack.clear();
        self.current_call.current_run.pc = self.current_call.current_run.script.len();
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
            Value::Object(_) => "Object",
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
    fn pushint16_be_decoding() {
        // 0x12 = positive; bytes 0x01 0x02 BE = 258
        let mut vm = vm_with_script(vec![0x12, 0x01, 0x02]);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(258u64));
    }

    #[test]
    fn pushint64_be_decoding() {
        let val: u64 = 0x0102_0304_0506_0708;
        let mut script = vec![0x14];
        script.extend_from_slice(&val.to_be_bytes());
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[0], Int253::from(val));
    }

    #[test]
    fn pushint128_be_decoding() {
        let val: u128 = 0xFEED_FACE_DEAD_BEEF_CAFE_BABE_BADD_CAFEu128;
        let mut script = vec![0x16];
        script.extend_from_slice(&val.to_be_bytes());
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
        let flv = Int253::from(7u64);
        let mut script = vec![0x1b];
        script.extend_from_slice(&flv.to_bytes());
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        match &vm.current_call.stack[0] {
            Value::ClearToken(t) => {
                assert!(t.is_zero_qty());
                assert_eq!(t.flv(), flv);
            }
            other => panic!("expected ClearToken, got {}", value_kind(other)),
        }
    }

    #[test]
    fn pushtoken_rejects_noncanonical_flavor() {
        // All-ones bytes are not a canonical Ristretto scalar.
        let mut script = vec![0x1b];
        script.extend_from_slice(&[0xffu8; 32]);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::InvalidInt253Encoding
        ));
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
        // pushtoken (linear) then dup:0
        let mut script = vec![0x1b];
        script.extend_from_slice(&Int253::from(1u64).to_bytes());
        script.push(0x20);
        let mut vm = vm_with_script(script);
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
    fn return_zero_at_root_exits_cleanly() {
        // push:0, return — k=0, empty stack remaining, root frame: clean exit.
        let mut vm = vm_with_script(vec![0x00, 0x7e]);
        run_until_tx_done(&mut vm).unwrap();
    }

    #[test]
    fn return_nonzero_at_root_errors() {
        // push:7, push:1, return — k=1 at root: nowhere for 7 to go.
        let mut vm = vm_with_script(vec![0x07, 0x01, 0x7e]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::BadReturnArity
        ));
    }

    #[test]
    fn return_with_dirty_leftover_errors() {
        // push:9, push:7, push:1, return — k=1, but two items below count.
        // After popping k, stack has 2 items > k=1 → StackNotClean.
        let mut vm = vm_with_script(vec![0x09, 0x07, 0x01, 0x7e]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::StackNotClean
        ));
    }

    #[test]
    fn return_too_few_items_errors() {
        // push:5, return — k=5 (popped) but only zero items left.
        let mut vm = vm_with_script(vec![0x05, 0x7e]);
        assert!(matches!(
            run_until_tx_done(&mut vm).unwrap_err(),
            VMError::BadReturnArity
        ));
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
            predicate: Predicate(CompressedRistretto([0u8; 32])),
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
        let flv = Int253::from(7u64).to_bytes();
        let mut script = vec![0x1b];
        script.extend_from_slice(&flv);
        script.push(0x1b);
        script.extend_from_slice(&flv);
        script.push(0x51);
        let mut vm = vm_with_script(script);
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
    fn divmod_magnitude_too_large() {
        // pushint full with a magnitude > u64::MAX, then push:1, divmod
        let mut huge = [0u8; 32];
        huge[16] = 1; // 2^128
        let mut script = vec![0x18];
        script.extend_from_slice(&huge);
        script.push(0x01); // push:1
        script.push(0x55);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::MagnitudeTooLarge
        ));
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

    // ── readuint (0x40) ──────────────────────────────────────────

    #[test]
    fn read_uint_success() {
        let mut script = pushstr_bytes(&[0x07, 0x00, 0x01, 0xff, 0xfe]);
        script.push(0x03);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 3);
        assert_int(&vm.current_call.stack[1], Int253::from(65543u64));
        assert_int(&vm.current_call.stack[2], Int253::from(1u64));
        assert_str(&vm.current_call.stack[0], &[0xff, 0xfe]);
    }

    #[test]
    fn read_uint_too_short_preserves_string() {
        let mut script = pushstr_bytes(&[0xaa]);
        script.push(0x03);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_eq!(vm.current_call.stack.len(), 2);
        assert_str(&vm.current_call.stack[0], &[0xaa]);
        assert_int(&vm.current_call.stack[1], Int253::from(0u64));
    }

    #[test]
    fn read_uint_n_too_large_errors() {
        let mut script = pushstr_bytes(&[0u8; 40]);
        script.push(0x10);
        script.push(33);
        script.push(0x40);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }

    // ── readint (0x41) ───────────────────────────────────────────

    #[test]
    fn read_int_positive() {
        // 2-byte LE 0xff 0x7f: magnitude 0x7fff, sign bit of byte 1 = 0
        let mut script = pushstr_bytes(&[0xff, 0x7f]);
        script.push(0x02);
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(32767u64));
    }

    #[test]
    fn read_int_negative() {
        // 2-byte LE 0xff 0xff: sign bit of byte 1 = 1; magnitude after mask = 0x7fff
        let mut script = pushstr_bytes(&[0xff, 0xff]);
        script.push(0x02);
        script.push(0x41);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_int(&vm.current_call.stack[1], Int253::from(-32767i64));
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
    fn write_bits_basic() {
        let mut script = pushstr_bytes(&[0xaa]);
        script.push(0x10);
        script.push(0xab);
        script.push(0x10);
        script.push(8);
        script.push(0x44);
        let mut vm = vm_with_script(script);
        run_to_end(&mut vm).unwrap();
        assert_str(&vm.current_call.stack[0], &[0xaa, 0xab]);
    }

    #[test]
    fn write_bits_non_multiple_of_8_errors() {
        let mut script = pushstr_bytes(&[]);
        script.push(0x10);
        script.push(0x05);
        script.push(0x10);
        script.push(7);
        script.push(0x44);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::BitCountOutOfRange
        ));
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
        script.extend_from_slice(&257u16.to_be_bytes()); // 257 > 256
        script.push(0x4c);
        let mut vm = vm_with_script(script);
        assert!(matches!(
            run_to_end(&mut vm).unwrap_err(),
            VMError::IndexOutOfRange
        ));
    }
}
