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
/// The `merlin` crate requires `Transcript::new` to take a `&'static [u8]`
/// label, which we can't honor for user-supplied labels. Instead, every
/// `Merlin` opens with a fixed domain separator `flamevm::merlin.v1` and
/// the user-supplied label is appended as the first message under a
/// fixed tag. This preserves the protocol property that two transcripts
/// with different user labels diverge from byte zero.
pub struct Merlin {
    transcript: Transcript,
}

impl Merlin {
    /// Creates a fresh transcript bound to `user_label`.
    pub fn new(user_label: &[u8]) -> Self {
        let mut t = Transcript::new(b"flamevm::merlin.v1");
        t.append_message(b"label", user_label);
        Merlin { transcript: t }
    }

    /// Appends `data` to the transcript under `user_label`. Used by the
    /// `merlinwrite` opcode.
    pub fn write_bytes(&mut self, user_label: &[u8], data: &[u8]) {
        self.transcript.append_message(b"write.label", user_label);
        self.transcript.append_message(b"write.data", data);
    }

    /// Squeezes `n` bytes of challenge from the transcript, tagged by
    /// `user_label`. Used by the `merlinread` opcode.
    pub fn read_bytes(&mut self, user_label: &[u8], n: usize) -> Vec<u8> {
        self.transcript.append_message(b"read.label", user_label);
        let mut out = vec![0u8; n];
        self.transcript.challenge_bytes(b"read.bytes", &mut out);
        out
    }
}
