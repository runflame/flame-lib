use crate::{BagOfCells, Cell, CellBuilder, CellEnvelope, CellError, CellResolver, CellSlice};

/// Canonically writes one expected type into a Cell builder.
pub trait CellEncode {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError>;

    fn to_cell(&self) -> Result<Cell, CellError> {
        let mut builder = CellBuilder::new();
        builder.store(self)?;
        Ok(builder.build())
    }

    /// Collects the root and its resident descendants for standalone transport.
    fn to_envelope(&self) -> Result<CellEnvelope, CellError> {
        let root = std::sync::Arc::new(self.to_cell()?);
        CellEnvelope::new(root.id(), BagOfCells::collect(root)?)
    }
}

/// Canonically reads one expected type from a Cell slice.
pub trait CellDecode: Sized {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError>;

    fn from_cell<R: CellResolver + ?Sized>(cell: &Cell, cells: &mut R) -> Result<Self, CellError> {
        let mut slice = CellSlice::new(cell);
        let value = Self::decode(&mut slice, cells)?;
        slice.finish()?;
        Ok(value)
    }
}

macro_rules! integer_codec {
    ($type:ty, $store:ident, $load:ident) => {
        impl CellEncode for $type {
            fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
                builder.$store(*self)?;
                Ok(())
            }
        }

        impl CellDecode for $type {
            fn decode<R: CellResolver + ?Sized>(
                slice: &mut CellSlice<'_>,
                _cells: &mut R,
            ) -> Result<Self, CellError> {
                slice.$load()
            }
        }
    };
}

integer_codec!(u8, store_u8, load_u8);
integer_codec!(u16, store_u16, load_u16);
integer_codec!(u32, store_u32, load_u32);
integer_codec!(u64, store_u64, load_u64);

impl<const N: usize> CellEncode for [u8; N] {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_bytes(self)?;
        Ok(())
    }
}

impl<const N: usize> CellDecode for [u8; N] {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        _cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(slice
            .load_bytes(N)?
            .try_into()
            .expect("requested array length"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellRef, MAX_CELL_PAYLOAD, MAX_CELL_REFS};

    struct PayloadThenRef(CellRef);

    impl CellEncode for PayloadThenRef {
        fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
            builder.store_u8(9)?.store_ref(self.0.clone())?;
            Ok(())
        }
    }

    fn leaf(byte: u8) -> Cell {
        Cell::new(vec![byte], vec![]).unwrap()
    }

    #[test]
    fn builder_and_slice_are_atomic_and_exact() {
        let child = CellRef::resident(leaf(7));
        let mut full = CellBuilder::new();
        full.store_bytes(&vec![0; MAX_CELL_PAYLOAD - 1]).unwrap();
        for _ in 0..MAX_CELL_REFS {
            full.store_ref(child.clone()).unwrap();
        }
        assert_eq!(
            full.store(&PayloadThenRef(child.clone())).unwrap_err(),
            CellError::ReferenceCapacity
        );
        assert_eq!(full.used_bytes(), MAX_CELL_PAYLOAD - 1);
        assert_eq!(full.used_refs(), MAX_CELL_REFS);

        let mut builder = CellBuilder::new();
        builder.store_u16(0x1234).unwrap().store_ref(child).unwrap();
        let cell = builder.build();
        let mut slice = CellSlice::new(&cell);

        assert_eq!(slice.load_ref().unwrap().id(), cell.refs()[0].id());
        assert_eq!(slice.remaining_bytes(), 2);
        assert_eq!(slice.preload(|next| next.load_u16()).unwrap(), 0x1234);
        assert_eq!(slice.remaining_bytes(), 2);
        assert_eq!(
            slice.try_load(|next| {
                next.load_u16()?;
                next.load_u8()
            }),
            Err(CellError::InsufficientBytes)
        );
        assert_eq!(slice.remaining_bytes(), 2);
        assert_eq!(slice.try_load(|next| next.load_u16()).unwrap(), 0x1234);
        slice.finish().unwrap();

        assert_eq!(
            CellSlice::new(&cell).finish(),
            Err(CellError::TrailingBytes)
        );
        let only_ref = Cell::new(vec![], vec![cell.refs()[0].clone()]).unwrap();
        assert_eq!(
            CellSlice::new(&only_ref).finish(),
            Err(CellError::TrailingReferences)
        );
    }

    #[test]
    fn primitive_codecs_round_trip_exactly() {
        #[derive(Debug, PartialEq)]
        struct Example {
            count: u32,
            id: [u8; 32],
        }

        impl CellEncode for Example {
            fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
                builder.store(&self.count)?.store(&self.id)?;
                Ok(())
            }
        }

        impl CellDecode for Example {
            fn decode<R: CellResolver + ?Sized>(
                slice: &mut CellSlice<'_>,
                cells: &mut R,
            ) -> Result<Self, CellError> {
                Ok(Self {
                    count: u32::decode(slice, cells)?,
                    id: <[u8; 32]>::decode(slice, cells)?,
                })
            }
        }

        let expected = Example {
            count: 42,
            id: [3; 32],
        };
        let cell = expected.to_cell().unwrap();
        assert_eq!(Example::from_cell(&cell, &mut ()).unwrap(), expected);

        let with_trailing = Cell::new([cell.payload(), &[0]].concat(), vec![]).unwrap();
        assert_eq!(
            Example::from_cell(&with_trailing, &mut ()).unwrap_err(),
            CellError::TrailingBytes
        );
    }
}
