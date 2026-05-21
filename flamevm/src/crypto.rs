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

pub struct Merlin {
    transcript: Transcript,
}
