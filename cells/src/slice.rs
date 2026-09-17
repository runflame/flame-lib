use crate::{Cell, CellError, CellRef, CellResolver, MAX_CELL_PAYLOAD, resolve_cell};

/// A consuming payload/reference cursor over one [`Cell`].
#[derive(Clone, Debug)]
pub struct CellSlice<'a> {
    cell: &'a Cell,
    byte_offset: usize,
    ref_offset: usize,
}

impl<'a> CellSlice<'a> {
    pub fn new(cell: &'a Cell) -> Self {
        Self {
            cell,
            byte_offset: 0,
            ref_offset: 0,
        }
    }

    pub fn remaining_bytes(&self) -> usize {
        self.cell.payload().len() - self.byte_offset
    }

    pub fn remaining_refs(&self) -> usize {
        self.cell.refs().len() - self.ref_offset
    }

    pub fn load_u8(&mut self) -> Result<u8, CellError> {
        Ok(self.load_bytes(1)?[0])
    }

    pub fn load_u16(&mut self) -> Result<u16, CellError> {
        Ok(u16::from_le_bytes(
            self.load_bytes(2)?.try_into().expect("length checked"),
        ))
    }

    pub fn load_u32(&mut self) -> Result<u32, CellError> {
        Ok(u32::from_le_bytes(
            self.load_bytes(4)?.try_into().expect("length checked"),
        ))
    }

    pub fn load_u64(&mut self) -> Result<u64, CellError> {
        Ok(u64::from_le_bytes(
            self.load_bytes(8)?.try_into().expect("length checked"),
        ))
    }

    pub fn load_bytes(&mut self, len: usize) -> Result<&'a [u8], CellError> {
        let end = self
            .byte_offset
            .checked_add(len)
            .ok_or(CellError::InsufficientBytes)?;
        let bytes = self
            .cell
            .payload()
            .get(self.byte_offset..end)
            .ok_or(CellError::InsufficientBytes)?;
        self.byte_offset = end;
        Ok(bytes)
    }

    /// Reads a byte string encoded by [`crate::CellBuilder::store_snake`].
    ///
    /// Checks the declared byte length against `limit` before allocating or
    /// resolving anything. Overflow consumes only the next unread parent ref;
    /// dedicated continuation Cells must have canonical lengths and ref counts.
    /// The cursor stays on the parent, preserving subsequent fields and refs.
    /// On error the cursor is unchanged, but resolver charges are not refunded.
    pub fn load_snake<R: CellResolver + ?Sized>(
        &mut self,
        cells: &mut R,
        limit: usize,
    ) -> Result<Vec<u8>, CellError> {
        self.try_load(|slice| {
            let length =
                usize::try_from(slice.load_u32()?).map_err(|_| CellError::LimitExceeded)?;
            if length > limit {
                return Err(CellError::LimitExceeded);
            }

            let inline = length.min(slice.remaining_bytes());
            if inline < length && slice.cell.payload().len() != MAX_CELL_PAYLOAD {
                return Err(CellError::InvalidFormat);
            }
            // Grow only from validated data, not the untrusted declared length.
            let mut bytes = slice.load_bytes(inline)?.to_vec();
            if bytes.len() == length {
                return Ok(bytes);
            }

            let mut reference = slice.load_ref()?;
            loop {
                let cell = resolve_cell(cells, &reference)?;
                let remaining = length - bytes.len();
                let continues = remaining > MAX_CELL_PAYLOAD;
                if cell.payload().len() != remaining.min(MAX_CELL_PAYLOAD)
                    || cell.refs().len() != usize::from(continues)
                {
                    return Err(CellError::InvalidFormat);
                }
                bytes.extend_from_slice(cell.payload());
                if !continues {
                    return Ok(bytes);
                }
                reference = cell.refs()[0].clone();
            }
        })
    }

    pub fn load_ref(&mut self) -> Result<CellRef, CellError> {
        let reference = self
            .cell
            .refs()
            .get(self.ref_offset)
            .ok_or(CellError::InsufficientReferences)?
            .clone();
        self.ref_offset += 1;
        Ok(reference)
    }

    pub fn preload<T>(
        &self,
        f: impl FnOnce(&mut CellSlice<'a>) -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        f(&mut self.clone())
    }

    pub fn try_load<T>(
        &mut self,
        f: impl FnOnce(&mut CellSlice<'a>) -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        let mut next = self.clone();
        let value = f(&mut next)?;
        *self = next;
        Ok(value)
    }

    pub fn finish(self) -> Result<(), CellError> {
        if self.remaining_bytes() != 0 {
            return Err(CellError::TrailingBytes);
        }
        if self.remaining_refs() != 0 {
            return Err(CellError::TrailingReferences);
        }
        Ok(())
    }
}
