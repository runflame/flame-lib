use crate::{Cell, CellEncode, CellError, CellRef, MAX_CELL_PAYLOAD, MAX_CELL_REFS};

/// Incrementally builds one bounded [`Cell`].
#[derive(Debug, Default)]
pub struct CellBuilder {
    payload: Vec<u8>,
    refs: Vec<CellRef>,
}

impl CellBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn used_bytes(&self) -> usize {
        self.payload.len()
    }

    pub fn remaining_bytes(&self) -> usize {
        MAX_CELL_PAYLOAD - self.used_bytes()
    }

    pub fn used_refs(&self) -> usize {
        self.refs.len()
    }

    pub fn remaining_refs(&self) -> usize {
        MAX_CELL_REFS - self.used_refs()
    }

    pub fn store_u8(&mut self, value: u8) -> Result<&mut Self, CellError> {
        self.store_bytes(&value.to_le_bytes())
    }

    pub fn store_u16(&mut self, value: u16) -> Result<&mut Self, CellError> {
        self.store_bytes(&value.to_le_bytes())
    }

    pub fn store_u32(&mut self, value: u32) -> Result<&mut Self, CellError> {
        self.store_bytes(&value.to_le_bytes())
    }

    pub fn store_u64(&mut self, value: u64) -> Result<&mut Self, CellError> {
        self.store_bytes(&value.to_le_bytes())
    }

    pub fn store_bytes(&mut self, value: &[u8]) -> Result<&mut Self, CellError> {
        if value.len() > self.remaining_bytes() {
            return Err(CellError::PayloadCapacity);
        }
        self.payload.extend_from_slice(value);
        Ok(self)
    }

    pub fn store_ref(&mut self, value: CellRef) -> Result<&mut Self, CellError> {
        if self.refs.len() == MAX_CELL_REFS {
            return Err(CellError::ReferenceCapacity);
        }
        self.refs.push(value);
        Ok(self)
    }

    /// Encodes `value` atomically: a failed compound write changes nothing.
    pub fn store<T: CellEncode + ?Sized>(&mut self, value: &T) -> Result<&mut Self, CellError> {
        let bytes = self.payload.len();
        let refs = self.refs.len();
        if let Err(error) = value.encode(self) {
            self.payload.truncate(bytes);
            self.refs.truncate(refs);
            return Err(error);
        }
        Ok(self)
    }

    pub fn build(self) -> Cell {
        Cell::new(self.payload, self.refs).expect("CellBuilder enforces Cell limits")
    }
}
