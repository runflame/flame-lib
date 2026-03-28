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

    

}