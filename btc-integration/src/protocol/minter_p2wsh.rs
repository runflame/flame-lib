use corepc_client::bitcoin::hashes::{Hash, sha256};
use curve25519_dalek::ristretto::CompressedRistretto;
use flamevm::Predicate;

/// SHA-256 hash of the complete P2WSH witness script used by a Minter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinterP2wsh([u8; 32]);

impl MinterP2wsh {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn from_witness_script(witness_script: &[u8]) -> Self {
        Self::new(sha256::Hash::hash(witness_script).to_byte_array())
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for MinterP2wsh {
    fn from(bytes: [u8; 32]) -> Self {
        Self::new(bytes)
    }
}

impl From<MinterP2wsh> for [u8; 32] {
    fn from(hash: MinterP2wsh) -> Self {
        hash.into_bytes()
    }
}

pub fn parse_predicate(bytes: &[u8]) -> Option<Predicate> {
    let compressed = CompressedRistretto(bytes.try_into().ok()?);
    compressed.decompress()?;
    Some(Predicate::opaque(compressed))
}
