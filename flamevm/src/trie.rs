//! Radix-4 Patricia trie over fixed-width byte-string keys.

use crate::chunk::{Chunk, ChunkID, ChunkRef};
use crate::errors::VMError;
use std::convert::TryFrom;

const LEAF_MASK: u8 = 0;
const NODE_HEADER: usize = 3; // child mask + u16 label length

/// Largest fixed key supported by the recursive mutation implementation.
///
/// ponytail: keep recursion bounded to the current Int253 use; switch mutation
/// to iterative path rebuilding before increasing this limit.
pub const MAX_TRIE_KEY_BYTES: usize = 32;

/// A content-addressed radix-4 Patricia trie.
///
/// Keys have one configured byte length and are read MSB-first as 2-bit
/// digits. Every node is an ordinary [`Chunk`] whose payload is
/// `child_mask:u8 || label_len:u16be || packed_label`. A zero mask is a leaf
/// with one reference to the caller's data Chunk; otherwise the low four mask
/// bits select two to four child references in ascending selector order. A
/// required pruned child produces [`VMError::ChunkReferencePruned`] rather
/// than being treated as absence.
#[derive(Clone, Debug)]
pub struct Trie {
    key_bytes: usize,
    len: usize,
    root: Option<Chunk>,
}

impl Trie {
    /// Constructs an empty trie for keys of exactly `key_bytes` bytes.
    pub fn new(key_bytes: usize) -> Result<Self, VMError> {
        check_key_width(key_bytes)?;
        Ok(Self {
            key_bytes,
            len: 0,
            root: None,
        })
    }

    /// Reconstructs a trie envelope around an existing root Chunk.
    ///
    /// The supplied length is trusted envelope metadata. Resident node shape
    /// is checked lazily as operations traverse it. Descendant references may
    /// be pruned, but the root itself must be resident to form a usable trie.
    pub fn from_trusted_root(
        key_bytes: usize,
        len: usize,
        root: Option<Chunk>,
    ) -> Result<Self, VMError> {
        check_key_width(key_bytes)?;
        if (len == 0) != root.is_none() {
            return Err(VMError::MalformedTrie);
        }
        Ok(Self {
            key_bytes,
            len,
            root,
        })
    }

    /// Configured key width in bytes.
    pub fn key_bytes(&self) -> usize {
        self.key_bytes
    }

    /// Number of leaves recorded by the trie envelope.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true when the trie has no root.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrows the raw root Chunk.
    pub fn root(&self) -> Option<&Chunk> {
        self.root.as_ref()
    }

    /// Returns the root Chunk commitment, if non-empty.
    ///
    /// The enclosing schema must separately authenticate the configured key
    /// width and cached length; neither is part of the raw root Chunk.
    pub fn root_id(&self) -> Option<ChunkID> {
        self.root.as_ref().map(Chunk::id)
    }

    /// Removes the bookkeeping wrapper and returns the raw root Chunk.
    pub fn into_root(self) -> Option<Chunk> {
        self.root
    }

    /// Finds the leaf's data reference without requiring the data body itself.
    pub fn get_ref(&self, key: &[u8]) -> Result<Option<&ChunkRef>, VMError> {
        self.check_key(key)?;
        let mut current = match self.root.as_ref() {
            Some(root) => root,
            None => return Ok(None),
        };
        let total_digits = self.key_bytes * 4;
        let mut depth = 0;

        loop {
            let (label, mask) = parse_chunk(current, total_digits - depth)?;
            if !label_matches(&label, key, depth) {
                return Ok(None);
            }
            depth += label.len();

            if mask == LEAF_MASK {
                return Ok(Some(&current.refs()[0]));
            }
            let selector = key_digit(key, depth);
            depth += 1;
            if mask & (1 << selector) == 0 {
                return Ok(None);
            }
            current = resident(&current.refs()[child_index(mask, selector)])?;
        }
    }

    /// Finds and borrows resident leaf data.
    pub fn get(&self, key: &[u8]) -> Result<Option<&Chunk>, VMError> {
        match self.get_ref(key)? {
            Some(reference) => resident(reference).map(Some),
            None => Ok(None),
        }
    }

    /// Inserts or replaces resident leaf data.
    ///
    /// The previous data reference is returned without forcing a pruned value
    /// body to load. The trie is unchanged if traversal fails.
    pub fn insert(&mut self, key: &[u8], value: Chunk) -> Result<Option<ChunkRef>, VMError> {
        self.check_key(key)?;
        let value = ChunkRef::resident(value);
        let (root, prior) = match self.root.as_ref() {
            Some(root) => insert_chunk(root, 0, self.key_bytes * 4, key, value)?,
            None => (leaf_chunk(key_digits(key, 0), value)?, None),
        };
        let new_len = if prior.is_none() {
            self.len.checked_add(1).ok_or(VMError::MalformedTrie)?
        } else {
            self.len
        };
        self.root = Some(root);
        self.len = new_len;
        Ok(prior)
    }

    /// Removes a key and returns its data reference, if present.
    ///
    /// Collapsing a now-unary branch requires the surviving trie node to be
    /// resident. On any error the original root and length remain unchanged.
    pub fn remove(&mut self, key: &[u8]) -> Result<Option<ChunkRef>, VMError> {
        self.check_key(key)?;
        let root = match self.root.as_ref() {
            Some(root) => root,
            None => return Ok(None),
        };
        let (new_root, removed) = remove_chunk(root, 0, self.key_bytes * 4, key)?;
        if removed.is_some() {
            let new_len = self.len.checked_sub(1).ok_or(VMError::MalformedTrie)?;
            if (new_len == 0) != new_root.is_none() {
                return Err(VMError::MalformedTrie);
            }
            self.root = new_root;
            self.len = new_len;
        }
        Ok(removed)
    }

    fn check_key(&self, key: &[u8]) -> Result<(), VMError> {
        if key.len() != self.key_bytes {
            return Err(VMError::TrieKeyLengthMismatch {
                expected: self.key_bytes,
                actual: key.len(),
            });
        }
        Ok(())
    }
}

fn check_key_width(key_bytes: usize) -> Result<(), VMError> {
    if key_bytes > MAX_TRIE_KEY_BYTES {
        return Err(VMError::TrieKeyTooLong);
    }
    Ok(())
}

fn resident(reference: &ChunkRef) -> Result<&Chunk, VMError> {
    reference
        .as_resident()
        .ok_or_else(|| VMError::ChunkReferencePruned(reference.id()))
}

fn parse_chunk(chunk: &Chunk, remaining_digits: usize) -> Result<(Vec<u8>, u8), VMError> {
    let payload = chunk.payload();
    if payload.len() < NODE_HEADER {
        return Err(VMError::MalformedTrie);
    }
    let mask = payload[0];
    let label_len = u16::from_be_bytes([payload[1], payload[2]]) as usize;
    if label_len > remaining_digits {
        return Err(VMError::MalformedTrie);
    }
    let packed_len = label_len.div_ceil(4);
    let label_end = NODE_HEADER + packed_len;
    if payload.len() != label_end || !padding_is_zero(&payload[NODE_HEADER..label_end], label_len) {
        return Err(VMError::MalformedTrie);
    }
    let label = unpack_digits(&payload[NODE_HEADER..label_end], label_len);

    let child_count = mask.count_ones() as usize;
    if mask == LEAF_MASK {
        if label_len != remaining_digits || chunk.refs().len() != 1 {
            return Err(VMError::MalformedTrie);
        }
    } else if mask & 0xf0 != 0
        || !(2..=4).contains(&child_count)
        || chunk.refs().len() != child_count
        || label_len == remaining_digits
    {
        return Err(VMError::MalformedTrie);
    }
    Ok((label, mask))
}

fn make_chunk(label: Vec<u8>, mask: u8, refs: Vec<ChunkRef>) -> Result<Chunk, VMError> {
    let label_len = u16::try_from(label.len()).map_err(|_| VMError::MalformedTrie)?;
    if label.iter().any(|digit| *digit > 3) {
        return Err(VMError::MalformedTrie);
    }
    let child_count = mask.count_ones() as usize;
    if (mask == LEAF_MASK && refs.len() != 1)
        || (mask != LEAF_MASK
            && (mask & 0xf0 != 0 || !(2..=4).contains(&child_count) || refs.len() != child_count))
    {
        return Err(VMError::MalformedTrie);
    }

    let mut payload = Vec::with_capacity(NODE_HEADER + label.len().div_ceil(4));
    payload.push(mask);
    payload.extend_from_slice(&label_len.to_be_bytes());
    payload.extend_from_slice(&pack_digits(&label));
    Chunk::new(payload, refs)
}

fn insert_chunk(
    chunk: &Chunk,
    depth: usize,
    total_digits: usize,
    key: &[u8],
    value: ChunkRef,
) -> Result<(Chunk, Option<ChunkRef>), VMError> {
    let (label, mask) = parse_chunk(chunk, total_digits - depth)?;
    let common = common_prefix(&label, key, depth);

    if common < label.len() {
        let old_selector = label[common];
        let new_selector = key_digit(key, depth + common);
        let old_child = ChunkRef::resident(make_chunk(
            label[common + 1..].to_vec(),
            mask,
            chunk.refs().to_vec(),
        )?);
        let new_child = ChunkRef::resident(leaf_chunk(key_digits(key, depth + common + 1), value)?);
        let branch = branch_chunk(
            label[..common].to_vec(),
            vec![(old_selector, old_child), (new_selector, new_child)],
        )?;
        return Ok((branch, None));
    }

    let next_depth = depth + label.len();
    if mask == LEAF_MASK {
        return Ok((leaf_chunk(label, value)?, Some(chunk.refs()[0].clone())));
    }

    let selector = key_digit(key, next_depth);
    let child_depth = next_depth + 1;
    let bit = 1 << selector;
    let index = child_index(mask, selector);
    let mut new_mask = mask;
    let mut children = chunk.refs().to_vec();
    let prior;
    if mask & bit == 0 {
        children.insert(
            index,
            ChunkRef::resident(leaf_chunk(key_digits(key, child_depth), value)?),
        );
        new_mask |= bit;
        prior = None;
    } else {
        let child = resident(&children[index])?;
        let (replacement, old) = insert_chunk(child, child_depth, total_digits, key, value)?;
        children[index] = ChunkRef::resident(replacement);
        prior = old;
    }
    Ok((make_chunk(label, new_mask, children)?, prior))
}

fn remove_chunk(
    chunk: &Chunk,
    depth: usize,
    total_digits: usize,
    key: &[u8],
) -> Result<(Option<Chunk>, Option<ChunkRef>), VMError> {
    let (label, mask) = parse_chunk(chunk, total_digits - depth)?;
    if !label_matches(&label, key, depth) {
        return Ok((Some(chunk.clone()), None));
    }
    let next_depth = depth + label.len();

    if mask == LEAF_MASK {
        return Ok((None, Some(chunk.refs()[0].clone())));
    }

    let selector = key_digit(key, next_depth);
    let bit = 1 << selector;
    if mask & bit == 0 {
        return Ok((Some(chunk.clone()), None));
    }
    let index = child_index(mask, selector);
    let child_depth = next_depth + 1;
    let child = resident(&chunk.refs()[index])?;
    let (replacement, removed) = remove_chunk(child, child_depth, total_digits, key)?;
    if removed.is_none() {
        return Ok((Some(chunk.clone()), None));
    }

    let mut new_mask = mask;
    let mut children = chunk.refs().to_vec();
    match replacement {
        Some(child) => children[index] = ChunkRef::resident(child),
        None => {
            children.remove(index);
            new_mask &= !bit;
        }
    }

    let rebuilt = if children.len() >= 2 {
        make_chunk(label, new_mask, children)?
    } else {
        let survivor_selector = new_mask.trailing_zeros() as u8;
        let survivor = children.pop().ok_or(VMError::MalformedTrie)?;
        let survivor_chunk = resident(&survivor)?;
        let (survivor_label, survivor_mask) =
            parse_chunk(survivor_chunk, total_digits - child_depth)?;
        let mut merged = label;
        merged.push(survivor_selector);
        merged.extend_from_slice(&survivor_label);
        make_chunk(merged, survivor_mask, survivor_chunk.refs().to_vec())?
    };
    Ok((Some(rebuilt), removed))
}

fn leaf_chunk(label: Vec<u8>, value: ChunkRef) -> Result<Chunk, VMError> {
    make_chunk(label, LEAF_MASK, vec![value])
}

fn branch_chunk(label: Vec<u8>, mut entries: Vec<(u8, ChunkRef)>) -> Result<Chunk, VMError> {
    entries.sort_by_key(|(selector, _)| *selector);
    if entries.len() < 2
        || entries.len() > 4
        || entries.windows(2).any(|w| w[0].0 == w[1].0)
        || entries.iter().any(|(selector, _)| *selector > 3)
    {
        return Err(VMError::MalformedTrie);
    }
    let mut mask = 0u8;
    let mut children = Vec::with_capacity(entries.len());
    for (selector, child) in entries {
        mask |= 1 << selector;
        children.push(child);
    }
    make_chunk(label, mask, children)
}

fn key_digit(key: &[u8], digit: usize) -> u8 {
    (key[digit / 4] >> (6 - 2 * (digit % 4))) & 3
}

fn key_digits(key: &[u8], start: usize) -> Vec<u8> {
    (start..key.len() * 4).map(|i| key_digit(key, i)).collect()
}

fn common_prefix(label: &[u8], key: &[u8], depth: usize) -> usize {
    label
        .iter()
        .enumerate()
        .take_while(|(i, digit)| **digit == key_digit(key, depth + i))
        .count()
}

fn label_matches(label: &[u8], key: &[u8], depth: usize) -> bool {
    common_prefix(label, key, depth) == label.len()
}

fn child_index(mask: u8, selector: u8) -> usize {
    (mask & ((1 << selector) - 1)).count_ones() as usize
}

fn pack_digits(digits: &[u8]) -> Vec<u8> {
    let mut packed = vec![0u8; digits.len().div_ceil(4)];
    for (i, digit) in digits.iter().enumerate() {
        packed[i / 4] |= digit << (6 - 2 * (i % 4));
    }
    packed
}

fn unpack_digits(packed: &[u8], len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (packed[i / 4] >> (6 - 2 * (i % 4))) & 3)
        .collect()
}

fn padding_is_zero(packed: &[u8], digits: usize) -> bool {
    let used = digits % 4;
    if used == 0 {
        return true;
    }
    let unused_bits = 8 - used * 2;
    packed
        .last()
        .is_some_and(|last| last & ((1 << unused_bits) - 1) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(n: u8) -> Chunk {
        Chunk::new(vec![n], vec![]).unwrap()
    }

    fn payload(reference: ChunkRef) -> u8 {
        reference.as_resident().unwrap().payload()[0]
    }

    #[test]
    fn insert_replace_lookup_and_remove() {
        let mut trie = Trie::new(1).unwrap();
        for (key, n) in [([0x00], 1), ([0x01], 2), ([0x40], 3), ([0xff], 4)] {
            assert!(trie.insert(&key, value(n)).unwrap().is_none());
        }
        assert_eq!(trie.len(), 4);
        assert_eq!(trie.get(&[0x40]).unwrap().unwrap().payload(), &[3]);
        assert!(trie.get(&[0x80]).unwrap().is_none());

        let prior = trie.insert(&[0x40], value(9)).unwrap().unwrap();
        assert_eq!(payload(prior), 3);
        assert_eq!(trie.len(), 4);
        assert_eq!(trie.get(&[0x40]).unwrap().unwrap().payload(), &[9]);

        let removed = trie.remove(&[0x01]).unwrap().unwrap();
        assert_eq!(payload(removed), 2);
        assert_eq!(trie.len(), 3);
        assert!(trie.remove(&[0x01]).unwrap().is_none());
    }

    #[test]
    fn all_one_byte_keys_roundtrip_and_drain() {
        let mut trie = Trie::new(1).unwrap();
        for key in 0u8..=u8::MAX {
            trie.insert(&[key], value(key)).unwrap();
        }
        for key in 0u8..=u8::MAX {
            assert_eq!(trie.get(&[key]).unwrap().unwrap().payload(), &[key]);
        }
        for key in (0u8..=u8::MAX).rev() {
            assert_eq!(payload(trie.remove(&[key]).unwrap().unwrap()), key);
        }
        assert!(trie.is_empty());
        assert!(trie.root().is_none());
    }

    #[test]
    fn one_bit_divergence_needs_no_alignment_node() {
        let mut trie = Trie::new(1).unwrap();
        trie.insert(&[0b0000_0000], value(1)).unwrap();
        trie.insert(&[0b0100_0000], value(2)).unwrap();

        let root = trie.root().unwrap();
        assert_eq!(root.payload(), &[0b0011, 0, 0]);
        let (label, mask) = parse_chunk(root, 4).unwrap();
        assert!(label.is_empty());
        assert_eq!(mask, 0b0011);
        assert_eq!(root.refs().len(), 2);
    }

    #[test]
    fn wrapper_unwraps_to_the_canonical_root_chunk() {
        let mut trie = Trie::new(1).unwrap();
        trie.insert(&[0x12], value(7)).unwrap();

        let root = trie.into_root().unwrap();
        assert_eq!(root.payload(), &[LEAF_MASK, 0, 4, 0x12]);
        let root_id = root.id();

        let trie = Trie::from_trusted_root(1, 1, Some(root)).unwrap();
        assert_eq!(trie.root_id(), Some(root_id));
        assert_eq!(trie.get(&[0x12]).unwrap().unwrap().payload(), &[7]);
        assert!(Trie::new(1).unwrap().into_root().is_none());
    }

    #[test]
    fn root_is_independent_of_insertion_order() {
        let mut forward = Trie::new(1).unwrap();
        let mut reverse = Trie::new(1).unwrap();
        let entries = [(0x00, 1), (0x01, 2), (0x40, 3), (0xff, 4)];
        for (key, item) in entries {
            forward.insert(&[key], value(item)).unwrap();
        }
        for &(key, item) in entries.iter().rev() {
            reverse.insert(&[key], value(item)).unwrap();
        }
        assert_eq!(forward.root_id(), reverse.root_id());
    }

    #[test]
    fn trusted_root_rejects_a_noncanonical_unary_branch_on_access() {
        let child = ChunkRef::resident(value(1));
        let root = Chunk::new(vec![0b0001, 0, 0], vec![child]).unwrap();
        let trie = Trie::from_trusted_root(1, 1, Some(root)).unwrap();

        assert!(matches!(trie.get(&[0]), Err(VMError::MalformedTrie)));
    }

    #[test]
    fn deleting_to_one_leaf_collapses_the_branch() {
        let mut trie = Trie::new(2).unwrap();
        trie.insert(&[0xaa, 0x00], value(1)).unwrap();
        trie.insert(&[0xaa, 0x01], value(2)).unwrap();
        trie.remove(&[0xaa, 0x00]).unwrap();

        let root = trie.root().unwrap();
        assert_eq!(parse_chunk(root, 8).unwrap().1, LEAF_MASK);
        assert_eq!(trie.get(&[0xaa, 0x01]).unwrap().unwrap().payload(), &[2]);
    }

    #[test]
    fn pruned_paths_fail_without_changing_the_root() {
        let mut trie = Trie::new(1).unwrap();
        trie.insert(&[0x00], value(1)).unwrap();
        trie.insert(&[0x40], value(2)).unwrap();
        let len = trie.len();
        let root_chunk = trie.into_root().unwrap();
        let root_id = root_chunk.id();
        let (payload, mut refs) = root_chunk.into_parts();
        refs[0].prune();
        let root = Chunk::new(payload, refs).unwrap();
        assert_eq!(root.id(), root_id);

        let mut trie = Trie::from_trusted_root(1, len, Some(root)).unwrap();
        assert!(matches!(
            trie.get(&[0x00]),
            Err(VMError::ChunkReferencePruned(_))
        ));
        assert_eq!(trie.get(&[0x40]).unwrap().unwrap().payload(), &[2]);

        let before = trie.root_id();
        assert!(matches!(
            trie.remove(&[0x00]),
            Err(VMError::ChunkReferencePruned(_))
        ));
        assert_eq!(trie.root_id(), before);

        // An absent root selector can still be inserted without opening siblings.
        trie.insert(&[0x80], value(3)).unwrap();
        assert_eq!(trie.get(&[0x80]).unwrap().unwrap().payload(), &[3]);
    }

    #[test]
    fn deletion_does_not_collapse_through_a_pruned_survivor() {
        let mut trie = Trie::new(1).unwrap();
        trie.insert(&[0x00], value(1)).unwrap();
        trie.insert(&[0x40], value(2)).unwrap();
        let root = trie.into_root().unwrap();
        let (payload, mut refs) = root.into_parts();
        refs[1].prune();
        let root = Chunk::new(payload, refs).unwrap();

        let mut trie = Trie::from_trusted_root(1, 2, Some(root)).unwrap();
        let before = trie.root_id();
        assert!(matches!(
            trie.remove(&[0x00]),
            Err(VMError::ChunkReferencePruned(_))
        ));
        assert_eq!(trie.root_id(), before);
        assert_eq!(trie.len(), 2);
    }

    #[test]
    fn validates_key_width() {
        let trie = Trie::new(2).unwrap();
        assert!(matches!(
            trie.get(&[0]),
            Err(VMError::TrieKeyLengthMismatch {
                expected: 2,
                actual: 1
            })
        ));
        assert!(matches!(
            Trie::new(MAX_TRIE_KEY_BYTES + 1),
            Err(VMError::TrieKeyTooLong)
        ));
    }

    #[test]
    fn trusted_metadata_cannot_overflow_or_commit_an_impossible_empty_root() {
        let mut original = Trie::new(1).unwrap();
        original.insert(&[0], value(1)).unwrap();
        let root = original.into_root().unwrap();

        let mut overflowing = Trie::from_trusted_root(1, usize::MAX, Some(root.clone())).unwrap();
        let before = overflowing.root_id();
        assert!(matches!(
            overflowing.insert(&[0x40], value(2)),
            Err(VMError::MalformedTrie)
        ));
        assert_eq!(overflowing.root_id(), before);
        assert_eq!(overflowing.len(), usize::MAX);

        let mut wrong_count = Trie::from_trusted_root(1, 2, Some(root)).unwrap();
        let before = wrong_count.root_id();
        assert!(matches!(
            wrong_count.remove(&[0]),
            Err(VMError::MalformedTrie)
        ));
        assert_eq!(wrong_count.root_id(), before);
        assert_eq!(wrong_count.len(), 2);
    }
}
