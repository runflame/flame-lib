//! Routing addresses encoded as an expected Cell sum type.

use crate::{ActorID, Dict, Predicate};
use cells::{CellBuilder, CellDecode, CellEncode, CellError, CellRef, CellResolver, CellSlice};

#[derive(Debug)]
pub enum Address {
    Predicate(Predicate),
    MessageTarget { dst: ActorID, args: Dict, gas: u64 },
}

impl Address {
    pub const TAG_PREDICATE: u8 = 0;
    pub const TAG_MESSAGE_TARGET: u8 = 1;

    pub fn to_bytes(&self) -> Result<Vec<u8>, CellError> {
        Ok(self.to_envelope()?.encode())
    }
}

impl CellEncode for Address {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        match self {
            Self::Predicate(predicate) => {
                builder.store_u8(Self::TAG_PREDICATE)?.store(predicate)?;
            }
            Self::MessageTarget { dst, args, gas } => {
                builder
                    .store_u8(Self::TAG_MESSAGE_TARGET)?
                    .store(dst)?
                    .store_u64(*gas)?;
                builder.store_ref(CellRef::resident(args.to_cell()?))?;
            }
        }
        Ok(())
    }
}

impl CellDecode for Address {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        match slice.load_u8()? {
            Self::TAG_PREDICATE => Ok(Self::Predicate(Predicate::decode(slice, cells)?)),
            Self::TAG_MESSAGE_TARGET => {
                let dst = ActorID::decode(slice, cells)?;
                let gas = slice.load_u64()?;
                let cell = cells::resolve_cell(cells, &slice.load_ref()?)?;
                let args = Dict::from_cell(&cell, cells)?;
                Ok(Self::MessageTarget { dst, args, gas })
            }
            _ => Err(CellError::InvalidFormat),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cells::Cell;

    #[test]
    fn routing_roundtrip_and_invalid_tags() {
        let address = Address::MessageTarget {
            dst: ActorID::Constructor(vec![1, 2, 3]),
            args: Dict::new(),
            gas: 42,
        };
        let cell = address.to_cell().unwrap();
        match Address::from_cell(&cell, &mut ()).unwrap() {
            Address::MessageTarget { dst, args, gas } => {
                assert_eq!(dst, ActorID::Constructor(vec![1, 2, 3]));
                assert!(args.is_empty());
                assert_eq!(gas, 42);
            }
            _ => panic!("wrong address variant"),
        }
        assert!(matches!(
            Address::from_cell(&Cell::new(vec![2], vec![]).unwrap(), &mut ()),
            Err(CellError::InvalidFormat)
        ));
    }
}
