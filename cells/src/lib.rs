//! Content-addressed Cells and the minimal structures built from them.

mod builder;
mod cell;
mod codec;
pub mod ctl;
mod error;
mod index;
mod slice;
#[cfg(test)]
mod snake;
mod transport;
mod trie;

pub use builder::CellBuilder;
pub use cell::{
    Cell, CellCommitment, CellID, CellReader, CellRef, CellResolver, CellView, MAX_CELL_LEVEL,
    MAX_CELL_PAYLOAD, MAX_CELL_REFS, read_cell, resolve_cell,
};
pub use codec::{CellDecode, CellEncode};
pub use error::CellError;
pub use index::{CellEnvelope, CellIndex};
pub use slice::CellSlice;
pub use transport::GasMeter;
pub use trie::{MAX_TRIE_KEY_BYTES, Trie};
