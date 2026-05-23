//! `Program` builder — assembles a sequence of [`Instruction`]s and
//! produces both:
//!
//! - canonical **bytecode** that the VM (verifier or prover) walks via
//!   its byte-dispatch loop, and
//! - a **witness queue** carrying the prover-side data the bytecode
//!   doesn't contain (one entry per witness-bearing instruction,
//!   in opcode order).
//!
//! This is the lightweight Phase-11 cousin of zkvm's `Program` type.
//! Currently the only witness-bearing instruction is `Alloc`; future
//! phases will widen the enum (and the queue's element type) to cover
//! `commit`-style operations.
//!
//! ## Authoring
//!
//! ```ignore
//! use flamevm::{Int253, Program};
//! let prog = Program::new()
//!     .alloc(Some(Int253::from(7u64)))
//!     .alloc(Some(Int253::from(3u64)))
//!     .add()
//!     .alloc(Some(Int253::from(10u64)))
//!     .eq()
//!     .verify()
//!     .build();
//! let bytecode = prog.to_bytecode();
//! let witnesses = prog.to_witnesses();   // [Some(7), Some(3), Some(10)]
//! ```
//!
//! ## Prover vs verifier flow
//!
//! - Prover: feeds `bytecode` + `witnesses` into the VM via
//!   `Prover::prove` (Phase 11.5).
//! - Verifier: feeds just `bytecode` into `Verifier::verify`; its
//!   witness queue is empty so each `alloc` allocates without a
//!   cleartext assignment.

use std::collections::VecDeque;

use crate::int253::Int253;
use crate::ops::Instruction;

/// A program is a list of [`Instruction`]s. Build with the fluent
/// methods (`alloc`, `add`, `eq`, `verify`, `raw`, …) and finalize
/// with `build()`, then derive bytecode + witnesses for the prover
/// or just bytecode for the verifier.
#[derive(Clone, Debug, Default)]
pub struct Program {
    instructions: Vec<Instruction>,
}

impl Program {
    /// Constructs an empty program.
    pub fn new() -> Self {
        Self { instructions: Vec::new() }
    }

    /// Consumes the builder and returns the underlying instruction list.
    /// (Equivalent in this minimal API to keeping `self` — the fluent
    /// builder methods take `&mut self` so the user can chain or call
    /// `build()` to finish.)
    pub fn build(self) -> Self {
        self
    }

    /// Returns the underlying instructions in order.
    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    /// Appends an instruction; for use by the fluent builder methods
    /// and by callers that want to inject arbitrary variants.
    pub fn push_instr(&mut self, instr: Instruction) -> &mut Self {
        self.instructions.push(instr);
        self
    }

    /// `0x5c alloc` — allocates a low-level R1CS variable and pushes
    /// an Expression wrapping it. `witness = Some(int)` on the prover
    /// side fills the cleartext value the constraint system can use
    /// when proving; `None` on the verifier side leaves the variable
    /// unassigned.
    pub fn alloc(mut self, witness: Option<Int253>) -> Self {
        self.instructions.push(Instruction::Alloc(witness));
        self
    }

    /// `0x5d expr` — lifts a `Variable` on the stack into an
    /// `Expression { LinearCombination([(v, 1)], witness?) }`.
    pub fn expr(mut self) -> Self {
        self.instructions.push(Instruction::Expr);
        self
    }

    /// `0x52 neg`.
    pub fn neg(mut self) -> Self {
        self.instructions.push(Instruction::Neg);
        self
    }

    /// `0x53 add`.
    pub fn add(mut self) -> Self {
        self.instructions.push(Instruction::Add);
        self
    }

    /// `0x54 mul`.
    pub fn mul(mut self) -> Self {
        self.instructions.push(Instruction::Mul);
        self
    }

    /// `0x51 eq`.
    pub fn eq(mut self) -> Self {
        self.instructions.push(Instruction::Eq);
        self
    }

    /// `0x79 verify`.
    pub fn verify(mut self) -> Self {
        self.instructions.push(Instruction::Verify);
        self
    }

    /// Appends raw bytecode bytes verbatim. Used to splice opcodes the
    /// Phase-11 `Instruction` enum hasn't yet enumerated (push
    /// literals, dict ops, etc.).
    pub fn raw(mut self, bytes: Vec<u8>) -> Self {
        self.instructions.push(Instruction::Raw(bytes));
        self
    }

    /// Serializes the program's bytecode (deterministic; matches the
    /// VM's byte-dispatch expectations exactly).
    pub fn to_bytecode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for instr in &self.instructions {
            instr.encode(&mut out);
        }
        out
    }

    /// Builds the witness queue in opcode order. Each witness-bearing
    /// instruction (currently only `Alloc`) contributes exactly one
    /// queue entry; other instructions contribute none.
    pub fn to_witnesses(&self) -> VecDeque<Option<Int253>> {
        self.instructions
            .iter()
            .filter_map(|i| i.witness())
            .collect()
    }
}
