use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};

use cells::{
    Cell, CellBuilder, CellDecode, CellEncode, CellError, CellRef, CellResolver, CellSlice, Trie,
};

use crate::errors::VMError;
use crate::Scalar;
use crate::Value;

/// Ordered map from `Scalar` keys to `Value`s.
///
/// The sole ordered index is a [`Trie`], using big-endian scalar paths so its
/// bytewise order agrees with unsigned scalar order. A typed-value cache keeps
/// live witnesses and nonportable values that cannot be serialized. Unaccessed
/// authenticated leaves may stay unloaded; resolver-aware operations load them.
///
/// Dicts are **never VM-copyable** (avoids variable gas for
/// `dup`/`getdup` and the linear-leak hazard). They
/// carry two independent sticky capability flags:
///
/// - `droppable` — true iff every value ever inserted was droppable.
///   Once a non-droppable value enters (a `Token` — portable but
///   linear), the flag stays false even if later removed, so a
///   non-empty token-bearing dict can't be silently `drop`ped. An empty
///   dict is always droppable so a fully drained container can be discarded.
/// - `portable` — true iff every value ever inserted was portable.
///   Once a non-portable value enters, the flag stays false even if
///   that value is later removed.
#[derive(Clone, Debug)]
pub struct Dict {
    // Keep the ordered index indirect so Dict does not enlarge every VM Value.
    trie: Box<Trie>,
    // The index may contain placeholder leaf bodies for these authoritative
    // typed values. The encoder installs their actual Cells before exposing it.
    values: HashMap<Scalar, Value>,
    // Committed by the Dict envelope, not needed by the low-level Trie.
    len: usize,
    droppable: bool,
    portable: bool,
}

impl Dict {
    /// Creates an empty dictionary. A new empty dict is vacuously
    /// droppable and portable.
    pub fn new() -> Self {
        Dict {
            trie: Box::new(Trie::new(32).expect("32-byte scalar keys fit Trie")),
            values: HashMap::new(),
            len: 0,
            droppable: true,
            portable: true,
        }
    }

    /// Number of entries. O(1).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the dictionary has no entries. O(1).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterates over fully hydrated entries in ascending key order.
    /// Lazy dictionaries must first use [`Self::hydrate_all`].
    pub fn entries(&self) -> impl Iterator<Item = (&Scalar, &Value)> {
        assert_eq!(
            self.values.len(),
            self.len(),
            "hydrate a lazy Dict before iterating values"
        );
        let mut values: Vec<_> = self.values.iter().collect();
        values.sort_unstable_by_key(|(key, _)| **key);
        values.into_iter()
    }

    /// Prover metadata only; unlike entries(), this never loads hidden branches.
    pub(crate) fn cached_values(&self) -> impl Iterator<Item = &Value> {
        self.values.values()
    }

    /// Resident path work performed when the encoder installs cached values.
    /// Values themselves are priced separately; untouched unloaded branches need
    /// no work. Every cached key must already have a resident authenticated path.
    pub(crate) fn encoding_path_gas(&self) -> Result<u64, CellError> {
        struct Meter(u64);
        impl CellResolver for Meter {
            fn resolve(&mut self, reference: &CellRef) -> Result<std::sync::Arc<Cell>, CellError> {
                let cell = ().resolve(reference)?;
                // One traversal plus rebuilding/hashing the same path.
                let cost = 2 * (1 + cell.record_size() as u64 + cell.refs().len() as u64);
                self.0 = self.0.checked_add(cost).ok_or(CellError::LimitExceeded)?;
                Ok(cell)
            }
        }
        let mut meter = Meter(0);
        for key in self.values.keys() {
            self.trie
                .get_ref(&key_path(key), &mut meter)?
                .ok_or(CellError::MalformedTrie)?;
        }
        Ok(meter.0)
    }

    /// Looks up a previously hydrated value. Use [`Self::get_resolved`] for
    /// arbitrary dictionaries; unavailable data must never mean an absent key.
    pub fn get(&self, key: &Scalar) -> Option<&Value> {
        if let Some(value) = self.values.get(key) {
            return Some(value);
        }
        assert!(
            self.trie
                .get_ref(&key_path(key), &mut ())
                .expect("use get_resolved for lazy Dicts")
                .is_none(),
            "use get_resolved to hydrate a Dict value"
        );
        None
    }

    /// Inserts a key-value pair. If the key already exists,
    /// replaces the value and returns the prior one. Updates both sticky
    /// capability flags from the new value.
    pub fn insert(&mut self, key: Scalar, value: Value) -> Option<Value> {
        self.insert_resolved(key, value, &mut ())
            .expect("use insert_resolved when Dict paths may be unloaded")
    }

    /// Inserts a key-value pair, failing if the key is already occupied.
    /// Returns the rejected value on conflict so the caller can decide
    /// whether to discard or surface it. A rejected insertion does not
    /// update either sticky capability flag.
    #[allow(clippy::result_large_err)]
    pub fn insert_strict(&mut self, key: Scalar, value: Value) -> Result<(), Value> {
        self.insert_strict_resolved(key, value, &mut ())
            .expect("use insert_strict_resolved when Dict paths may be unloaded")
    }

    /// Removes a key and returns its value if present.
    /// Does **not** unset the sticky flags — see the struct doc-comment.
    pub fn remove(&mut self, key: &Scalar) -> Option<Value> {
        self.remove_resolved(key, &mut ())
            .expect("use remove_resolved when Dict paths may be unloaded")
    }

    /// Smallest key in the dict, or `None` if empty.
    pub fn first_key(&self) -> Option<Scalar> {
        self.first_key_resolved(&mut ())
            .expect("use first_key_resolved for unloaded Dicts")
    }

    /// Largest key in the dict, or `None` if empty.
    pub fn last_key(&self) -> Option<Scalar> {
        self.last_key_resolved(&mut ())
            .expect("use last_key_resolved for unloaded Dicts")
    }

    /// Smallest key strictly greater than `k`, or `None` if no such key
    /// exists.
    pub fn next_key_after(&self, k: &Scalar) -> Option<Scalar> {
        self.next_key_after_resolved(k, &mut ())
            .expect("use next_key_after_resolved for unloaded Dicts")
    }

    /// Loads and validates one value without copying its VM ownership.
    pub fn get_resolved<R: CellResolver + ?Sized>(
        &mut self,
        key: &Scalar,
        resolver: &mut R,
    ) -> Result<Option<&Value>, CellError> {
        let cell = match self.trie.get(&key_path(key), resolver)? {
            Some(cell) => cell,
            None => return Ok(None),
        };
        // Unsupported runtime values have no public representation. A placeholder
        // is usable only alongside the authoritative in-memory typed value.
        if cell.payload().is_empty() && cell.refs().is_empty() {
            return self
                .values
                .get(key)
                .map(Some)
                .ok_or(CellError::InvalidFormat);
        }
        // ponytail: replay this bounded decode even on cache hits so logical
        // access/gas never depends on residency; replace with a nonallocating
        // encoding walk only if profiling justifies the second traversal API.
        let value = Value::from_trusted_cell(&cell, resolver)?;
        self.check_claimed_flags(&value)?;
        self.trie
            .insert_ref(&key_path(key), CellRef::resident(cell), resolver)?;
        self.values.entry(*key).or_insert(value);
        Ok(self.values.get(key))
    }

    /// Inserts atomically; a traversal/decoding error returns the supplied value.
    #[allow(clippy::result_large_err)]
    pub fn insert_resolved<R: CellResolver + ?Sized>(
        &mut self,
        key: Scalar,
        value: Value,
        resolver: &mut R,
    ) -> Result<Option<Value>, (CellError, Value)> {
        let cell = match value.to_cell() {
            Ok(cell) => cell,
            Err(CellError::InvalidFormat | CellError::LimitExceeded) => CellBuilder::new().build(),
            Err(error) => return Err((error, value)),
        };
        let result = self
            .get_resolved(&key, resolver)
            .map(|prior| prior.is_some())
            .and_then(|replaces| {
                let len = if replaces {
                    self.len
                } else {
                    self.len.checked_add(1).ok_or(CellError::MalformedTrie)?
                };
                self.trie
                    .insert(&key_path(&key), cell, resolver)
                    .map(|_| len)
            });
        self.len = match result {
            Ok(len) => len,
            Err(error) => return Err((error, value)),
        };
        self.absorb_flags(&value);
        Ok(self.values.insert(key, value))
    }

    /// Strict insertion. An occupied key returns `Ok(Err(value))`; a loading
    /// failure returns `Err((error, value))`. Neither changes sticky flags.
    #[allow(clippy::result_large_err)]
    pub fn insert_strict_resolved<R: CellResolver + ?Sized>(
        &mut self,
        key: Scalar,
        value: Value,
        resolver: &mut R,
    ) -> Result<Result<(), Value>, (CellError, Value)> {
        match self.trie.get_ref(&key_path(&key), resolver) {
            Ok(Some(_)) => Ok(Err(value)),
            Ok(None) => self.insert_resolved(key, value, resolver).map(|_| Ok(())),
            Err(error) => Err((error, value)),
        }
    }

    /// Removes an owned value only after decoding and path rebuilding succeed.
    pub fn remove_resolved<R: CellResolver + ?Sized>(
        &mut self,
        key: &Scalar,
        resolver: &mut R,
    ) -> Result<Option<Value>, CellError> {
        if self.get_resolved(key, resolver)?.is_none() {
            return Ok(None);
        }
        let len = self.len.checked_sub(1).ok_or(CellError::MalformedTrie)?;
        let mut trie = (*self.trie).clone();
        trie.remove(&key_path(key), resolver)?;
        if (len == 0) != trie.is_empty() {
            return Err(CellError::MalformedTrie);
        }
        *self.trie = trie;
        self.len = len;
        Ok(self.values.remove(key))
    }

    pub fn first_key_resolved<R: CellResolver + ?Sized>(
        &self,
        resolver: &mut R,
    ) -> Result<Option<Scalar>, CellError> {
        self.trie
            .first_key(resolver)?
            .map(scalar_from_path)
            .transpose()
    }

    pub fn last_key_resolved<R: CellResolver + ?Sized>(
        &self,
        resolver: &mut R,
    ) -> Result<Option<Scalar>, CellError> {
        self.trie
            .last_key(resolver)?
            .map(scalar_from_path)
            .transpose()
    }

    pub fn next_key_after_resolved<R: CellResolver + ?Sized>(
        &self,
        key: &Scalar,
        resolver: &mut R,
    ) -> Result<Option<Scalar>, CellError> {
        self.trie
            .next_key_after(&key_path(key), resolver)?
            .map(scalar_from_path)
            .transpose()
    }

    /// Validates an entire import, including its leaf count and scalar paths.
    /// Decoded values are installed only after all leaves pass validation.
    pub fn hydrate_all<R: CellResolver + ?Sized>(
        &mut self,
        resolver: &mut R,
    ) -> Result<(), CellError> {
        self.hydrate_at(resolver, 1)
    }

    fn hydrate_at<R: CellResolver + ?Sized>(
        &mut self,
        resolver: &mut R,
        depth: usize,
    ) -> Result<(), CellError> {
        let entries = self.trie.entries_exact(self.len, resolver)?;
        let mut trie = self.trie.clone();
        let mut loaded = Vec::new();
        for (path, reference) in entries {
            let key = scalar_from_path(path)?;
            if !self.values.contains_key(&key) {
                let cell = cells::resolve_cell(resolver, &reference)?;
                let value = crate::encoding::value_from_cell_at(&cell, resolver, depth, false)?;
                self.check_claimed_flags(&value)?;
                trie.insert_ref(&key_path(&key), CellRef::resident(cell), resolver)?;
                loaded.push((key, value));
            }
        }
        self.trie = trie;
        self.values.extend(loaded);
        Ok(())
    }

    /// Reads an envelope already authenticated by an owning state transition.
    /// Unlike ordinary [`CellDecode`], this does not resolve hidden branches.
    pub fn decode_trusted<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        _resolver: &mut R,
    ) -> Result<Self, CellError> {
        slice.try_load(|slice| {
            let len = usize::try_from(slice.load_u64()?).map_err(|_| CellError::LimitExceeded)?;
            let flags = slice.load_u8()?;
            if flags & !3 != 0 {
                return Err(CellError::InvalidFormat);
            }
            let trie = if len == 0 {
                Trie::new(32)?
            } else {
                Trie::from_cell(slice.load_ref()?, 32)?
            };
            Ok(Self {
                trie: Box::new(trie),
                values: HashMap::new(),
                len,
                portable: flags & 1 != 0,
                droppable: flags & 2 != 0,
            })
        })
    }

    /// Opens a previously validated dictionary envelope without hydration.
    pub fn from_trusted_cell<R: CellResolver + ?Sized>(
        cell: &Cell,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        let mut slice = CellSlice::new(cell);
        let dict = Self::decode_trusted(&mut slice, resolver)?;
        slice.finish()?;
        Ok(dict)
    }

    fn check_claimed_flags(&self, value: &Value) -> Result<(), CellError> {
        if (self.portable && !value.is_portable()) || (self.droppable && !value.is_droppable()) {
            return Err(CellError::InvalidFormat);
        }
        Ok(())
    }

    /// Dicts are never VM-copyable (avoids variable `dup`/`getdup` gas
    /// and the linear-leak hazard).
    pub fn is_copyable(&self) -> bool {
        false
    }

    /// Returns true iff this dict can be sealed into long-term storage.
    /// O(1): successful insertion of any non-portable value permanently
    /// clears the cached flag.
    pub fn is_portable(&self) -> bool {
        self.portable
    }

    /// Returns true iff this dict can be silently discarded by `drop`:
    /// it is empty, or every member ever inserted was droppable. A dict
    /// containing only `Variable` / `Constraint` / pure-computation
    /// values is droppable but not copyable.
    pub fn is_droppable(&self) -> bool {
        self.is_empty() || self.droppable
    }

    /// Logical heap work needed for a Rust-level rollback clone. Used only
    /// for gas charging; VM copyability remains governed by `try_clone`.
    pub(crate) fn clone_gas(&self) -> u64 {
        self.values
            .values()
            .fold(1 + self.values.len() as u64, |gas, value| {
                gas.saturating_add(value.clone_gas())
            })
    }

    /// VM-level clone — always fails: dicts are non-copyable.
    /// The `dup`/`getdup` family routes here, so a dict value can never
    /// be duplicated on the stack. The Rust-level `Clone` impl is the
    /// separate data-duplication path (registry snapshot / txlog entry).
    pub fn try_clone(&self) -> Result<Dict, VMError> {
        Err(VMError::TypeNotCopyable)
    }

    /// Builds a dict from a list of values with implicit keys 0, 1, 2, ...
    /// The resulting dict has sequential keys, which is the canonical input
    /// for list-style wire encoding.
    pub fn from_values(values: Vec<Value>) -> Self {
        let mut d = Dict::new();
        for (i, v) in values.into_iter().enumerate() {
            d.insert(Scalar::from(i as u64), v);
        }
        d
    }

    fn absorb_flags(&mut self, v: &Value) {
        self.droppable &= v.is_droppable();
        self.portable &= v.is_portable();
    }
}

impl CellEncode for Dict {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        self.encode_at(builder, 1)
    }
}

impl Dict {
    pub(crate) fn encode_at(
        &self,
        builder: &mut CellBuilder,
        depth: usize,
    ) -> Result<(), CellError> {
        if depth > crate::encoding::MAX_VALUE_DEPTH {
            return Err(CellError::LimitExceeded);
        }
        let mut trie = self.trie.clone();
        for (key, value) in &self.values {
            trie.insert(
                &key_path(key),
                crate::encoding::value_cell_at(value, depth)?,
                &mut (),
            )?;
        }
        builder.store_u64(u64::try_from(self.len()).map_err(|_| CellError::LimitExceeded)?)?;
        builder.store_u8(u8::from(self.portable) | (u8::from(self.droppable) << 1))?;
        if let Some(root) = trie.into_root() {
            builder.store_ref(root)?;
        }
        Ok(())
    }

    pub(crate) fn decode_at<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
        depth: usize,
    ) -> Result<Self, CellError> {
        if depth > crate::encoding::MAX_VALUE_DEPTH {
            return Err(CellError::LimitExceeded);
        }
        slice.try_load(|slice| {
            let mut dict = Self::decode_trusted(slice, resolver)?;
            dict.hydrate_at(resolver, depth)?;
            Ok(dict)
        })
    }
}

impl CellDecode for Dict {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        Self::decode_at(slice, resolver, 1)
    }
}

/// Big-endian path representation preserves unsigned numeric Scalar order.
pub fn key_path(key: &Scalar) -> [u8; 32] {
    let mut bytes = key.to_bytes();
    bytes.reverse();
    bytes
}

fn scalar_from_path(path: Vec<u8>) -> Result<Scalar, CellError> {
    let mut bytes: [u8; 32] = path.try_into().map_err(|_| CellError::InvalidFormat)?;
    bytes.reverse();
    Scalar::from_bytes(bytes).ok_or(CellError::InvalidFormat)
}

impl Default for Dict {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClearToken, Merlin, String};
    use cells::{CellIndex, CellRef};
    use std::sync::Arc;

    #[test]
    fn encoding_path_gas_tracks_cached_paths_without_serializing_values() {
        let small = Dict::from_values(vec![v(1)]);
        let large = Dict::from_values((0..64).map(v).collect());
        assert_eq!(Dict::new().encoding_path_gas().unwrap(), 0);
        assert!(small.encoding_path_gas().unwrap() > 0);
        assert!(large.encoding_path_gas().unwrap() > small.encoding_path_gas().unwrap());

        let mut runtime = Dict::new();
        runtime.insert(Scalar::ZERO, Value::Merlin(Merlin::new(b"runtime")));
        assert!(runtime.to_cell().is_err());
        assert_eq!(
            runtime.encoding_path_gas().unwrap(),
            small.encoding_path_gas().unwrap()
        );

        let cell = small.to_cell().unwrap();
        let lazy = Dict::from_trusted_cell(&cell, &mut ()).unwrap();
        assert_eq!(lazy.encoding_path_gas().unwrap(), 0);
    }

    #[test]
    fn cached_and_unloaded_values_have_identical_logical_access_costs() {
        struct Meter {
            cells: CellIndex,
            gas: u64,
        }
        impl CellResolver for Meter {
            fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
                let cell = self.cells.resolve(reference)?;
                self.gas += 1 + cell.record_size() as u64 + cell.refs().len() as u64;
                Ok(cell)
            }
        }
        let mut original = Dict::new();
        original.insert(
            Scalar::ONE,
            Value::String(String::from(vec![9; String::MAX_LEN])),
        );
        original.insert(Scalar::from(2u64), v(2));
        let root = Arc::new(original.to_cell().unwrap());
        let bag = CellIndex::collect(root.clone()).unwrap();
        let detached = bag.get(&root.id()).unwrap();
        let mut slice = CellSlice::new(&detached);
        let mut unloaded = Dict::decode_trusted(&mut slice, &mut ()).unwrap();
        slice.finish().unwrap();
        let mut costs = Vec::new();
        for dictionary in [&mut original, &mut unloaded] {
            for _ in 0..2 {
                let mut meter = Meter {
                    cells: bag.clone(),
                    gas: 0,
                };
                assert!(
                    matches!(dictionary.get_resolved(&Scalar::ONE, &mut meter).unwrap(), Some(Value::String(value)) if value.len() == String::MAX_LEN)
                );
                costs.push(meter.gas);
            }
        }
        assert!(costs.iter().all(|cost| *cost == costs[0]));
        assert!(costs[0] >= String::MAX_LEN as u64);
    }

    fn v(n: u64) -> Value {
        Value::Scalar(Scalar::from(n))
    }

    #[test]
    fn empty_dict() {
        let d = Dict::new();
        assert_eq!(d.len(), 0);
        assert!(d.is_empty());
        assert!(d.is_portable());
        assert!(d.get(&Scalar::from(0u64)).is_none());
    }

    #[test]
    fn insert_basic() {
        let mut d = Dict::new();
        assert!(d.insert(Scalar::from(5u64), v(50)).is_none());
        assert!(d.insert(Scalar::from(1u64), v(10)).is_none());
        assert!(d.insert(Scalar::from(3u64), v(30)).is_none());
        let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![Scalar::from(1u64), Scalar::from(3u64), Scalar::from(5u64)]
        );
    }

    #[test]
    fn insert_existing_replaces_and_returns_old() {
        let mut d = Dict::new();
        assert!(d.insert(Scalar::from(2u64), v(100)).is_none());
        let prior = d.insert(Scalar::from(2u64), v(200));
        match prior {
            Some(Value::Scalar(i)) => assert_eq!(i, Scalar::from(100u64)),
            _ => panic!("expected old Scalar"),
        }
        match d.get(&Scalar::from(2u64)) {
            Some(Value::Scalar(i)) => assert_eq!(*i, Scalar::from(200u64)),
            _ => panic!("expected Scalar"),
        }
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn get_present_and_absent() {
        let mut d = Dict::new();
        d.insert(Scalar::from(1u64), v(10));
        d.insert(
            Scalar::from(99u64),
            Value::String(String::from(b"hi".to_vec())),
        );
        assert!(d.get(&Scalar::from(50u64)).is_none());
        match d.get(&Scalar::from(99u64)) {
            Some(Value::String(s)) => assert_eq!(s.as_opaque().unwrap(), b"hi"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn remove_present_and_absent() {
        let mut d = Dict::new();
        d.insert(Scalar::from(1u64), v(10));
        d.insert(Scalar::from(2u64), v(20));
        let removed = d.remove(&Scalar::from(1u64));
        assert!(matches!(removed, Some(Value::Scalar(_))));
        assert_eq!(d.len(), 1);
        assert!(d.remove(&Scalar::from(1u64)).is_none());
        assert!(d.remove(&Scalar::from(42u64)).is_none());
    }

    #[test]
    fn scalar_keys_use_canonical_unsigned_order() {
        let mut d = Dict::new();
        d.insert(Scalar::from(2i64), v(2));
        d.insert(Scalar::from(-3i64), v(30));
        d.insert(Scalar::from(0i64), v(0));
        d.insert(Scalar::from(-1i64), v(10));
        let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![
                Scalar::from(0i64),
                Scalar::from(2i64),
                Scalar::from(-3i64),
                Scalar::from(-1i64),
            ]
        );
    }

    #[test]
    fn from_values_produces_sequential_keys() {
        let d = Dict::from_values(vec![v(10), v(20), v(30)]);
        assert_eq!(d.len(), 3);
        assert!(d.is_portable());
        let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![Scalar::from(0u64), Scalar::from(1u64), Scalar::from(2u64)]
        );
    }

    #[test]
    fn portability_is_sticky() {
        let mut d = Dict::new();
        d.insert(Scalar::ZERO, Value::Merlin(Merlin::new(b"test")));
        assert!(!d.is_portable());

        d.insert(Scalar::ZERO, v(1));
        assert!(!d.is_portable());
        d.remove(&Scalar::ZERO);
        assert!(!d.is_portable());
    }

    #[test]
    fn rejected_insert_does_not_clear_portability() {
        let mut d = Dict::new();
        d.insert(Scalar::ZERO, v(1));
        assert!(d
            .insert_strict(Scalar::ZERO, Value::Merlin(Merlin::new(b"test")))
            .is_err());
        assert!(d.is_portable());
    }

    #[test]
    fn constructors_absorb_nested_portability() {
        let child = Dict::from_values(vec![Value::Merlin(Merlin::new(b"child"))]);
        let parent = Dict::from_values(vec![Value::Dict(child)]);
        assert!(!parent.is_portable());

        let mut explicit = Dict::new();
        explicit.insert(Scalar::from(7u64), Value::Merlin(Merlin::new(b"explicit")));
        assert!(!explicit.is_portable());
    }

    fn token(qty: i64) -> Value {
        Value::ClearToken(ClearToken::new(Scalar::from(qty), Scalar::ONE))
    }

    #[test]
    fn cell_round_trip_keeps_sticky_flags_even_when_empty() {
        let mut original = Dict::from_values(vec![token(-1)]);
        original.remove(&Scalar::ZERO);
        assert!(original.is_empty());
        assert!(original.is_droppable());
        let encoded = original.to_cell().unwrap();
        let mut restored = Dict::from_cell(&encoded, &mut ()).unwrap();
        assert!(!restored.is_portable());
        assert!(restored.is_droppable());
        restored.insert(Scalar::ONE, v(7));
        assert!(!restored.is_droppable());
        assert!(!restored.is_portable());
    }

    #[test]
    fn untrusted_import_rejects_forged_flags_counts_and_scalar_keys() {
        let original = Dict::from_values(vec![token(-1)]).to_cell().unwrap();
        let mut payload = original.payload().to_vec();
        payload[8] |= 1;
        let forged = Cell::new(payload, original.refs().to_vec()).unwrap();
        assert!(matches!(
            Dict::from_cell(&forged, &mut ()),
            Err(CellError::InvalidFormat)
        ));

        let mut payload = original.payload().to_vec();
        payload[..8].copy_from_slice(&2u64.to_le_bytes());
        let forged = Cell::new(payload, original.refs().to_vec()).unwrap();
        assert!(matches!(
            Dict::from_cell(&forged, &mut ()),
            Err(CellError::MalformedTrie)
        ));

        let mut trie = Trie::new(32).unwrap();
        trie.insert(&[255; 32], v(1).to_cell().unwrap(), &mut ())
            .unwrap();
        let mut builder = CellBuilder::new();
        builder
            .store_u64(1)
            .unwrap()
            .store_u8(3)
            .unwrap()
            .store_ref(trie.into_root().unwrap())
            .unwrap();
        let forged = builder.build();
        assert!(matches!(
            Dict::from_cell(&forged, &mut ()),
            Err(CellError::InvalidFormat)
        ));
        let lazy = Dict::from_trusted_cell(&forged, &mut ()).unwrap();
        assert!(matches!(
            lazy.first_key_resolved(&mut ()),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn invalid_counts_cannot_overflow_or_lose_a_linear_value() {
        let original = Dict::from_values(vec![token(9)]).to_cell().unwrap();
        for count in [0usize, 2, usize::MAX] {
            let mut dict = Dict::from_trusted_cell(&original, &mut ()).unwrap();
            // Exercise defensive mutation checks even if prior admission was wrong.
            dict.len = count;
            let before = dict.trie.root_id();
            if count == usize::MAX {
                let (error, returned) = dict
                    .insert_resolved(Scalar::ONE, token(7), &mut ())
                    .unwrap_err();
                assert_eq!(error, CellError::MalformedTrie);
                assert!(matches!(returned, Value::ClearToken(t) if t.qty() == Scalar::from(7u64)));
                assert!(dict.get_resolved(&Scalar::ONE, &mut ()).unwrap().is_none());
            }
            assert!(matches!(
                dict.remove_resolved(&Scalar::ZERO, &mut ()),
                Err(CellError::MalformedTrie)
            ));
            assert_eq!(dict.trie.root_id(), before);
            assert_eq!(dict.len(), count);
            assert!(matches!(
                dict.get_resolved(&Scalar::ZERO, &mut ()).unwrap(),
                Some(Value::ClearToken(t)) if t.qty() == Scalar::from(9u64)
            ));
        }
    }

    #[test]
    fn lazy_access_and_mutation_preserve_unloaded_siblings() {
        let mut original = Dict::new();
        for key in [0u64, 255, 256, u64::MAX] {
            original.insert(Scalar::from(key), v(key));
        }
        let full = original.to_cell().unwrap();
        let mut bag = CellIndex::collect(Arc::new(full.clone())).unwrap();
        let unloaded = Cell::decode_record_exact(&full.encode_record()).unwrap();
        let mut lazy = Dict::from_trusted_cell(&unloaded, &mut ()).unwrap();
        assert!(lazy.values.is_empty());
        assert_eq!(lazy.to_cell().unwrap().id(), full.id());
        assert!(matches!(
            lazy.get_resolved(&Scalar::ZERO, &mut ()),
            Err(CellError::MissingCell(_))
        ));
        assert!(
            matches!(lazy.get_resolved(&Scalar::ZERO, &mut bag).unwrap(), Some(Value::Scalar(n)) if *n == Scalar::ZERO)
        );
        assert_eq!(lazy.values.len(), 1);
        assert_eq!(lazy.to_cell().unwrap().id(), full.id());
        lazy.insert_resolved(Scalar::from(256u64), v(77), &mut bag)
            .unwrap();
        original.insert(Scalar::from(256u64), v(77));
        assert_eq!(lazy.values.len(), 2);
        assert_eq!(
            lazy.to_cell().unwrap().id(),
            original.to_cell().unwrap().id()
        );
    }

    #[test]
    fn failed_collapse_keeps_linear_value_and_retry_cannot_duplicate_it() {
        let mut original = Dict::new();
        original.insert(Scalar::ZERO, token(9));
        original.insert(Scalar::from(256u64), v(7));
        let full = original.to_cell().unwrap();
        let mut bag = CellIndex::collect(Arc::new(full.clone())).unwrap();
        let root = full.refs()[0].as_resident().unwrap();
        let mut refs = root.refs().to_vec();
        refs[1] = refs[1].to_unloaded().unwrap();
        let partial_root = Cell::new(root.payload().to_vec(), refs).unwrap();
        let partial = Cell::new(
            full.payload().to_vec(),
            vec![CellRef::resident(partial_root)],
        )
        .unwrap();
        let mut dict = Dict::from_trusted_cell(&partial, &mut ()).unwrap();
        assert!(matches!(
            dict.remove_resolved(&Scalar::ZERO, &mut ()),
            Err(CellError::MissingCell(_))
        ));
        assert_eq!(dict.len(), 2);
        assert!(
            matches!(dict.get(&Scalar::ZERO), Some(Value::ClearToken(t)) if t.qty() == Scalar::from(9u64))
        );
        assert_eq!(dict.to_cell().unwrap().id(), full.id());
        assert!(matches!(
            dict.remove_resolved(&Scalar::ZERO, &mut bag).unwrap(),
            Some(Value::ClearToken(_))
        ));
        assert!(dict
            .remove_resolved(&Scalar::ZERO, &mut bag)
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_insertion_returns_supplied_debt_without_changing_dictionary() {
        let full = Dict::from_values(vec![v(1)]).to_cell().unwrap();
        let unloaded = Cell::decode_record_exact(&full.encode_record()).unwrap();
        let mut dict = Dict::from_trusted_cell(&unloaded, &mut ()).unwrap();
        let (error, returned) = dict
            .insert_resolved(Scalar::ONE, token(-7), &mut ())
            .unwrap_err();
        assert!(matches!(error, CellError::MissingCell(_)));
        assert!(matches!(returned, Value::ClearToken(t) if t.qty() == Scalar::from(-7i64)));
        assert_eq!(dict.len(), 1);
        assert!(dict.is_portable());
        assert_eq!(dict.to_cell().unwrap().id(), full.id());
    }
}
