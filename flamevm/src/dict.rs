use crate::Integer;
use crate::Value;

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

    /// Returns a reference to the entries.
    pub fn entries(&self) -> &[(Integer, Value)] { &self.entries }

    /// Pushes a key-value pair.
    pub fn push(&mut self, key: Integer, value: Value) {
        self.entries.push((key, value));
    }

    /// Creates a Dict from a list of values with implicit keys 0, 1, 2, ...
    pub fn from_values(values: Vec<Value>) -> Self {
        let entries = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Integer::from(i as u64), v))
            .collect();
        Dict { entries }
    }

    /// Returns true if all keys are sequential integers 0, 1, 2, ..., len-1.
    /// Such a dict can be encoded with the compact list encoding (no keys).
    pub fn has_sequential_keys(&self) -> bool {
        self.entries
            .iter()
            .enumerate()
            .all(|(i, (k, _))| *k == Integer::from(i as u64))
    }
}