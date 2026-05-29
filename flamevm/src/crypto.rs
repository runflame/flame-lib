use curve25519_dalek::ristretto::CompressedRistretto;
use merlin::Transcript;

use crate::constraints::Commitment;
use crate::errors::VMError;

/// Ristretto255 group element on the stack — always 32 bytes on the
/// wire, but on the prover side may carry typed witness data for
/// downstream constraint-system or predicate ops.
///
/// `Opaque` is the verifier's view (and the prover's view when no
/// witness is needed). `Commitment` and `Predicate` are prover-side
/// variants that ride alongside the canonical 32-byte point. Boxed so
/// the enum stays small and the common `Opaque` path is inline.
#[derive(Clone, Debug)]
pub enum Point {
    /// Verifier-visible compressed point.
    Opaque(CompressedRistretto),
    /// Pedersen commitment witness; canonical bytes via `commitment.to_point()`.
    Commitment(Box<Commitment>),
    /// Taproot predicate witness; canonical bytes via `predicate.to_point()`.
    Predicate(Box<crate::cell::Predicate>),
}

impl Point {
    /// Wraps a `CompressedRistretto` as the verifier-visible `Opaque` variant.
    pub fn from_compressed(p: CompressedRistretto) -> Self {
        Point::Opaque(p)
    }

    /// Wraps 32 raw bytes as `Opaque`. Does not validate decompressability;
    /// callers that need a valid group element must check via
    /// `to_compressed().decompress()`.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Point::Opaque(CompressedRistretto(bytes))
    }

    /// Constructs a witness-bearing `Commitment` variant.
    pub fn commitment(c: Commitment) -> Self {
        Point::Commitment(Box::new(c))
    }

    /// Constructs a witness-bearing `Predicate` variant.
    pub fn predicate(p: crate::cell::Predicate) -> Self {
        Point::Predicate(Box::new(p))
    }

    /// Returns the canonical 32-byte compressed Ristretto point. Cheap
    /// for `Opaque`; computes the point for witness-bearing variants.
    pub fn to_compressed(&self) -> CompressedRistretto {
        match self {
            Point::Opaque(c) => *c,
            Point::Commitment(c) => c.to_point(),
            Point::Predicate(p) => p.to_point(),
        }
    }

    /// Returns the canonical 32-byte form (owned). Convenience over
    /// `to_compressed().to_bytes()` for sites that just need bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        *self.to_compressed().as_bytes()
    }

    /// Downcasts to `Commitment`. `Opaque` → `Commitment::Closed`;
    /// `Commitment` returns its witness directly; `Predicate` errors.
    pub fn to_commitment(self) -> Result<Commitment, VMError> {
        match self {
            Point::Commitment(c) => Ok(*c),
            Point::Opaque(c) => Ok(Commitment::Closed(c)),
            Point::Predicate(_) => Err(VMError::TypeNotPoint),
        }
    }

    /// Downcasts to `Predicate`. `Opaque` → `Predicate::Opaque`;
    /// `Predicate` returns its witness directly; `Commitment` errors.
    pub fn to_predicate(self) -> Result<crate::cell::Predicate, VMError> {
        match self {
            Point::Predicate(p) => Ok(*p),
            Point::Opaque(c) => Ok(crate::cell::Predicate::opaque(c)),
            Point::Commitment(_) => Err(VMError::TypeNotPoint),
        }
    }
}

/// A Merlin transcript wrapper. Linear (non-copyable, non-droppable).
///
/// User-supplied labels are passed directly to the underlying
/// `Transcript` API. This relies on the `dynamic-labels` feature of
/// the runflame fork of merlin (see workspace `[patch.crates-io]`),
/// which relaxes the upstream `&'static [u8]` constraint to `&[u8]`.
///
/// Rust-`Clone` (duplicates the transcript state) — distinct from VM
/// copyability, which `is_copyable` denies.
#[derive(Clone)]
pub struct Merlin {
    transcript: Transcript,
}

/// Opaque, state-dependent fingerprint: clones the transcript and
/// squeezes 8 challenge bytes, so two transcripts in the same state
/// print the same id without revealing or disturbing the real state.
impl core::fmt::Debug for Merlin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut probe = self.transcript.clone();
        let mut id = [0u8; 8];
        probe.challenge_bytes(b"flamevm.merlin.debug", &mut id);
        write!(f, "Merlin{{0x{:016x}}}", u64::from_be_bytes(id))
    }
}

impl Merlin {
    /// Creates a fresh transcript bound to `label`.
    pub fn new(label: &[u8]) -> Self {
        Merlin { transcript: Transcript::new(label) }
    }

    /// Appends `data` to the transcript under `label`. Used by the
    /// `merlinwrite` opcode.
    pub fn write_bytes(&mut self, label: &[u8], data: &[u8]) {
        self.transcript.append_message(label, data);
    }

    /// Squeezes `n` bytes of challenge from the transcript, tagged by
    /// `label`. Used by the `merlinread` opcode.
    pub fn read_bytes(&mut self, label: &[u8], n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        self.transcript.challenge_bytes(label, &mut out);
        out
    }
}
