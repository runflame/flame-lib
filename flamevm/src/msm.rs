//! Lazy multi-scalar-multiplication for custom Sigma-protocols.
//!
//! `MultiscalarMul` is the point-arithmetic analog of [`Expression`]:
//! a stack-visible accumulator of `(scalar, point)` terms that lifts
//! point/scalar combinations into a single deferred check at the end
//! of the transaction. `verify` on an MSM appends it to the same
//! `BatchVerifier` that holds the Schnorr/Musig signatures, asserting
//! `sum(s_i · P_i) == identity` — verified in one batched
//! multi-scalar multiplication (Strauss/Pippenger) along with the
//! signature batch.
//!
//! Construction is implicit via the existing arithmetic opcodes:
//!
//! - `Point + Point` → MSM with two terms.
//! - `Int253 * Point` → MSM with one term (scalar reduced mod ℓ).
//! - `MSM + Point` / `Point + MSM` / `MSM + MSM` → MSM with appended terms.
//! - `MSM * Int253` → MSM with all coefficients scaled.
//! - `-MSM` / `-Point` → MSM with negated coefficients.
//!
//! [`Expression`]: crate::Expression

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;

use crate::crypto::Point;
use crate::int253::Int253;

/// Lazy multi-scalar multiplication: a deferred assertion
/// `sum(scalar_i · point_i) == identity` that the VM appends to the
/// delegate's `BatchVerifier` when `verify` consumes it.
///
/// Linear (non-copyable, non-droppable) and stack-only — like
/// [`Expression`](crate::Expression) and [`Constraint`](crate::Constraint).
/// Points are stored compressed; decompression is deferred to batch-
/// verify time (Dalek's `optional_multiscalar_mul` handles
/// undecompressable points by failing the batch).
///
/// Internal storage is a flat `Vec<(Scalar, CompressedRistretto)>`.
/// Once an MSM is on the stack, the originating `Point` enum variant
/// (Opaque / Commitment / Predicate) is irrelevant — only the
/// canonical compressed bytes matter for the verification equation,
/// so the prover-side witness is not preserved past the MSM boundary.
#[derive(Clone, Debug)]
pub struct MultiscalarMul {
    /// `(scalar, point)` terms whose weighted sum must equal the
    /// Ristretto identity point. Empty MSM trivially verifies (sum
    /// over zero terms is identity).
    terms: Vec<(Scalar, CompressedRistretto)>,
}

impl MultiscalarMul {
    /// Constructs an MSM with a single term `(1, point)`.
    pub fn from_point(p: &Point) -> Self {
        Self { terms: vec![(Scalar::ONE, p.to_compressed())] }
    }

    /// Constructs an MSM with a single term `(scalar, point)`.
    pub fn term(s: Scalar, p: CompressedRistretto) -> Self {
        Self { terms: vec![(s, p)] }
    }

    /// Term count. Useful for tests and gas accounting.
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// Appends another MSM's terms (consumes both).
    pub fn append(mut self, mut other: MultiscalarMul) -> Self {
        self.terms.append(&mut other.terms);
        self
    }

    /// Appends a single `(scalar, point)` term.
    pub fn push_term(mut self, s: Scalar, p: CompressedRistretto) -> Self {
        self.terms.push((s, p));
        self
    }

    /// Appends a Point with coefficient 1.
    pub fn push_point(self, p: &Point) -> Self {
        self.push_term(Scalar::ONE, p.to_compressed())
    }

    /// Negates all scalar coefficients.
    pub fn negated(mut self) -> Self {
        for (s, _) in &mut self.terms {
            *s = -*s;
        }
        self
    }

    /// Scales all coefficients by `factor`.
    pub fn scaled(mut self, factor: Scalar) -> Self {
        for (s, _) in &mut self.terms {
            *s *= factor;
        }
        self
    }

    /// Consumes self, returning the `(scalar, point)` terms.
    pub fn into_terms(self) -> Vec<(Scalar, CompressedRistretto)> {
        self.terms
    }
}

/// Convenience: `Int253 → Scalar` via the canonical sign-magnitude
/// reduction. Matches the conversion used elsewhere in the VM
/// (e.g. `Prover::commit_variable`).
pub(crate) fn int_to_scalar(i: Int253) -> Scalar {
    i.into()
}
