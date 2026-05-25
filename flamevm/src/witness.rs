//! Per-input prover witnesses for re-attaching Commitment::Open after Cell::decode.

use crate::constraints::Commitment;

/// Witness scalars for a single `Token` payload entry. Both fields
/// **must** be `Commitment::Open` (witness-bearing). Dispatch
/// (`attach_input_witnesses`) enforces this via
/// [`crate::errors::VMError::WitnessNotOpen`] before the
/// point-equality check, so a caller bug surfaces immediately
/// rather than cascading into a far-away `WitnessMissing` from
/// `mix`/`commit_variable`.
///
/// The fields are typed as `Commitment` (the parent enum) rather
/// than a dedicated `OpenCommitment` newtype because the existing
/// `Commitment::Open` already carries the `(Int253, Scalar)`
/// witness pair, and the dispatch-time runtime check is sufficient
/// — keeps the type wall thin while still being strict.
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
