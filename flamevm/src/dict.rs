use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

use crate::errors::VMError;
use crate::Int253;
use crate::Value;

/// Ordered map from `Int253` keys to `Value`s.
///
/// Backed by a `BTreeMap`, so `insert` / `get` / `remove` are O(log n)
/// per call. Iteration yields entries in ascending key order, which the
/// wire encoder relies on for canonical output.
///
/// Carries three **sticky** flags that summarize member types:
///
/// - `copyable` — true iff every value ever inserted was copyable. Once
///   a non-copyable value enters, the flag stays false even if the
///   value is later removed. This is a safe over-approximation that
///   matches the script's intuition that a poisoned dict stays poisoned.
/// - `portable` — same shape, tracking whether any non-portable value
///   has ever been inserted.
/// - `droppable` — same shape, tracking whether any non-droppable
///   value has ever been inserted. A non-empty dict whose only members
///   are `Variable` / `Constraint` / zero-qty `ClearToken` etc. is
///   still droppable even though it isn't copyable.
#[derive(Clone)]
pub struct Dict {
    entries: BTreeMap<Int253, Value>,
    copyable: bool,
    portable: bool,
    droppable: bool,
}

/// Manual `Debug` — `Value` payloads include linear types that don't
/// derive `Debug`. Print just the entry count and the sticky flags;
/// callers that need deeper inspection can iterate `entries()`.
impl core::fmt::Debug for Dict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dict")
            .field("len", &self.entries.len())
            .field("copyable", &self.copyable)
            .field("portable", &self.portable)
            .field("droppable", &self.droppable)
            .finish()
    }
}

impl Dict {
    /// Creates an empty dictionary. An empty dict is copyable,
    /// portable, and droppable (all vacuously).
    pub fn new() -> Self {
        Dict {
            entries: BTreeMap::new(),
            copyable: true,
            portable: true,
            droppable: true,
        }
    }

    /// Number of entries. O(1).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the dictionary has no entries. O(1).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates over entries in ascending key order.
    pub fn entries(&self) -> impl Iterator<Item = (&Int253, &Value)> {
        self.entries.iter()
    }

    /// Looks up a value by key. O(log n).
    pub fn get(&self, key: &Int253) -> Option<&Value> {
        self.entries.get(key)
    }

    /// Inserts a key-value pair. O(log n). If the key already exists,
    /// replaces the value and returns the prior one. Updates the sticky
    /// copyable/portable flags from the new value.
    pub fn insert(&mut self, key: Int253, value: Value) -> Option<Value> {
        self.absorb_flags(&value);
        self.entries.insert(key, value)
    }

    /// Inserts a key-value pair, failing if the key is already occupied.
    /// Returns the rejected value on conflict so the caller can decide
    /// whether to discard or surface it.
    pub fn insert_strict(&mut self, key: Int253, value: Value) -> Result<(), Value> {
        if self.entries.contains_key(&key) {
            return Err(value);
        }
        self.insert(key, value);
        Ok(())
    }

    /// Removes a key and returns its value if present. O(log n).
    /// Does **not** unset the sticky flags — see the struct doc-comment.
    pub fn remove(&mut self, key: &Int253) -> Option<Value> {
        self.entries.remove(key)
    }

    /// Smallest key in the dict, or `None` if empty. O(log n).
    pub fn first_key(&self) -> Option<Int253> {
        self.entries.keys().next().copied()
    }

    /// Largest key in the dict, or `None` if empty. O(log n).
    pub fn last_key(&self) -> Option<Int253> {
        self.entries.keys().next_back().copied()
    }

    /// Smallest key strictly greater than `k`, or `None` if no such key
    /// exists. O(log n).
    pub fn next_key_after(&self, k: &Int253) -> Option<Int253> {
        self.entries
            .range((Excluded(*k), Unbounded))
            .next()
            .map(|(k, _)| *k)
    }

    /// Returns true iff this dict can be safely duplicated (every member
    /// ever inserted was copyable).
    pub fn is_copyable(&self) -> bool {
        self.copyable
    }

    /// Returns true iff this dict can be sealed into long-term storage
    /// (every member ever inserted was portable).
    pub fn is_portable(&self) -> bool {
        self.portable
    }

    /// Returns true iff this dict can be silently discarded by `drop`
    /// (every member ever inserted was droppable). A dict containing
    /// only `Variable` / `Constraint` / pure-computation values is
    /// droppable but not copyable.
    pub fn is_droppable(&self) -> bool {
        self.droppable
    }

    /// VM-level clone — enforces stack-copyability discipline. Errors
    /// with `TypeNotCopyable` if the sticky copyable flag is false
    /// (some non-copyable member entered at some point). Used by the
    /// `dup` family of stack opcodes.
    ///
    /// Distinct from the `Clone` impl, which is the Rust-level
    /// data-duplication path (registry snapshotting / txlog entry
    /// construction): linear-type values (Token, Cell, …) are rejected
    /// here but cloned fine by `.clone()`.
    pub fn try_clone(&self) -> Result<Dict, VMError> {
        if !self.copyable {
            return Err(VMError::TypeNotCopyable);
        }
        let mut new = Dict::new();
        for (k, v) in self.entries() {
            // Every value is copyable by the flag invariant.
            new.entries.insert(*k, v.try_clone()?);
        }
        // Preserve the flags (try_clone of an all-copyable dict produces
        // another all-copyable dict).
        new.copyable = self.copyable;
        new.portable = self.portable;
        new.droppable = self.droppable;
        Ok(new)
    }

    /// Builds a dict from a list of values with implicit keys 0, 1, 2, ...
    /// The resulting dict has sequential keys, which is the canonical input
    /// for list-style wire encoding.
    pub fn from_values(values: Vec<Value>) -> Self {
        let mut d = Dict::new();
        for (i, v) in values.into_iter().enumerate() {
            d.insert(Int253::from(i as u64), v);
        }
        d
    }

    /// Builds a dict from entries that the caller guarantees are strictly
    /// ascending by key. Used by the wire decoder, which performs the
    /// ordering check inline as it reads.
    pub(crate) fn from_entries_unchecked(entries: Vec<(Int253, Value)>) -> Self {
        let mut d = Dict::new();
        for (k, v) in entries {
            d.absorb_flags(&v);
            d.entries.insert(k, v);
        }
        d
    }

    /// Updates the sticky flags based on `v`. Internal use only.
    fn absorb_flags(&mut self, v: &Value) {
        if !v.is_copyable() {
            self.copyable = false;
        }
        if !v.is_portable() {
            self.portable = false;
        }
        if !v.is_droppable() {
            self.droppable = false;
        }
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
