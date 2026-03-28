use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;

pub struct Point {
    inner: CompressedRistretto,
}

pub struct MultiscalarMul {
    weights: Vec<Scalar>,
    points: Vec<Option<RistrettoPoint>>,
}

pub struct Merlin {
    transcript: Transcript,
}
