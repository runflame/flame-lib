use crate::{Cell, CellEncode, CellError, CellRef, MAX_CELL_PAYLOAD, MAX_CELL_REFS};

/// Incrementally builds one bounded [`Cell`].
#[derive(Clone, Debug, Default)]
pub struct CellBuilder {
    payload: Vec<u8>,
    refs: Vec<CellRef>,
}

impl CellBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Edit existing bytes without changing capacity, length, or references.
    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.payload
    }

    pub fn refs(&self) -> &[CellRef] {
        &self.refs
    }

    /// Factual commitment of the current contents, without finalizing the Builder.
    pub fn commitment(&self) -> Result<crate::CellCommitment, CellError> {
        Cell::compute_commitment(&self.payload, &self.refs)
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

    /// Stores an unsigned integer in its shortest LEB128 representation.
    /// Fails without mutation if the complete encoding does not fit.
    pub fn store_leb128(&mut self, mut value: u64) -> Result<&mut Self, CellError> {
        let mut bytes = [0; 10];
        let mut len = 0;
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            bytes[len] = byte | if value == 0 { 0 } else { 0x80 };
            len += 1;
            if value == 0 {
                return self.store_bytes(&bytes[..len]);
            }
        }
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
    /// Fails without mutation if the length exceeds u32, the continuation depth
    /// exceeds u16, or payload/reference capacity is unavailable. No whole-string
    /// buffer is used.
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
        check_snake_depth(value.len() - inline)?;

        // Build only the overflow, tail first, directly into its final Cells.
        let mut next = None;
        for chunk in value[inline..].chunks(MAX_CELL_PAYLOAD).rev() {
            next = Some(CellRef::resident(Cell::new(
                chunk.to_vec(),
                next.into_iter().collect(),
            )?));
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
        value.validate_child()?;
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

fn check_snake_depth(overflow_bytes: usize) -> Result<(), CellError> {
    if overflow_bytes.div_ceil(MAX_CELL_PAYLOAD) > usize::from(u16::MAX) {
        return Err(CellError::DepthOverflow);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CellCommitment;
    use std::sync::Arc;

    #[test]
    fn borrowed_commitments_match_finalization_after_in_place_payload_edits() {
        let child = Cell::new(vec![9], vec![]).unwrap().prune(2).unwrap();
        let mut builder = CellBuilder::new();
        builder
            .store_bytes(&[1, 2])
            .unwrap()
            .store_ref(child.into())
            .unwrap();
        let first = builder.commitment().unwrap();
        assert_eq!(&first, builder.clone().build().commitment());
        let pointer = builder.payload().as_ptr();
        builder.payload_mut()[0] = 3;
        assert_eq!(pointer, builder.payload().as_ptr());
        let edited = builder.commitment().unwrap();
        assert_ne!(first.id(), edited.id());
        assert_eq!(&edited, builder.build().commitment());
    }

    #[test]
    fn reference_validation_is_atomic_and_checks_every_level() {
        let mut builder = CellBuilder::new();
        builder.store_u8(7).unwrap();
        let reference = CellRef::Unloaded(Arc::new(
            CellCommitment::new(1, vec![[1; 32], [2; 32]], vec![u16::MAX, 0]).unwrap(),
        ));
        assert!(matches!(
            builder.store_ref(reference),
            Err(CellError::DepthOverflow)
        ));
        assert_eq!(builder.used_bytes(), 1);
        assert_eq!(builder.used_refs(), 0);
        assert_eq!(builder.build().payload(), &[7]);
    }

    #[test]
    fn snake_depth_limit_is_checked_before_building_a_chain() {
        let max_overflow = MAX_CELL_PAYLOAD * usize::from(u16::MAX);
        assert_eq!(check_snake_depth(0), Ok(()));
        assert_eq!(check_snake_depth(max_overflow), Ok(()));
        assert_eq!(
            check_snake_depth(max_overflow + 1),
            Err(CellError::DepthOverflow)
        );
    }

    #[test]
    fn leb128_uses_shortest_encoding_and_capacity_failure_is_atomic() {
        for (value, expected) in [
            (0, vec![0]),
            (127, vec![127]),
            (128, vec![0x80, 1]),
            (624485, vec![0xe5, 0x8e, 0x26]),
            (
                u64::MAX,
                vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1],
            ),
        ] {
            let mut builder = CellBuilder::new();
            builder.store_leb128(value).unwrap();
            let cell = builder.build();
            assert_eq!(cell.payload(), expected);
            let mut slice = crate::CellSlice::new(&cell);
            assert_eq!(slice.load_leb128(), Ok(value));
            slice.finish().unwrap();
        }

        let mut builder = CellBuilder::new();
        builder.store_bytes(&vec![7; MAX_CELL_PAYLOAD - 1]).unwrap();
        assert!(matches!(
            builder.store_leb128(128),
            Err(CellError::PayloadCapacity)
        ));
        assert_eq!(builder.used_bytes(), MAX_CELL_PAYLOAD - 1);
        builder.store_leb128(127).unwrap();
        assert_eq!(builder.build().payload()[MAX_CELL_PAYLOAD - 1], 127);
    }
}
