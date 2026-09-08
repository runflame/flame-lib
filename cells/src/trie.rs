//! Radix-4 Patricia trie over fixed-width byte-string keys.

use crate::{Cell, CellError, CellID, CellRef, CellResolver, resolve_cell};
use std::{convert::TryFrom, sync::Arc};

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
/// digits. Every node is an ordinary [`Cell`] whose payload is
/// `child_mask:u8 || label_len:u16be || packed_label`. A zero mask is a leaf
/// with one reference to the caller's data Cell; otherwise the low four mask
/// bits select two to four child references in ascending selector order.
#[derive(Clone, Debug)]
pub struct Trie {
    key_bytes: usize,
    len: usize,
    root: Option<CellRef>,
}

impl Trie {
    /// Constructs an empty trie for keys of exactly `key_bytes` bytes.
    pub fn new(key_bytes: usize) -> Result<Self, CellError> {
        check_key_width(key_bytes)?;
        Ok(Self {
            key_bytes,
            len: 0,
            root: None,
        })
    }

    /// Reconstructs a trie envelope around an authenticated root reference.
    ///
    /// The supplied length is trusted envelope metadata. Node shape is checked
    /// lazily as operations traverse it.
    pub fn from_trusted_root(
        key_bytes: usize,
        len: usize,
        root: Option<CellRef>,
    ) -> Result<Self, CellError> {
        check_key_width(key_bytes)?;
        if (len == 0) != root.is_none() {
            return Err(CellError::MalformedTrie);
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

    /// Borrows the raw root reference.
    pub fn root(&self) -> Option<&CellRef> {
        self.root.as_ref()
    }

    /// Returns the root Cell commitment, if non-empty.
    ///
    /// The enclosing schema must separately authenticate the configured key
    /// width and cached length; neither is part of the raw root Cell.
    pub fn root_id(&self) -> Option<CellID> {
        self.root.as_ref().map(CellRef::id)
    }

    /// Removes the bookkeeping wrapper and returns the raw root reference.
    pub fn into_root(self) -> Option<CellRef> {
        self.root
    }

    /// Finds the leaf's data reference without requiring the data body itself.
    pub fn get_ref<R: CellResolver + ?Sized>(
        &self,
        key: &[u8],
        resolver: &mut R,
    ) -> Result<Option<CellRef>, CellError> {
        self.check_key(key)?;
        let mut current = match self.root.as_ref() {
            Some(root) => resolve_cell(resolver, root)?,
            None => return Ok(None),
        };
        let total_digits = self.key_bytes * 4;
        let mut depth = 0;

        loop {
            let (label, mask) = parse_cell(&current, total_digits - depth)?;
            if !label_matches(&label, key, depth) {
                return Ok(None);
            }
            depth += label.len();

            if mask == LEAF_MASK {
                return Ok(Some(current.refs()[0].clone()));
            }
            let selector = key_digit(key, depth);
            depth += 1;
            if mask & (1 << selector) == 0 {
                return Ok(None);
            }
            current = resolve_cell(resolver, &current.refs()[child_index(mask, selector)])?;
        }
    }

    /// Finds resident or resolver-backed leaf data.
    pub fn get<R: CellResolver + ?Sized>(
        &self,
        key: &[u8],
        resolver: &mut R,
    ) -> Result<Option<Arc<Cell>>, CellError> {
        match self.get_ref(key, resolver)? {
            Some(reference) => resolve_cell(resolver, &reference).map(Some),
            None => Ok(None),
        }
    }

    /// Inserts or replaces a leaf.
    ///
    /// The previous data reference is returned without forcing its body to
    /// load. The trie is unchanged if traversal fails.
    pub fn insert<R: CellResolver + ?Sized>(
        &mut self,
        key: &[u8],
        value: Cell,
        resolver: &mut R,
    ) -> Result<Option<CellRef>, CellError> {
        self.check_key(key)?;
        let value = CellRef::resident(value);
        let (root, prior) = match self.root.as_ref() {
            Some(root) => {
                let root = resolve_cell(resolver, root)?;
                insert_cell(&root, 0, self.key_bytes * 4, key, value, resolver)?
            }
            None => (leaf_cell(key_digits(key, 0), value)?, None),
        };
        let new_len = if prior.is_none() {
            self.len.checked_add(1).ok_or(CellError::MalformedTrie)?
        } else {
            self.len
        };
        self.root = Some(CellRef::resident(root));
        self.len = new_len;
        Ok(prior)
    }

    /// Removes a key and returns its data reference, if present.
    ///
    /// Collapsing a now-unary branch resolves the surviving trie node. On any
    /// error the original root and length remain unchanged.
    pub fn remove<R: CellResolver + ?Sized>(
        &mut self,
        key: &[u8],
        resolver: &mut R,
    ) -> Result<Option<CellRef>, CellError> {
        self.check_key(key)?;
        let root = match self.root.as_ref() {
            Some(root) => resolve_cell(resolver, root)?,
            None => return Ok(None),
        };
        let (new_root, removed) = remove_cell(&root, 0, self.key_bytes * 4, key, resolver)?;
        if removed.is_some() {
            let new_len = self.len.checked_sub(1).ok_or(CellError::MalformedTrie)?;
            if (new_len == 0) != new_root.is_none() {
                return Err(CellError::MalformedTrie);
            }
            self.root = new_root.map(CellRef::resident);
            self.len = new_len;
        }
        Ok(removed)
    }

    fn check_key(&self, key: &[u8]) -> Result<(), CellError> {
        if key.len() != self.key_bytes {
            return Err(CellError::TrieKeyLengthMismatch {
                expected: self.key_bytes,
                actual: key.len(),
            });
        }
        Ok(())
    }
}

fn check_key_width(key_bytes: usize) -> Result<(), CellError> {
    if key_bytes > MAX_TRIE_KEY_BYTES {
        return Err(CellError::TrieKeyTooLong {
            actual: key_bytes,
            max: MAX_TRIE_KEY_BYTES,
        });
    }
    Ok(())
}

fn parse_cell(cell: &Cell, remaining_digits: usize) -> Result<(Vec<u8>, u8), CellError> {
    let payload = cell.payload();
    if payload.len() < NODE_HEADER {
        return Err(CellError::MalformedTrie);
    }
    let mask = payload[0];
    let label_len = u16::from_be_bytes([payload[1], payload[2]]) as usize;
    if label_len > remaining_digits {
        return Err(CellError::MalformedTrie);
    }
    let packed_len = label_len.div_ceil(4);
    let label_end = NODE_HEADER + packed_len;
    if payload.len() != label_end || !padding_is_zero(&payload[NODE_HEADER..label_end], label_len) {
        return Err(CellError::MalformedTrie);
    }
    let label = unpack_digits(&payload[NODE_HEADER..label_end], label_len);

    let child_count = mask.count_ones() as usize;
    if mask == LEAF_MASK {
        if label_len != remaining_digits || cell.refs().len() != 1 {
            return Err(CellError::MalformedTrie);
        }
    } else if mask & 0xf0 != 0
        || !(2..=4).contains(&child_count)
        || cell.refs().len() != child_count
        || label_len == remaining_digits
    {
        return Err(CellError::MalformedTrie);
    }
    Ok((label, mask))
}

fn make_cell(label: Vec<u8>, mask: u8, refs: Vec<CellRef>) -> Result<Cell, CellError> {
    let label_len = u16::try_from(label.len()).map_err(|_| CellError::MalformedTrie)?;
    if label.iter().any(|digit| *digit > 3) {
        return Err(CellError::MalformedTrie);
    }
    let child_count = mask.count_ones() as usize;
    if (mask == LEAF_MASK && refs.len() != 1)
        || (mask != LEAF_MASK
            && (mask & 0xf0 != 0 || !(2..=4).contains(&child_count) || refs.len() != child_count))
    {
        return Err(CellError::MalformedTrie);
    }

    let mut payload = Vec::with_capacity(NODE_HEADER + label.len().div_ceil(4));
    payload.push(mask);
    payload.extend_from_slice(&label_len.to_be_bytes());
    payload.extend_from_slice(&pack_digits(&label));
    Cell::new(payload, refs)
}

fn insert_cell<R: CellResolver + ?Sized>(
    cell: &Cell,
    depth: usize,
    total_digits: usize,
    key: &[u8],
    value: CellRef,
    resolver: &mut R,
) -> Result<(Cell, Option<CellRef>), CellError> {
    let (label, mask) = parse_cell(cell, total_digits - depth)?;
    let common = common_prefix(&label, key, depth);

    if common < label.len() {
        let old_selector = label[common];
        let new_selector = key_digit(key, depth + common);
        let old_child = CellRef::resident(make_cell(
            label[common + 1..].to_vec(),
            mask,
            cell.refs().to_vec(),
        )?);
        let new_child = CellRef::resident(leaf_cell(key_digits(key, depth + common + 1), value)?);
        let branch = branch_cell(
            label[..common].to_vec(),
            vec![(old_selector, old_child), (new_selector, new_child)],
        )?;
        return Ok((branch, None));
    }

    let next_depth = depth + label.len();
    if mask == LEAF_MASK {
        return Ok((leaf_cell(label, value)?, Some(cell.refs()[0].clone())));
    }

    let selector = key_digit(key, next_depth);
    let child_depth = next_depth + 1;
    let bit = 1 << selector;
    let index = child_index(mask, selector);
    let mut new_mask = mask;
    let mut children = cell.refs().to_vec();
    let prior;
    if mask & bit == 0 {
        children.insert(
            index,
            CellRef::resident(leaf_cell(key_digits(key, child_depth), value)?),
        );
        new_mask |= bit;
        prior = None;
    } else {
        let child = resolve_cell(resolver, &children[index])?;
        let (replacement, old) =
            insert_cell(&child, child_depth, total_digits, key, value, resolver)?;
        children[index] = CellRef::resident(replacement);
        prior = old;
    }
    Ok((make_cell(label, new_mask, children)?, prior))
}

fn remove_cell<R: CellResolver + ?Sized>(
    cell: &Cell,
    depth: usize,
    total_digits: usize,
    key: &[u8],
    resolver: &mut R,
) -> Result<(Option<Cell>, Option<CellRef>), CellError> {
    let (label, mask) = parse_cell(cell, total_digits - depth)?;
    if !label_matches(&label, key, depth) {
        return Ok((Some(cell.clone()), None));
    }
    let next_depth = depth + label.len();

    if mask == LEAF_MASK {
        return Ok((None, Some(cell.refs()[0].clone())));
    }

    let selector = key_digit(key, next_depth);
    let bit = 1 << selector;
    if mask & bit == 0 {
        return Ok((Some(cell.clone()), None));
    }
    let index = child_index(mask, selector);
    let child_depth = next_depth + 1;
    let child = resolve_cell(resolver, &cell.refs()[index])?;
    let (replacement, removed) = remove_cell(&child, child_depth, total_digits, key, resolver)?;
    if removed.is_none() {
        return Ok((Some(cell.clone()), None));
    }

    let mut new_mask = mask;
    let mut children = cell.refs().to_vec();
    match replacement {
        Some(child) => children[index] = CellRef::resident(child),
        None => {
            children.remove(index);
            new_mask &= !bit;
        }
    }

    let rebuilt = if children.len() >= 2 {
        make_cell(label, new_mask, children)?
    } else {
        let survivor_selector = new_mask.trailing_zeros() as u8;
        let survivor = children.pop().ok_or(CellError::MalformedTrie)?;
        let survivor = resolve_cell(resolver, &survivor)?;
        let (survivor_label, survivor_mask) = parse_cell(&survivor, total_digits - child_depth)?;
        let mut merged = label;
        merged.push(survivor_selector);
        merged.extend_from_slice(&survivor_label);
        make_cell(merged, survivor_mask, survivor.refs().to_vec())?
    };
    Ok((Some(rebuilt), removed))
}

fn leaf_cell(label: Vec<u8>, value: CellRef) -> Result<Cell, CellError> {
    make_cell(label, LEAF_MASK, vec![value])
}

fn branch_cell(label: Vec<u8>, mut entries: Vec<(u8, CellRef)>) -> Result<Cell, CellError> {
    entries.sort_by_key(|(selector, _)| *selector);
    if entries.len() < 2
        || entries.len() > 4
        || entries.windows(2).any(|w| w[0].0 == w[1].0)
        || entries.iter().any(|(selector, _)| *selector > 3)
    {
        return Err(CellError::MalformedTrie);
    }
    let mut mask = 0u8;
    let mut children = Vec::with_capacity(entries.len());
    for (selector, child) in entries {
        mask |= 1 << selector;
        children.push(child);
    }
    make_cell(label, mask, children)
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
    use std::collections::HashMap;

    fn value(n: u8) -> Cell {
        Cell::new(vec![n], vec![]).unwrap()
    }

    fn payload(reference: CellRef) -> u8 {
        reference.as_resident().unwrap().payload()[0]
    }

    fn resident(reference: &CellRef) -> &Cell {
        reference.as_resident().unwrap()
    }

    #[test]
    fn insert_replace_lookup_and_remove() {
        let mut trie = Trie::new(1).unwrap();
        let mut resolver = ();
        for (key, n) in [([0x00], 1), ([0x01], 2), ([0x40], 3), ([0xff], 4)] {
            assert!(
                trie.insert(&key, value(n), &mut resolver)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(trie.len(), 4);
        assert_eq!(
            trie.get(&[0x40], &mut resolver).unwrap().unwrap().payload(),
            &[3]
        );
        assert!(trie.get(&[0x80], &mut resolver).unwrap().is_none());

        let prior = trie
            .insert(&[0x40], value(9), &mut resolver)
            .unwrap()
            .unwrap();
        assert_eq!(payload(prior), 3);
        assert_eq!(trie.len(), 4);
        assert_eq!(
            trie.get(&[0x40], &mut resolver).unwrap().unwrap().payload(),
            &[9]
        );

        let removed = trie.remove(&[0x01], &mut resolver).unwrap().unwrap();
        assert_eq!(payload(removed), 2);
        assert_eq!(trie.len(), 3);
        assert!(trie.remove(&[0x01], &mut resolver).unwrap().is_none());
    }

    #[test]
    fn all_one_byte_keys_roundtrip_and_drain() {
        let mut trie = Trie::new(1).unwrap();
        let mut resolver = ();
        for key in 0u8..=u8::MAX {
            trie.insert(&[key], value(key), &mut resolver).unwrap();
        }
        for key in 0u8..=u8::MAX {
            assert_eq!(
                trie.get(&[key], &mut resolver).unwrap().unwrap().payload(),
                &[key]
            );
        }
        for key in (0u8..=u8::MAX).rev() {
            assert_eq!(
                payload(trie.remove(&[key], &mut resolver).unwrap().unwrap()),
                key
            );
        }
        assert!(trie.is_empty());
        assert!(trie.root().is_none());
    }

    #[test]
    fn one_bit_divergence_needs_no_alignment_node() {
        let mut trie = Trie::new(1).unwrap();
        let mut resolver = ();
        trie.insert(&[0b0000_0000], value(1), &mut resolver)
            .unwrap();
        trie.insert(&[0b0100_0000], value(2), &mut resolver)
            .unwrap();

        let root = resident(trie.root().unwrap());
        assert_eq!(root.payload(), &[0b0011, 0, 0]);
        let (label, mask) = parse_cell(root, 4).unwrap();
        assert!(label.is_empty());
        assert_eq!(mask, 0b0011);
        assert_eq!(root.refs().len(), 2);
    }

    #[test]
    fn wrapper_unwraps_to_the_canonical_root_cell() {
        let mut trie = Trie::new(1).unwrap();
        let mut resolver = ();
        trie.insert(&[0x12], value(7), &mut resolver).unwrap();

        let root = trie.into_root().unwrap();
        assert_eq!(resident(&root).payload(), &[LEAF_MASK, 0, 4, 0x12]);
        let root_id = root.id();

        let trie = Trie::from_trusted_root(1, 1, Some(root)).unwrap();
        assert_eq!(trie.root_id(), Some(root_id));
        assert_eq!(
            trie.get(&[0x12], &mut resolver).unwrap().unwrap().payload(),
            &[7]
        );
        assert!(Trie::new(1).unwrap().into_root().is_none());
    }

    #[test]
    fn root_is_independent_of_insertion_order() {
        let mut forward = Trie::new(1).unwrap();
        let mut reverse = Trie::new(1).unwrap();
        let mut resolver = ();
        let entries = [(0x00, 1), (0x01, 2), (0x40, 3), (0xff, 4)];
        for (key, item) in entries {
            forward.insert(&[key], value(item), &mut resolver).unwrap();
        }
        for &(key, item) in entries.iter().rev() {
            reverse.insert(&[key], value(item), &mut resolver).unwrap();
        }
        assert_eq!(forward.root_id(), reverse.root_id());
    }

    #[test]
    fn trusted_root_rejects_a_noncanonical_unary_branch_on_access() {
        let child = CellRef::resident(value(1));
        let root = Cell::new(vec![0b0001, 0, 0], vec![child]).unwrap();
        let trie = Trie::from_trusted_root(1, 1, Some(CellRef::resident(root))).unwrap();

        assert!(matches!(
            trie.get(&[0], &mut ()),
            Err(CellError::MalformedTrie)
        ));
    }

    #[test]
    fn deleting_to_one_leaf_collapses_the_branch() {
        let mut trie = Trie::new(2).unwrap();
        let mut resolver = ();
        trie.insert(&[0xaa, 0x00], value(1), &mut resolver).unwrap();
        trie.insert(&[0xaa, 0x01], value(2), &mut resolver).unwrap();
        trie.remove(&[0xaa, 0x00], &mut resolver).unwrap();

        let root = resident(trie.root().unwrap());
        assert_eq!(parse_cell(root, 8).unwrap().1, LEAF_MASK);
        assert_eq!(
            trie.get(&[0xaa, 0x01], &mut resolver)
                .unwrap()
                .unwrap()
                .payload(),
            &[2]
        );
    }

    #[test]
    fn pruned_paths_fail_without_changing_the_root() {
        let mut trie = Trie::new(1).unwrap();
        let mut resolver = ();
        trie.insert(&[0x00], value(1), &mut resolver).unwrap();
        trie.insert(&[0x40], value(2), &mut resolver).unwrap();
        let len = trie.len();
        let root_ref = trie.into_root().unwrap();
        let root_id = root_ref.id();
        let (payload, mut refs) = resident(&root_ref).clone().into_parts();
        refs[0] = CellRef::pruned(refs[0].id());
        let root = Cell::new(payload, refs).unwrap();
        assert_eq!(root.id(), root_id);

        let mut trie = Trie::from_trusted_root(1, len, Some(CellRef::resident(root))).unwrap();
        assert!(matches!(
            trie.get(&[0x00], &mut resolver),
            Err(CellError::MissingCell(_))
        ));
        assert_eq!(
            trie.get(&[0x40], &mut resolver).unwrap().unwrap().payload(),
            &[2]
        );

        let before = trie.root_id();
        assert!(matches!(
            trie.remove(&[0x00], &mut resolver),
            Err(CellError::MissingCell(_))
        ));
        assert_eq!(trie.root_id(), before);

        // An absent root selector can still be inserted without opening siblings.
        trie.insert(&[0x80], value(3), &mut resolver).unwrap();
        assert_eq!(
            trie.get(&[0x80], &mut resolver).unwrap().unwrap().payload(),
            &[3]
        );
    }

    #[test]
    fn deletion_does_not_collapse_through_a_pruned_survivor() {
        let mut trie = Trie::new(1).unwrap();
        let mut resolver = ();
        trie.insert(&[0x00], value(1), &mut resolver).unwrap();
        trie.insert(&[0x40], value(2), &mut resolver).unwrap();
        let root_ref = trie.into_root().unwrap();
        let (payload, mut refs) = resident(&root_ref).clone().into_parts();
        refs[1] = CellRef::pruned(refs[1].id());
        let root = Cell::new(payload, refs).unwrap();

        let mut trie = Trie::from_trusted_root(1, 2, Some(CellRef::resident(root))).unwrap();
        let before = trie.root_id();
        assert!(matches!(
            trie.remove(&[0x00], &mut resolver),
            Err(CellError::MissingCell(_))
        ));
        assert_eq!(trie.root_id(), before);
        assert_eq!(trie.len(), 2);
    }

    #[test]
    fn validates_key_width() {
        let trie = Trie::new(2).unwrap();
        assert!(matches!(
            trie.get(&[0], &mut ()),
            Err(CellError::TrieKeyLengthMismatch {
                expected: 2,
                actual: 1
            })
        ));
        assert!(matches!(
            Trie::new(MAX_TRIE_KEY_BYTES + 1),
            Err(CellError::TrieKeyTooLong {
                actual,
                max: MAX_TRIE_KEY_BYTES
            }) if actual == MAX_TRIE_KEY_BYTES + 1
        ));
    }

    #[test]
    fn trusted_metadata_cannot_overflow_or_commit_an_impossible_empty_root() {
        let mut resolver = ();
        let mut original = Trie::new(1).unwrap();
        original.insert(&[0], value(1), &mut resolver).unwrap();
        let root = original.into_root().unwrap();

        let mut overflowing = Trie::from_trusted_root(1, usize::MAX, Some(root.clone())).unwrap();
        let before = overflowing.root_id();
        assert!(matches!(
            overflowing.insert(&[0x40], value(2), &mut resolver),
            Err(CellError::MalformedTrie)
        ));
        assert_eq!(overflowing.root_id(), before);
        assert_eq!(overflowing.len(), usize::MAX);

        let mut wrong_count = Trie::from_trusted_root(1, 2, Some(root)).unwrap();
        let before = wrong_count.root_id();
        assert!(matches!(
            wrong_count.remove(&[0], &mut resolver),
            Err(CellError::MalformedTrie)
        ));
        assert_eq!(wrong_count.root_id(), before);
        assert_eq!(wrong_count.len(), 2);
    }

    struct MapResolver {
        cells: HashMap<CellID, Arc<Cell>>,
        calls: Vec<CellID>,
    }

    impl CellResolver for MapResolver {
        fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
            if let CellRef::Resident(cell) = reference {
                return Ok(Arc::clone(cell));
            }
            let id = reference.id();
            self.calls.push(id);
            self.cells
                .get(&id)
                .cloned()
                .ok_or(CellError::MissingCell(id))
        }
    }

    #[test]
    fn resolves_only_the_accessed_path() {
        let mut trie = Trie::new(1).unwrap();
        trie.insert(&[0x00], value(1), &mut ()).unwrap();
        trie.insert(&[0x40], value(2), &mut ()).unwrap();
        let root_ref = trie.into_root().unwrap();
        let (payload, mut refs) = resident(&root_ref).clone().into_parts();
        let missing_id = refs[0].id();
        let missing_body = Arc::new(resident(&refs[0]).clone());
        refs[0] = CellRef::pruned(missing_id);
        let root = CellRef::resident(Cell::new(payload, refs).unwrap());
        let trie = Trie::from_trusted_root(1, 2, Some(root)).unwrap();
        let mut resolver = MapResolver {
            cells: HashMap::from([(missing_id, missing_body)]),
            calls: Vec::new(),
        };

        assert_eq!(
            trie.get(&[0x40], &mut resolver).unwrap().unwrap().payload(),
            &[2]
        );
        assert!(resolver.calls.is_empty());
        assert_eq!(
            trie.get(&[0x00], &mut resolver).unwrap().unwrap().payload(),
            &[1]
        );
        assert_eq!(resolver.calls, vec![missing_id]);
    }
}
