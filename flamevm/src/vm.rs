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
use curve25519_dalek::ristretto::CompressedRistretto;
use core::mem;

use crate::errors::VMError;
use crate::tx::TxHeader;
use crate::Value;

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
            0x1d => {
                self.op_nop()?;
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

    // ── Common opcode handlers ───────────────────────────────────

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
}
