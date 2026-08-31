//! Trie-backed dictionary keyed by numerically ordered [`Int253`] paths.

use crate::chunk::{Chunk, ChunkID, ChunkRef};
use crate::errors::VMError;
use crate::{Int253, Trie};

/// Fixed path width used by [`Dict2`].
pub const DICT2_KEY_BYTES: usize = 32;

/// Experimental Chunk-backed dictionary substrate.
///
/// Values are opaque data Chunks. Their interpretation, portability, and
/// linearity remain the responsibility of the layer that serializes them.
#[derive(Clone, Debug)]
pub struct Dict2 {
    trie: Trie,
}

impl Dict2 {
    /// Constructs an empty Int253-keyed dictionary.
    pub fn new() -> Self {
        Self {
            trie: Trie::new(DICT2_KEY_BYTES).expect("32-byte trie keys fit in a Chunk"),
        }
    }

    /// Wraps a previously validated Int253-keyed trie.
    ///
    /// The caller must establish that every leaf path is the ordered encoding
    /// of a canonical Int253; this cannot be rechecked through pruned branches.
    pub fn from_trusted_trie(trie: Trie) -> Result<Self, VMError> {
        if trie.key_bytes() != DICT2_KEY_BYTES {
            return Err(VMError::TrieKeyLengthMismatch {
                expected: DICT2_KEY_BYTES,
                actual: trie.key_bytes(),
            });
        }
        Ok(Self { trie })
    }

    /// Number of entries recorded by the trie envelope.
    pub fn len(&self) -> usize {
        self.trie.len()
    }

    /// Returns true when the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.trie.is_empty()
    }

    /// Returns the root Chunk commitment, if non-empty.
    pub fn root_id(&self) -> Option<ChunkID> {
        self.trie.root_id()
    }

    /// Borrows the underlying trie.
    pub fn trie(&self) -> &Trie {
        &self.trie
    }

    /// Consumes the dictionary into its underlying trie.
    pub fn into_trie(self) -> Trie {
        self.trie
    }

    /// Looks up a data reference without forcing a pruned value body to load.
    pub fn get_ref(&self, key: &Int253) -> Result<Option<&ChunkRef>, VMError> {
        self.trie.get_ref(&int253_to_ordered_key(*key))
    }

    /// Looks up resident data.
    pub fn get(&self, key: &Int253) -> Result<Option<&Chunk>, VMError> {
        self.trie.get(&int253_to_ordered_key(*key))
    }

    /// Inserts or replaces resident data and returns the prior reference.
    pub fn insert(&mut self, key: Int253, value: Chunk) -> Result<Option<ChunkRef>, VMError> {
        self.trie.insert(&int253_to_ordered_key(key), value)
    }

    /// Removes a key and returns its data reference.
    pub fn remove(&mut self, key: &Int253) -> Result<Option<ChunkRef>, VMError> {
        self.trie.remove(&int253_to_ordered_key(*key))
    }
}

impl Default for Dict2 {
    fn default() -> Self {
        Self::new()
    }
}

/// Converts numeric Int253 order into lexicographic 32-byte order.
///
/// This is the conventional signed-order transform: encode the value as a
/// 256-bit two's-complement big-endian integer, then flip its top bit.
pub fn int253_to_ordered_key(value: Int253) -> [u8; 32] {
    let negative = value.is_negative();
    let mut bytes = value.to_bytes();
    bytes[31] &= 0x7f;
    if negative {
        twos_complement_le(&mut bytes);
    }
    bytes.reverse();
    bytes[0] ^= 0x80;
    bytes
}

/// Decodes an ordered trie path back into a canonical Int253.
///
/// Not every 32-byte path represents an Int253 because magnitudes are bounded
/// by the Ristretto scalar modulus.
pub fn ordered_key_to_int253(mut key: [u8; 32]) -> Option<Int253> {
    key[0] ^= 0x80;
    key.reverse();
    let negative = key[31] & 0x80 != 0;
    if negative {
        twos_complement_le(&mut key);
        key[31] |= 0x80;
    }
    Int253::from_bytes(key)
}

fn twos_complement_le(bytes: &mut [u8; 32]) {
    let mut carry = true;
    for byte in bytes {
        *byte = !*byte;
        if carry {
            let (sum, overflow) = byte.overflowing_add(1);
            *byte = sum;
            carry = overflow;
        }
    }
}

#[cfg(test)]
mod tests {
    use curve25519_dalek::scalar::Scalar;

    use super::*;

    fn value(n: u8) -> Chunk {
        Chunk::new(vec![n], vec![]).unwrap()
    }

    #[test]
    fn ordered_paths_match_numeric_order_and_roundtrip() {
        let largest = Int253::from(-Scalar::ONE);
        let values = [
            -largest,
            Int253::from(-100i64),
            Int253::from(-1i64),
            Int253::ZERO,
            Int253::ONE,
            Int253::from(100u64),
            largest,
        ];
        let paths: Vec<_> = values.iter().copied().map(int253_to_ordered_key).collect();
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
        for (value, path) in values.iter().copied().zip(paths) {
            assert_eq!(ordered_key_to_int253(path), Some(value));
        }
    }

    #[test]
    fn arbitrary_out_of_range_paths_are_rejected() {
        assert!(ordered_key_to_int253([0u8; 32]).is_none());
        assert!(ordered_key_to_int253([0xff; 32]).is_none());
    }

    #[test]
    fn dictionary_operations_use_numeric_keys() {
        let mut dict = Dict2::new();
        assert!(dict.insert(Int253::from(5i64), value(5)).unwrap().is_none());
        assert!(dict
            .insert(Int253::from(-2i64), value(2))
            .unwrap()
            .is_none());
        assert_eq!(dict.len(), 2);
        assert_eq!(
            dict.get(&Int253::from(-2i64)).unwrap().unwrap().payload(),
            &[2]
        );
        assert!(dict.get(&Int253::ZERO).unwrap().is_none());

        let removed = dict.remove(&Int253::from(5i64)).unwrap().unwrap();
        assert_eq!(removed.as_resident().unwrap().payload(), &[5]);
        assert_eq!(dict.len(), 1);
    }
}
