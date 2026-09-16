//! Radix-4 Patricia trie over fixed-width byte-string keys.

use crate::{Cell, CellError, CellID, CellRef, CellResolver, resolve_cell};
use std::{convert::TryFrom, sync::Arc};

const LEAF_MASK: u8 = 0;
const NODE_HEADER: usize = 3; // child mask + u16 label length

/// Largest fixed key supported by the recursive mutation implementation.
///
/// ponytail: keep recursion bounded to the current Scalar use; switch mutation
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
    root: Option<CellRef>,
}

impl Trie {
    /// Constructs an empty trie for keys of exactly `key_bytes` bytes.
    pub fn new(key_bytes: usize) -> Result<Self, CellError> {
        check_key_width(key_bytes)?;
        Ok(Self {
            key_bytes,
            root: None,
        })
    }

    /// Wraps a resident or pruned root without loading or traversing it.
    /// Accepts `CellRef`, `Cell`, or `Arc<Cell>`. Every node, including the root,
    /// is resolved and validated on access, regardless of residency.
    /// Key width belongs to the owning schema; no entry count is needed.
    /// Use `new` for an empty Trie.
    pub fn from_cell(root: impl Into<CellRef>, key_bytes: usize) -> Result<Self, CellError> {
        check_key_width(key_bytes)?;
        Ok(Self {
            key_bytes,
            root: Some(root.into()),
        })
    }

    /// Configured key width in bytes.
    pub fn key_bytes(&self) -> usize {
        self.key_bytes
    }

    /// Returns true when the trie has no root.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Borrows the raw root reference.
    pub fn root(&self) -> Option<&CellRef> {
        self.root.as_ref()
    }

    /// Returns the root Cell commitment, if non-empty.
    ///
    /// The enclosing schema fixes the key width and owns any entry-count field;
    /// neither is part of the raw root Cell.
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
        match self.root.as_ref() {
            Some(root) => Self::lookup_ref(root, key, resolver),
            None => Ok(None),
        }
    }

    /// Looks up one fixed-width key directly from a raw Trie root. No entry
    /// count or envelope is needed. The caller's schema fixes the key width;
    /// all visited node shapes and Cell hashes are checked during traversal.
    pub fn lookup<R: CellResolver + ?Sized>(
        root: &CellRef,
        key: &[u8],
        resolver: &mut R,
    ) -> Result<Option<Arc<Cell>>, CellError> {
        match Self::lookup_ref(root, key, resolver)? {
            Some(reference) => resolve_cell(resolver, &reference).map(Some),
            None => Ok(None),
        }
    }

    fn lookup_ref<R: CellResolver + ?Sized>(
        root: &CellRef,
        key: &[u8],
        resolver: &mut R,
    ) -> Result<Option<CellRef>, CellError> {
        check_key_width(key.len())?;
        let mut current = resolve_cell(resolver, root)?;
        let total_digits = key.len() * 4;
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
        self.insert_ref(key, CellRef::resident(value), resolver)
    }

    /// Inserts a data reference without requiring its body to be resident.
    /// As with [`Self::insert`], traversal errors leave this trie unchanged.
    pub fn insert_ref<R: CellResolver + ?Sized>(
        &mut self,
        key: &[u8],
        value: CellRef,
        resolver: &mut R,
    ) -> Result<Option<CellRef>, CellError> {
        self.check_key(key)?;
        let (root, prior) = match self.root.as_ref() {
            Some(root) => {
                let root = resolve_cell(resolver, root)?;
                insert_cell(&root, 0, self.key_bytes * 4, key, value, resolver)?
            }
            None => (leaf_cell(key_digits(key, 0), value)?, None),
        };
        self.root = Some(CellRef::resident(root));
        Ok(prior)
    }

    /// Finds the lexicographically smallest key, resolving only its path.
    pub fn first_key<R: CellResolver + ?Sized>(
        &self,
        resolver: &mut R,
    ) -> Result<Option<Vec<u8>>, CellError> {
        self.seek_key(None, false, resolver)
    }

    /// Finds the lexicographically largest key, resolving only its path.
    pub fn last_key<R: CellResolver + ?Sized>(
        &self,
        resolver: &mut R,
    ) -> Result<Option<Vec<u8>>, CellError> {
        self.seek_key(None, true, resolver)
    }

    /// Finds the smallest key strictly greater than `key`.
    /// Subtrees preceding the requested key are never resolved.
    pub fn next_key_after<R: CellResolver + ?Sized>(
        &self,
        key: &[u8],
        resolver: &mut R,
    ) -> Result<Option<Vec<u8>>, CellError> {
        self.check_key(key)?;
        self.seek_key(Some(key), false, resolver)
    }

    /// Visits all leaf references in ascending key order. Data bodies remain
    /// unresolved. Use a metered resolver to bound traversal of an untrusted DAG.
    pub fn entries<R: CellResolver + ?Sized>(
        &self,
        resolver: &mut R,
    ) -> Result<Vec<(Vec<u8>, CellRef)>, CellError> {
        self.collect_entries(usize::MAX, resolver)
    }

    /// Checks a count owned by an enclosing schema, stopping at the first extra
    /// entry. The count never sizes an allocation and is not needed for lookup
    /// or mutation of the Trie itself.
    pub fn entries_exact<R: CellResolver + ?Sized>(
        &self,
        expected_len: usize,
        resolver: &mut R,
    ) -> Result<Vec<(Vec<u8>, CellRef)>, CellError> {
        let entries = self.collect_entries(expected_len, resolver)?;
        if entries.len() != expected_len {
            return Err(CellError::MalformedTrie);
        }
        Ok(entries)
    }

    fn collect_entries<R: CellResolver + ?Sized>(
        &self,
        limit: usize,
        resolver: &mut R,
    ) -> Result<Vec<(Vec<u8>, CellRef)>, CellError> {
        let mut entries = Vec::new();
        if let Some(root) = &self.root {
            collect_entries(
                root,
                Vec::new(),
                self.key_bytes * 4,
                limit,
                &mut entries,
                resolver,
            )?;
        }
        Ok(entries)
    }

    fn seek_key<R: CellResolver + ?Sized>(
        &self,
        after: Option<&[u8]>,
        reverse: bool,
        resolver: &mut R,
    ) -> Result<Option<Vec<u8>>, CellError> {
        match &self.root {
            None => Ok(None),
            Some(root) => seek_cell(
                root,
                Vec::new(),
                self.key_bytes * 4,
                after,
                reverse,
                resolver,
            ),
        }
    }

    /// Removes a key and returns its data reference, if present.
    ///
    /// Collapsing a now-unary branch resolves the surviving trie node. On any
    /// error the original root remains unchanged.
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
            self.root = new_root.map(CellRef::resident);
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

fn seek_cell<R: CellResolver + ?Sized>(
    reference: &CellRef,
    mut prefix: Vec<u8>,
    total_digits: usize,
    mut after: Option<&[u8]>,
    reverse: bool,
    resolver: &mut R,
) -> Result<Option<Vec<u8>>, CellError> {
    let cell = resolve_cell(resolver, reference)?;
    let (label, mask) = parse_cell(&cell, total_digits - prefix.len())?;
    prefix.extend(label);
    if let Some(key) = after {
        match prefix
            .iter()
            .copied()
            .cmp((0..prefix.len()).map(|i| key_digit(key, i)))
        {
            std::cmp::Ordering::Less => return Ok(None),
            std::cmp::Ordering::Greater => after = None,
            std::cmp::Ordering::Equal => {}
        }
    }
    if mask == LEAF_MASK {
        return Ok(after.is_none().then(|| pack_digits(&prefix)));
    }
    for i in 0..4 {
        let selector = if reverse { 3 - i } else { i };
        if mask & (1 << selector) == 0
            || after.is_some_and(|key| selector < key_digit(key, prefix.len()))
        {
            continue;
        }
        let mut child_prefix = prefix.clone();
        child_prefix.push(selector);
        if let Some(key) = seek_cell(
            &cell.refs()[child_index(mask, selector)],
            child_prefix,
            total_digits,
            after,
            reverse,
            resolver,
        )? {
            return Ok(Some(key));
        }
    }
    Ok(None)
}

fn collect_entries<R: CellResolver + ?Sized>(
    reference: &CellRef,
    mut prefix: Vec<u8>,
    total_digits: usize,
    expected_len: usize,
    entries: &mut Vec<(Vec<u8>, CellRef)>,
    resolver: &mut R,
) -> Result<(), CellError> {
    let cell = resolve_cell(resolver, reference)?;
    let (label, mask) = parse_cell(&cell, total_digits - prefix.len())?;
    prefix.extend(label);
    if mask == LEAF_MASK {
        if entries.len() >= expected_len {
            return Err(CellError::MalformedTrie);
        }
        entries.push((pack_digits(&prefix), cell.refs()[0].clone()));
    } else {
        for selector in 0..4 {
            if mask & (1 << selector) != 0 {
                let mut child_prefix = prefix.clone();
                child_prefix.push(selector);
                collect_entries(
                    &cell.refs()[child_index(mask, selector)],
                    child_prefix,
                    total_digits,
                    expected_len,
                    entries,
                    resolver,
                )?;
            }
        }
    }
    Ok(())
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
    fn raw_root_lookup_needs_no_count_and_still_checks_nodes() {
        let mut trie = Trie::new(1).unwrap();
        for key in [0, 7, 255] {
            trie.insert(&[key], value(key), &mut ()).unwrap();
        }
        let root = trie.into_root().unwrap();
        let mut bag = crate::BagOfCells::collect(root.as_resident_arc().unwrap().clone()).unwrap();
        let root = root.to_pruned();
        for key in [0, 7, 255] {
            assert_eq!(
                Trie::lookup(&root, &[key], &mut bag)
                    .unwrap()
                    .unwrap()
                    .payload(),
                &[key]
            );
        }
        assert!(Trie::lookup(&root, &[8], &mut bag).unwrap().is_none());
        assert!(Trie::lookup(&root, &[], &mut bag).is_err());
        assert!(matches!(
            Trie::lookup(&root, &[7], &mut ()),
            Err(CellError::MissingCell(_))
        ));
        let malformed = CellRef::resident(Cell::new(vec![0], vec![]).unwrap());
        assert!(matches!(
            Trie::lookup(&malformed, &[7], &mut ()),
            Err(CellError::MalformedTrie)
        ));
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
        assert_eq!(trie.entries(&mut resolver).unwrap().len(), 4);
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
        assert_eq!(trie.entries(&mut resolver).unwrap().len(), 4);
        assert_eq!(
            trie.get(&[0x40], &mut resolver).unwrap().unwrap().payload(),
            &[9]
        );

        let removed = trie.remove(&[0x01], &mut resolver).unwrap().unwrap();
        assert_eq!(payload(removed), 2);
        assert_eq!(trie.entries(&mut resolver).unwrap().len(), 3);
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
    fn ordered_navigation_and_import_validation() {
        let mut trie = Trie::new(2).unwrap();
        let keys = [[0, 0], [0, 1], [0, 255], [1, 0], [127, 128], [255, 255]];
        for (i, key) in keys.iter().enumerate().rev() {
            trie.insert(key, value(i as u8), &mut ()).unwrap();
        }
        assert_eq!(trie.first_key(&mut ()).unwrap(), Some(keys[0].to_vec()));
        assert_eq!(trie.last_key(&mut ()).unwrap(), Some(keys[5].to_vec()));
        for candidate in 0..=u16::MAX {
            let key = candidate.to_be_bytes();
            let expected = keys
                .iter()
                .find(|stored| **stored > key)
                .map(|stored| stored.to_vec());
            assert_eq!(trie.next_key_after(&key, &mut ()).unwrap(), expected);
        }
        let entries = trie.entries(&mut ()).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.0.clone())
                .collect::<Vec<_>>(),
            keys
        );
        assert_eq!(trie.entries_exact(6, &mut ()).unwrap().len(), 6);
        for count in [0, 5, 7, usize::MAX] {
            assert!(matches!(
                trie.entries_exact(count, &mut ()),
                Err(CellError::MalformedTrie)
            ));
        }
        let empty = Trie::new(2).unwrap();
        assert!(empty.entries_exact(0, &mut ()).unwrap().is_empty());
        assert!(matches!(
            empty.entries_exact(1, &mut ()),
            Err(CellError::MalformedTrie)
        ));
    }

    #[test]
    fn ordered_navigation_does_not_load_preceding_siblings_or_value_bodies() {
        let mut trie = Trie::new(1).unwrap();
        trie.insert_ref(&[0], CellRef::pruned([7; 32]), &mut ())
            .unwrap();
        trie.insert_ref(&[255], CellRef::pruned([8; 32]), &mut ())
            .unwrap();
        let (payload, mut refs) = trie
            .root()
            .unwrap()
            .as_resident()
            .unwrap()
            .clone()
            .into_parts();
        refs[0] = refs[0].to_pruned();
        let partial = Trie::from_cell(Cell::new(payload, refs).unwrap(), 1).unwrap();
        assert!(matches!(
            partial.first_key(&mut ()),
            Err(CellError::MissingCell(_))
        ));
        assert_eq!(partial.last_key(&mut ()).unwrap(), Some(vec![255]));
        assert_eq!(
            partial.next_key_after(&[128], &mut ()).unwrap(),
            Some(vec![255])
        );
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

        let trie = Trie::from_cell(root, 1).unwrap();
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
    fn resident_and_pruned_roots_reject_noncanonical_nodes_on_access() {
        let child = CellRef::resident(value(1));
        let root = Cell::new(vec![0b0001, 0, 0], vec![child]).unwrap();
        let mut bag = crate::BagOfCells::collect(Arc::new(root.clone())).unwrap();
        for reference in [CellRef::pruned(root.id()), root.into()] {
            let id = reference.id();
            let mut trie = Trie::from_cell(reference, 1).unwrap();
            assert!(matches!(
                trie.get(&[0], &mut bag),
                Err(CellError::MalformedTrie)
            ));
            assert!(matches!(
                trie.remove(&[0], &mut bag),
                Err(CellError::MalformedTrie)
            ));
            assert_eq!(trie.root_id(), Some(id));
        }
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
        let root_ref = trie.into_root().unwrap();
        let root_id = root_ref.id();
        let (payload, mut refs) = resident(&root_ref).clone().into_parts();
        refs[0] = CellRef::pruned(refs[0].id());
        let root = Cell::new(payload, refs).unwrap();
        assert_eq!(root.id(), root_id);

        let mut trie = Trie::from_cell(root, 1).unwrap();
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

        let mut trie = Trie::from_cell(root, 1).unwrap();
        let before = trie.root_id();
        assert!(matches!(
            trie.remove(&[0x00], &mut resolver),
            Err(CellError::MissingCell(_))
        ));
        assert_eq!(trie.root_id(), before);
        assert!(!trie.is_empty());
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
    fn root_constructor_preserves_lazy_access_and_validates_key_width() {
        let mut original = Trie::new(1).unwrap();
        original.insert(&[0], value(1), &mut ()).unwrap();
        let root = original.into_root().unwrap();
        let cell = root.as_resident_arc().unwrap().clone();
        let wrong_width = Trie::from_cell(cell.clone(), 2).unwrap();
        assert!(matches!(
            wrong_width.get(&[0, 0], &mut ()),
            Err(CellError::MalformedTrie)
        ));
        assert!(matches!(
            Trie::from_cell(cell.clone(), MAX_TRIE_KEY_BYTES + 1),
            Err(CellError::TrieKeyTooLong { .. })
        ));
        assert!(matches!(
            Trie::from_cell(root.to_pruned(), MAX_TRIE_KEY_BYTES + 1),
            Err(CellError::TrieKeyTooLong { .. })
        ));
        let mut lazy = Trie::from_cell(root.to_pruned(), 1).unwrap();
        assert_eq!(lazy.root_id(), Some(root.id()));
        assert!(matches!(
            lazy.remove(&[0], &mut ()),
            Err(CellError::MissingCell(id)) if id == root.id()
        ));
        assert_eq!(lazy.root_id(), Some(root.id()));
        let mut bag = crate::BagOfCells::collect(cell).unwrap();
        let removed = lazy.remove(&[0], &mut bag).unwrap().unwrap();
        assert_eq!(resolve_cell(&mut bag, &removed).unwrap().payload(), &[1]);
        assert!(lazy.is_empty());

        // Zero-byte keys are valid: a singleton has an empty compressed label.
        let mut singleton = Trie::new(0).unwrap();
        singleton.insert(&[], value(7), &mut ()).unwrap();
        let singleton = Trie::from_cell(resident(singleton.root().unwrap()).clone(), 0).unwrap();
        assert_eq!(
            singleton.get(&[], &mut ()).unwrap().unwrap().payload(),
            &[7]
        );
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
        let root = Cell::new(payload, refs).unwrap();
        let trie = Trie::from_cell(root, 1).unwrap();
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
