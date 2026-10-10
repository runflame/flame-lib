use std::{ops::Deref, sync::Arc};

use cells::{Cell, CellRef};

/// A VM Cell handle with optional private, per-value prover annotations.
/// Identity and serialization depend only on `reference`, never the annotation.
#[derive(Clone, Debug)]
pub struct CellValue {
    pub(crate) reference: CellRef,
    pub(crate) witness: Option<Arc<crate::String>>,
}

impl From<CellRef> for CellValue {
    fn from(reference: CellRef) -> Self {
        Self {
            reference,
            witness: None,
        }
    }
}

impl From<Cell> for CellValue {
    fn from(cell: Cell) -> Self {
        CellRef::from(cell).into()
    }
}

impl Deref for CellValue {
    type Target = CellRef;
    fn deref(&self) -> &CellRef {
        &self.reference
    }
}
