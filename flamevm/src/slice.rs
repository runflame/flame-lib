use std::sync::Arc;

use cells::{Cell, CellError, CellRef};

/// Owned VM cursor; copies share immutable data, never cursor state or authority.
#[derive(Clone, Debug)]
pub struct Slice {
    cell: Arc<Cell>,
    byte_offset: usize,
    ref_offset: usize,
}

impl Slice {
    pub fn new(cell: Arc<Cell>) -> Result<Self, CellError> {
        if cell.is_pruned() {
            return Err(CellError::PrunedCell);
        }
        Ok(Self {
            cell,
            byte_offset: 0,
            ref_offset: 0,
        })
    }

    pub fn remaining_bytes(&self) -> usize {
        self.bytes().len()
    }
    pub fn remaining_refs(&self) -> usize {
        self.refs().len()
    }

    /// Factual commitment of the unread byte/ref streams as an ordinary Cell.
    pub fn commitment(&self) -> Result<cells::CellCommitment, CellError> {
        Cell::compute_commitment(self.bytes(), self.refs())
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.cell.payload()[self.byte_offset..]
    }
    pub(crate) fn refs(&self) -> &[CellRef] {
        &self.cell.refs()[self.ref_offset..]
    }

    pub(crate) fn advance_bytes(&mut self, count: usize) {
        assert!(count <= self.remaining_bytes());
        self.byte_offset += count;
    }

    pub(crate) fn advance_refs(&mut self, count: usize) {
        assert!(count <= self.remaining_refs());
        self.ref_offset += count;
    }
}
