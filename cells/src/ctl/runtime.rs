use std::{fmt, marker::PhantomData};

use crate::{
    CellBuilder, CellDecode, CellEncode, CellError, CellRef, CellResolver, CellSlice, resolve_cell,
};

/// A lazy CTL `^T` reference. The type describes the expected child encoding;
/// its body is resolved and validated only by [`Self::load`].
pub struct Ref<T> {
    reference: CellRef,
    expected: PhantomData<fn() -> T>,
}

impl<T> Ref<T> {
    pub fn from_reference(reference: CellRef) -> Self {
        Self {
            reference,
            expected: PhantomData,
        }
    }

    pub fn reference(&self) -> &CellRef {
        &self.reference
    }

    pub fn into_reference(self) -> CellRef {
        self.reference
    }

    pub fn from_value(value: &T) -> Result<Self, CellError>
    where
        T: CellEncode,
    {
        Ok(Self::from_reference(value.to_cell()?.into()))
    }

    pub fn load<R: CellResolver + ?Sized>(&self, cells: &mut R) -> Result<T, CellError>
    where
        T: CellDecode,
    {
        let cell = resolve_cell(cells, &self.reference)?;
        T::from_cell(&cell, cells)
    }
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Self::from_reference(self.reference.clone())
    }
}

impl<T> fmt::Debug for Ref<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Ref").field(&self.reference).finish()
    }
}

impl<T> CellEncode for Ref<T> {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_ref(self.reference.clone())?;
        Ok(())
    }
}

impl<T> CellDecode for Ref<T> {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        _cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self::from_reference(slice.load_ref()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, CellCommitment};
    use std::sync::Arc;

    struct NeverResolve;

    impl CellResolver for NeverResolve {
        fn resolve(&mut self, _: &CellRef) -> Result<Arc<Cell>, CellError> {
            panic!("decoding a reference must not resolve its body")
        }
    }

    struct Fixed(Cell, usize);

    impl CellResolver for Fixed {
        fn resolve(&mut self, _: &CellRef) -> Result<Arc<Cell>, CellError> {
            self.1 += 1;
            Ok(Arc::new(self.0.clone()))
        }
    }

    #[test]
    fn references_preserve_unloaded_and_pruned_bodies_without_type_bounds() {
        struct NoTraits;

        let child = CellRef::resident(42u8.to_cell().unwrap());
        let reference = Ref::<NoTraits>::from_reference(child.to_unloaded().unwrap());
        let encoded = reference.clone().to_cell().unwrap();
        assert!(encoded.payload().is_empty());
        assert_eq!(encoded.refs().len(), 1);
        let decoded = Ref::<NoTraits>::from_cell(&encoded, &mut NeverResolve).unwrap();
        assert!(decoded.reference().is_unloaded());
        assert_eq!(decoded.clone().into_reference().id(), child.id());
        assert!(format!("{decoded:?}").starts_with("Ref("));

        let absent = Ref::<u8>::from_reference(decoded.into_reference());
        assert_eq!(
            absent.load(&mut ()),
            Err(CellError::MissingCell(child.id()))
        );

        let pruned = Cell::from_pruned(1, vec![[7; 32]], vec![0]).unwrap();
        let reference = Ref::<u8>::from_reference(pruned.into());
        let parent = reference.to_cell().unwrap();
        let decoded = Ref::<u8>::from_cell(&parent, &mut NeverResolve).unwrap();
        assert_eq!(decoded.reference().id(), reference.reference().id());
        assert_eq!(decoded.load(&mut ()), Err(CellError::PrunedCell));
    }

    #[test]
    fn opening_checks_resolver_commitments_and_exact_child_encoding() {
        let reference = Ref::from_value(&0x1234u16).unwrap();
        let child = reference.reference().as_resident().unwrap().clone();
        let mut resolver = Fixed(child.clone(), 0);
        assert_eq!(reference.load(&mut resolver), Ok(0x1234));
        assert_eq!(resolver.1, 1);

        assert!(matches!(
            reference.load(&mut Fixed(1u8.to_cell().unwrap(), 0)),
            Err(CellError::CellHashMismatch { .. })
        ));
        let forged = CellCommitment::new(0, vec![child.id()], vec![1]).unwrap();
        let forged = Ref::<u16>::from_reference(CellRef::Unloaded(Arc::new(forged)));
        assert_eq!(
            forged.load(&mut resolver),
            Err(CellError::CellCommitmentMismatch(child.id()))
        );

        let trailing_bytes = Ref::<u8>::from_reference(child.into());
        assert_eq!(trailing_bytes.load(&mut ()), Err(CellError::TrailingBytes));
        let trailing_refs = Ref::<u8>::from_reference(
            Cell::new(vec![1], vec![2u8.to_cell().unwrap().into()])
                .unwrap()
                .into(),
        );
        assert_eq!(
            trailing_refs.load(&mut ()),
            Err(CellError::TrailingReferences)
        );
    }
}
