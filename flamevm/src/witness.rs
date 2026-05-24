//! Per-input prover witnesses (Phase 22).
//!
//! `Cell::encode` strips `Commitment::Open` → `Closed` on the wire
//! (only the 32-byte point reaches consumers). On the verifier side
//! that's fine — the verifier only ever needs points. On the prover
//! side it's a problem: after `pushstr <cell_bytes> input → open`
//! the Tokens that get poured onto the stack are `Closed`, so a
//! downstream `mix` opcode (which calls
//! `delegate.commit_variable(&commitment)`) fails
//! [`crate::errors::VMError::WitnessMissing`] because
//! `Commitment::Closed` has no `.witness()`.
//!
//! The fix is to thread the witnesses through the prover via the
//! `Instruction::Input(Option<Box<InputWitnesses>>)` variant — same
//! mechanism as `Instruction::Alloc(Option<Int253>)`. Bytecode
//! `encode()` writes only the bare `0x90` opcode; bytecode `parse()`
//! reconstructs `Input(None)`. The witness is a prover-side memory
//! object that never crosses the wire.
//!
//! On dispatch the prover-side `op_input` decodes the cell from the
//! pushed bytes, then walks the payload in order, consuming one
//! [`TokenWitness`] from the queue for every `Value::Token` entry
//! it encounters. The witness's `to_point()` must match the decoded
//! Closed commitment's point — mismatch is a hard error
//! ([`crate::errors::VMError::WitnessPointMismatch`]) since it
//! signals a buggy witness on the prover side, not a malicious
//! input.

use crate::constraints::Commitment;

/// Witness scalars for a single `Token` payload entry. Both fields
/// must be `Commitment::Open` (witness-bearing); the dispatch code
/// asserts they decode to the same point as the cell's encoded
/// commitments before swapping them in.
#[derive(Clone, Debug)]
pub struct TokenWitness {
    /// Open commitment for the Token's quantity. The `Commitment::Open`
    /// variant carries `(Int253 value, Scalar blinding)` which the
    /// prover feeds to `r1cs::Prover::commit(value, blinding)` during
    /// `value_to_allocated`.
    pub qty: Commitment,
    /// Open commitment for the Token's flavor.
    pub flv: Commitment,
}

/// All witness data needed for a single `op_input` invocation —
/// one [`TokenWitness`] per `Value::Token` in the consumed cell's
/// payload, in payload order. Non-Token payload entries (Int253,
/// String, Point, Cell, …) consume no witnesses.
///
/// The witness queue length must match the count of `Token`
/// entries; mismatch on dispatch errors
/// [`crate::errors::VMError::WitnessCountMismatch`].
#[derive(Clone, Debug, Default)]
pub struct InputWitnesses {
    pub tokens: Vec<TokenWitness>,
}

impl InputWitnesses {
    /// Constructs an empty witness queue. Use when the cell payload
    /// contains no Tokens (the prover still needs `Some(witnesses)`
    /// to take the witness-bearing path — but the `tokens` vec stays
    /// empty).
    pub fn empty() -> Self {
        Self { tokens: Vec::new() }
    }

    /// Convenience: build a witness queue from a list of
    /// `(qty, flv)` commitment pairs.
    pub fn from_pairs(pairs: Vec<(Commitment, Commitment)>) -> Self {
        Self {
            tokens: pairs
                .into_iter()
                .map(|(qty, flv)| TokenWitness { qty, flv })
                .collect(),
        }
    }
}
