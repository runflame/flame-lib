//! Trie-backed dictionary keyed directly by canonical [`Scalar`] bytes.

use crate::chunk::{Chunk, ChunkID, ChunkRef};
use crate::errors::VMError;
use crate::{Scalar, Trie};

/// Fixed path width used by [`Dict2`].
pub const DICT2_KEY_BYTES: usize = 32;

/// Experimental Chunk-backed dictionary substrate.
///
/// Values are opaque data Chunks. Their interpretation, portability, and
/// linearity remain the responsibility of the layer that serializes them.
/// Trie paths use little-endian scalar bytes, not numeric scalar order.
#[derive(Clone, Debug)]
pub struct Dict2 {
    trie: Trie,
}

impl Dict2 {
    /// Constructs an empty Scalar-keyed dictionary.
    pub fn new() -> Self {
        Self {
            trie: Trie::new(DICT2_KEY_BYTES).expect("32-byte trie keys fit in a Chunk"),
        }
    }

    /// Wraps a previously validated Scalar-keyed trie.
    ///
    /// The caller must establish that every leaf path is the little-endian encoding
    /// of a canonical Scalar; this cannot be rechecked through pruned branches.
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
    pub fn get_ref(&self, key: &Scalar) -> Result<Option<&ChunkRef>, VMError> {
        self.trie.get_ref(key.as_bytes())
    }

    /// Looks up resident data.
    pub fn get(&self, key: &Scalar) -> Result<Option<&Chunk>, VMError> {
        self.trie.get(key.as_bytes())
    }

    /// Inserts or replaces resident data and returns the prior reference.
    pub fn insert(&mut self, key: Scalar, value: Chunk) -> Result<Option<ChunkRef>, VMError> {
        self.trie.insert(key.as_bytes(), value)
    }

    /// Removes a key and returns its data reference.
    pub fn remove(&mut self, key: &Scalar) -> Result<Option<ChunkRef>, VMError> {
        self.trie.remove(key.as_bytes())
    }
}

impl Default for Dict2 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(n: u8) -> Chunk {
        Chunk::new(vec![n], vec![]).unwrap()
    }

    #[test]
    fn dictionary_paths_are_canonical_scalar_bytes() {
        let keys = [
            Scalar::ZERO,
            Scalar::ONE,
            Scalar::from(255u64),
            Scalar::from(256u64),
            Scalar::from(u128::MAX),
            -Scalar::ONE,
        ];
        let mut dict = Dict2::new();
        let mut trie = Trie::new(DICT2_KEY_BYTES).unwrap();
        for (i, key) in keys.iter().enumerate() {
            dict.insert(*key, value(i as u8)).unwrap();
            trie.insert(key.as_bytes(), value(i as u8)).unwrap();
        }
        assert_eq!(dict.root_id(), trie.root_id());

        let mut restored = Dict2::from_trusted_trie(trie).unwrap();
        for (i, key) in keys.iter().enumerate() {
            let expected = dict.trie().get_ref(key.as_bytes()).unwrap().unwrap().id();
            assert_eq!(restored.get_ref(key).unwrap().unwrap().id(), expected);
            assert_eq!(restored.get(key).unwrap().unwrap().payload(), &[i as u8]);
            assert_eq!(restored.remove(key).unwrap().unwrap().id(), expected);
        }
        assert!(restored.is_empty());
    }

    #[test]
    fn dictionary_operations_use_scalar_keys() {
        let mut dict = Dict2::new();
        assert!(dict.insert(Scalar::from(5i64), value(5)).unwrap().is_none());
        assert!(dict
            .insert(Scalar::from(-2i64), value(2))
            .unwrap()
            .is_none());
        assert_eq!(dict.len(), 2);
        assert_eq!(
            dict.get(&Scalar::from(-2i64)).unwrap().unwrap().payload(),
            &[2]
        );
        assert!(dict.get(&Scalar::ZERO).unwrap().is_none());

        let removed = dict.remove(&Scalar::from(5i64)).unwrap().unwrap();
        assert_eq!(removed.as_resident().unwrap().payload(), &[5]);
        assert_eq!(dict.len(), 1);
    }
}
