use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;

pub struct Point {
    pub(crate) inner: CompressedRistretto,
}

impl Point {
    pub fn from_compressed(p: CompressedRistretto) -> Self {
        Point { inner: p }
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
