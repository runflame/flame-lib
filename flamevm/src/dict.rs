use std::collections::BTreeMap;

use crate::Int253;
use crate::Value;

/// Ordered map from `Int253` keys to `Value`s.
///
/// Backed by a `BTreeMap`, so `insert` / `get` / `remove` are O(log n)
/// per call. Iteration yields entries in ascending key order, which the
/// wire encoder relies on for canonical output.
pub struct Dict {
    entries: BTreeMap<Int253, Value>,
}

impl Dict {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Dict { entries: BTreeMap::new() }
    }

    /// Number of entries. O(1).
    pub fn len(&self) -> usize { self.entries.len() }

    /// Returns true if the dictionary has no entries. O(1).
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// Iterates over entries in ascending key order.
    pub fn entries(&self) -> impl Iterator<Item = (&Int253, &Value)> {
        self.entries.iter()
    }

    /// Looks up a value by key. O(log n).
    pub fn get(&self, key: &Int253) -> Option<&Value> {
        self.entries.get(key)
    }

    /// Inserts a key-value pair. O(log n). If the key already exists,
    /// replaces the value and returns the prior one.
    pub fn insert(&mut self, key: Int253, value: Value) -> Option<Value> {
        self.entries.insert(key, value)
    }

    /// Removes a key and returns its value if present. O(log n).
    pub fn remove(&mut self, key: &Int253) -> Option<Value> {
        self.entries.remove(key)
    }

    /// Builds a dict from a list of values with implicit keys 0, 1, 2, ...
    /// The resulting dict has sequential keys, which is the canonical input
    /// for list-style wire encoding.
    pub fn from_values(values: Vec<Value>) -> Self {
        let entries = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Int253::from(i as u64), v))
            .collect();
        Dict { entries }
    }

    /// Builds a dict from entries that the caller guarantees are strictly
    /// ascending by key. Used by the wire decoder, which performs the
    /// ordering check inline as it reads.
    pub(crate) fn from_entries_unchecked(entries: Vec<(Int253, Value)>) -> Self {
        Dict { entries: entries.into_iter().collect() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::String;

    fn v(n: u64) -> Value { Value::Int253(Int253::from(n)) }

    #[test]
    fn empty_dict() {
        let d = Dict::new();
        assert_eq!(d.len(), 0);
        assert!(d.is_empty());
        assert!(d.get(&Int253::from(0u64)).is_none());
    }

    #[test]
    fn insert_basic() {
        let mut d = Dict::new();
        assert!(d.insert(Int253::from(5u64), v(50)).is_none());
        assert!(d.insert(Int253::from(1u64), v(10)).is_none());
        assert!(d.insert(Int253::from(3u64), v(30)).is_none());
        let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![Int253::from(1u64), Int253::from(3u64), Int253::from(5u64)]);
    }

    #[test]
    fn insert_existing_replaces_and_returns_old() {
        let mut d = Dict::new();
        assert!(d.insert(Int253::from(2u64), v(100)).is_none());
        let prior = d.insert(Int253::from(2u64), v(200));
        match prior {
            Some(Value::Int253(i)) => assert_eq!(i, Int253::from(100u64)),
            _ => panic!("expected old Int253"),
        }
        match d.get(&Int253::from(2u64)) {
            Some(Value::Int253(i)) => assert_eq!(*i, Int253::from(200u64)),
            _ => panic!("expected Int253"),
        }
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn get_present_and_absent() {
        let mut d = Dict::new();
        d.insert(Int253::from(1u64), v(10));
        d.insert(Int253::from(99u64), Value::String(String::from(b"hi".to_vec())));
        assert!(d.get(&Int253::from(50u64)).is_none());
        match d.get(&Int253::from(99u64)) {
            Some(Value::String(s)) => assert_eq!(s.as_bytes(), b"hi"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn remove_present_and_absent() {
        let mut d = Dict::new();
        d.insert(Int253::from(1u64), v(10));
        d.insert(Int253::from(2u64), v(20));
        let removed = d.remove(&Int253::from(1u64));
        assert!(matches!(removed, Some(Value::Int253(_))));
        assert_eq!(d.len(), 1);
        assert!(d.remove(&Int253::from(1u64)).is_none());
        assert!(d.remove(&Int253::from(42u64)).is_none());
    }

    #[test]
    fn signed_keys_ordered_negatives_first() {
        let mut d = Dict::new();
        d.insert(Int253::from(2i64), v(2));
        d.insert(Int253::from(-3i64), v(30));
        d.insert(Int253::from(0i64), v(0));
        d.insert(Int253::from(-1i64), v(10));
        let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![
                Int253::from(-3i64),
                Int253::from(-1i64),
                Int253::from(0i64),
                Int253::from(2i64),
            ]
        );
    }

    #[test]
    fn from_values_produces_sequential_keys() {
        let d = Dict::from_values(vec![v(10), v(20), v(30)]);
        assert_eq!(d.len(), 3);
        let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![Int253::from(0u64), Int253::from(1u64), Int253::from(2u64)]);
    }
}
