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

    /// Stores a byte string with a four-byte little-endian length prefix.
    ///
    /// The prefix must fit here. Bytes fill this Cell before spilling into
    /// dedicated continuation Cells, using the next reference slot. Each
    /// nonterminal continuation is full and has one ref; the last has none.
    /// The builder stays on this Cell, so later refs can still be stored here.
    /// An empty string or exact fit uses no continuation ref.
    ///
    /// Fails without mutation if the length exceeds u32 or the required
    /// payload/reference capacity is unavailable. No whole-string buffer is used.
    pub fn store_snake(&mut self, value: &[u8]) -> Result<&mut Self, CellError> {
        let length = u32::try_from(value.len()).map_err(|_| CellError::PayloadCapacity)?;
        let available = self
            .remaining_bytes()
            .checked_sub(4)
            .ok_or(CellError::PayloadCapacity)?;
        let inline = value.len().min(available);
        if inline < value.len() && self.remaining_refs() == 0 {
            return Err(CellError::ReferenceCapacity);
        }

        // Build only the overflow, tail first, directly into its final Cells.
        let mut next = None;
        for chunk in value[inline..].chunks(MAX_CELL_PAYLOAD).rev() {
            next = Some(CellRef::resident(
                Cell::new(chunk.to_vec(), next.into_iter().collect())
                    .expect("snake segments fit Cell limits"),
            ));
        }

        self.payload.extend_from_slice(&length.to_le_bytes());
        self.payload.extend_from_slice(&value[..inline]);
        self.refs.extend(next);
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
