//! High-level `Instruction` enum — the *decoded* form of each opcode.
//!
//! Phase 11 introduces this layer so the prover side can author programs
//! with witness data attached (e.g. `Alloc(Some(scalar))`) and have that
//! witness flow into the VM's constraint system at the right moment.
//! The on-the-wire bytecode is unchanged — every Instruction encodes to
//! exactly the bytes its opcode-byte dispatch handler expects to parse —
//! so a prover-built `Program` and the corresponding verifier-side
//! bytecode are byte-identical.
//!
//! ## Phase-11 minimal scope
//!
//! Phase 11 only exposes Instruction variants that the prover needs to
//! drive end-to-end through CS-bound opcodes:
//!
//! - witness-bearing: `Alloc(Option<Int253>)`.
//! - witness-less stack/arith ops the prover can build inline:
//!   `Expr`, `Verify`, `Neg`, `Add`, `Mul`, `Eq`.
//! - a catch-all `Raw(Vec<u8>)` escape hatch so programs can splice
//!   arbitrary existing bytecode (push literals, dict ops, etc.) into a
//!   prover-side `Program` without enumerating every opcode variant.
//!
//! Future phases extend this enum to cover the full opcode set (and
//! mirror zkvm's `Instruction` shape more thoroughly). The escape hatch
//! lets Phase 11 ship without a 50-variant data-layer rewrite.

use crate::int253::Int253;

/// Opcode bytes used by Phase 11's CS instructions. Defined here to keep
/// the encoder, the parser (when added), and the VM dispatch on the same
/// canonical numbers.
pub(crate) const OP_NEG: u8 = 0x52;
pub(crate) const OP_ADD: u8 = 0x53;
pub(crate) const OP_MUL: u8 = 0x54;
pub(crate) const OP_EQ: u8 = 0x51;
pub(crate) const OP_VERIFY: u8 = 0x79;
pub(crate) const OP_ALLOC: u8 = 0x5c;
pub(crate) const OP_EXPR: u8 = 0x5d;

/// A high-level VM instruction.
///
/// Variants either:
/// 1. emit a fixed-length opcode byte sequence (most variants here),
/// 2. carry a witness slot the prover fills and the verifier ignores
///    (`Alloc(Option<Int253>)`), or
/// 3. carry raw bytecode the program author wants spliced verbatim
///    (`Raw(Vec<u8>)` — used to compose Phase 11 programs with
///     non-Phase-11 opcodes the Instruction enum hasn't yet enumerated).
#[derive(Clone, Debug)]
pub enum Instruction {
    /// `0x5c alloc` — allocates a low-level R1CS variable and pushes
    /// `Expression::LinearCombination([(v, 1)], witness?)`. The witness
    /// is consumed at execution time from the delegate's witness queue.
    /// `Some(int)` on the prover side, `None` on the verifier side.
    Alloc(Option<Int253>),
    /// `0x5d expr` — `var → expr`. Pops a `Variable`, calls
    /// `delegate.commit_variable`, pushes the resulting Expression.
    Expr,
    /// `0x52 neg` — pops an Int253 or Expression, pushes its negation.
    Neg,
    /// `0x53 add` — pops two Int253-or-Expressions, pushes their sum.
    Add,
    /// `0x54 mul` — pops two Int253-or-Expressions, pushes their product.
    Mul,
    /// `0x51 eq` — peeks the top two values; if both Expression, also
    /// pushes a Constraint on top (Int253 path unchanged).
    Eq,
    /// `0x79 verify` — pops an Int253 or Constraint; verifies it.
    Verify,
    /// Raw bytecode bytes to splice into the encoded program. The
    /// VM dispatch ultimately runs them via the byte path; the bytes
    /// must be a valid opcode sequence.
    Raw(Vec<u8>),
}

impl Instruction {
    /// Appends this instruction's bytecode representation to `out`.
    /// `Alloc(_)` always writes exactly one byte (the witness is
    /// discarded — it travels via the prover's witness queue instead).
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Instruction::Alloc(_) => out.push(OP_ALLOC),
            Instruction::Expr => out.push(OP_EXPR),
            Instruction::Neg => out.push(OP_NEG),
            Instruction::Add => out.push(OP_ADD),
            Instruction::Mul => out.push(OP_MUL),
            Instruction::Eq => out.push(OP_EQ),
            Instruction::Verify => out.push(OP_VERIFY),
            Instruction::Raw(bytes) => out.extend_from_slice(bytes),
        }
    }

    /// Returns this instruction's contribution to the prover's witness
    /// queue: `Some(witness)` for variants that own a witness, `None`
    /// for variants without one. Walked by `Program::to_witnesses` to
    /// build the queue in the same order as the bytecode's
    /// witness-consuming opcodes.
    pub fn witness(&self) -> Option<Option<Int253>> {
        match self {
            Instruction::Alloc(w) => Some(*w),
            _ => None,
        }
    }
}
