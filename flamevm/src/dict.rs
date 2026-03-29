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
}