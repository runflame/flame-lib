//! The history the chain does not keep.
//!
//! A `Blockchain` keeps roots and an accumulator; it forgets which
//! transaction created which contract, and it never knew which predicate
//! locked it. A wallet needs both. These two maps are that memory, and they
//! live in RAM: an archival node rebuilds them by replaying `blocks.bin`.

pub mod outputindex;
pub mod txindex;

pub use outputindex::{OutputIndex, OutputRecord, SpendRecord};
pub use txindex::{TxIndex, TxRecord};
