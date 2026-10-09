//! Content-addressed Cells and the minimal structures built from them.

mod boc;
mod builder;
mod cell;
mod codec;
pub mod ctl;
mod error;
mod slice;
#[cfg(test)]
mod snake;
mod trie;

pub use boc::{BagOfCells, BoCID, CellEnvelope, GasMeter};
pub use builder::CellBuilder;
pub use cell::{
    Cell, CellCommitment, CellID, CellRef, CellResolver, CellView, MAX_CELL_LEVEL,
    MAX_CELL_PAYLOAD, MAX_CELL_REFS, resolve_cell,
};
pub use codec::{CellDecode, CellEncode};
pub use error::CellError;
pub use slice::CellSlice;
pub use trie::{MAX_TRIE_KEY_BYTES, Trie};
