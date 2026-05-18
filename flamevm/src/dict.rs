use core::cmp::Ordering;

use crate::Integer;
use crate::Value;

/// Errors from `Dict` operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictError {
    /// Attempted to insert a key that already exists.
    DuplicateKey,
    /// A bulk constructor received entries that were not strictly ascending by key.
    KeysOutOfOrder,
}

/// Ordered map from `Integer` keys to `Value`s.
///
/// Invariant: keys are strictly ascending by `Integer::cmp`. This makes
/// the wire encoding canonical (one wire form per logical dict) and
/// permits O(log n) lookup via binary search.
pub struct Dict {
    entries: Vec<(Integer, Value)>,
}

impl Dict {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Dict { entries: Vec::new() }
    }

    /// Number of entries.
    pub fn len(&self) -> usize { self.entries.len() }

    /// Returns true if the dictionary has no entries.
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// Returns the entries in ascending key order.
    pub fn entries(&self) -> &[(Integer, Value)] { &self.entries }

    /// Iterates over entries in ascending key order.
    pub fn iter(&self) -> core::slice::Iter<'_, (Integer, Value)> {
        self.entries.iter()
    }

    /// Looks up a value by key. O(log n).
    pub fn get(&self, key: &Integer) -> Option<&Value> {
        self.position(key).ok().map(|i| &self.entries[i].1)
    }

    /// Returns true if the key is present.
    pub fn contains_key(&self, key: &Integer) -> bool {
        self.position(key).is_ok()
    }

    /// Inserts a key-value pair. Returns `DuplicateKey` if the key already exists.
    pub fn insert(&mut self, key: Integer, value: Value) -> Result<(), DictError> {
        match self.position(&key) {
            Ok(_) => Err(DictError::DuplicateKey),
            Err(i) => {
                self.entries.insert(i, (key, value));
                Ok(())
            }
        }
    }

    /// Inserts or replaces a key-value pair. Returns the prior value if the key existed.
    pub fn upsert(&mut self, key: Integer, value: Value) -> Option<Value> {
        match self.position(&key) {
            Ok(i) => Some(core::mem::replace(&mut self.entries[i].1, value)),
            Err(i) => {
                self.entries.insert(i, (key, value));
                None
            }
        }
    }

    /// Removes a key and returns its value if present.
    pub fn remove(&mut self, key: &Integer) -> Option<Value> {
        match self.position(key) {
            Ok(i) => Some(self.entries.remove(i).1),
            Err(_) => None,
        }
    }

    /// Builds a dict from a list of values with implicit keys 0, 1, 2, ...
    /// The resulting dict has sequential keys, which is the canonical input
    /// for list-style wire encoding.
    pub fn from_values(values: Vec<Value>) -> Self {
        let entries = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Integer::from(i as u64), v))
            .collect();
        Dict { entries }
    }

    /// Builds a dict from entries that must already be strictly ascending by key.
    pub fn from_sorted_entries(entries: Vec<(Integer, Value)>) -> Result<Self, DictError> {
        for w in entries.windows(2) {
            match w[0].0.cmp(&w[1].0) {
                Ordering::Less => continue,
                Ordering::Equal => return Err(DictError::DuplicateKey),
                Ordering::Greater => return Err(DictError::KeysOutOfOrder),
            }
        }
        Ok(Dict { entries })
    }

    fn position(&self, key: &Integer) -> Result<usize, usize> {
        self.entries.binary_search_by(|(k, _)| k.cmp(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::String;

    fn v(n: u64) -> Value { Value::Int(Integer::from(n)) }

    #[test]
    fn empty_dict() {
        let d = Dict::new();
        assert_eq!(d.len(), 0);
        assert!(d.is_empty());
        assert!(d.get(&Integer::from(0u64)).is_none());
    }

    #[test]
    fn insert_basic() {
        let mut d = Dict::new();
        d.insert(Integer::from(5u64), v(50)).unwrap();
        d.insert(Integer::from(1u64), v(10)).unwrap();
        d.insert(Integer::from(3u64), v(30)).unwrap();
        // Entries must come back in ascending key order regardless of insertion order.
        let keys: Vec<_> = d.entries().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![Integer::from(1u64), Integer::from(3u64), Integer::from(5u64)]);
    }

    #[test]
    fn insert_duplicate_rejected() {
        let mut d = Dict::new();
        d.insert(Integer::from(7u64), v(1)).unwrap();
        assert_eq!(d.insert(Integer::from(7u64), v(2)), Err(DictError::DuplicateKey));
        // Original value preserved.
        match d.get(&Integer::from(7u64)) {
            Some(Value::Int(i)) => assert_eq!(*i, Integer::from(1u64)),
            _ => panic!("expected Int"),
        }
    }

    #[test]
    fn upsert_replaces_and_returns_old() {
        let mut d = Dict::new();
        assert!(d.upsert(Integer::from(2u64), v(100)).is_none());
        let prior = d.upsert(Integer::from(2u64), v(200));
        match prior {
            Some(Value::Int(i)) => assert_eq!(i, Integer::from(100u64)),
            _ => panic!("expected old Int"),
        }
        match d.get(&Integer::from(2u64)) {
            Some(Value::Int(i)) => assert_eq!(*i, Integer::from(200u64)),
            _ => panic!("expected Int"),
        }
    }

    #[test]
    fn get_and_contains_key() {
        let mut d = Dict::new();
        d.insert(Integer::from(1u64), v(10)).unwrap();
        d.insert(Integer::from(99u64), Value::String(String::from(b"hi".to_vec()))).unwrap();
        assert!(d.contains_key(&Integer::from(1u64)));
        assert!(d.contains_key(&Integer::from(99u64)));
        assert!(!d.contains_key(&Integer::from(50u64)));
        match d.get(&Integer::from(99u64)) {
            Some(Value::String(s)) => assert_eq!(s.as_bytes(), b"hi"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn remove_present_and_absent() {
        let mut d = Dict::new();
        d.insert(Integer::from(1u64), v(10)).unwrap();
        d.insert(Integer::from(2u64), v(20)).unwrap();
        let removed = d.remove(&Integer::from(1u64));
        assert!(matches!(removed, Some(Value::Int(_))));
        assert_eq!(d.len(), 1);
        assert!(d.remove(&Integer::from(1u64)).is_none());
        assert!(d.remove(&Integer::from(42u64)).is_none());
    }

    #[test]
    fn signed_keys_ordered_negatives_first() {
        let mut d = Dict::new();
        d.insert(Integer::from(2i64), v(2)).unwrap();
        d.insert(Integer::from(-3i64), v(30)).unwrap();
        d.insert(Integer::from(0i64), v(0)).unwrap();
        d.insert(Integer::from(-1i64), v(10)).unwrap();
        let keys: Vec<_> = d.entries().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![
                Integer::from(-3i64),
                Integer::from(-1i64),
                Integer::from(0i64),
                Integer::from(2i64),
            ]
        );
    }

    #[test]
    fn from_values_produces_sequential_keys() {
        let d = Dict::from_values(vec![v(10), v(20), v(30)]);
        assert_eq!(d.len(), 3);
        let keys: Vec<_> = d.entries().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![Integer::from(0u64), Integer::from(1u64), Integer::from(2u64)]);
    }

    #[test]
    fn from_sorted_entries_accepts_sorted() {
        let d = Dict::from_sorted_entries(vec![
            (Integer::from(-1i64), v(1)),
            (Integer::from(0i64), v(2)),
            (Integer::from(5i64), v(3)),
        ]).unwrap();
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn from_sorted_entries_rejects_unsorted() {
        let err = Dict::from_sorted_entries(vec![
            (Integer::from(5u64), v(1)),
            (Integer::from(2u64), v(2)),
        ]);
        assert_eq!(err.err(), Some(DictError::KeysOutOfOrder));
    }

    #[test]
    fn from_sorted_entries_rejects_duplicates() {
        let err = Dict::from_sorted_entries(vec![
            (Integer::from(3u64), v(1)),
            (Integer::from(3u64), v(2)),
        ]);
        assert_eq!(err.err(), Some(DictError::DuplicateKey));
    }
}
