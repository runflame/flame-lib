use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub(crate) inner: CompressedRistretto,
}

impl Point {
    pub fn from_compressed(p: CompressedRistretto) -> Self {
        Point { inner: p }
    }

    /// Constructs from a 32-byte compressed Ristretto encoding. Does not
    /// validate decompressability; callers that need a valid group element
    /// must check via `inner.decompress()`.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Point { inner: CompressedRistretto(bytes) }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        self.inner.as_bytes()
    }
}

pub struct MultiscalarMul {
    weights: Vec<Scalar>,
    points: Vec<Option<RistrettoPoint>>,
}

/// A Merlin transcript wrapper. Linear (non-copyable, non-droppable).
///
/// User-supplied labels are passed directly to the underlying
/// `Transcript` API. This relies on the `dynamic-labels` feature of the
/// runflame fork of merlin, which relaxes the upstream `&'static [u8]`
/// constraint to `&[u8]`.
pub struct Merlin {
    transcript: Transcript,
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
