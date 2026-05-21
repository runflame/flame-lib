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
}
