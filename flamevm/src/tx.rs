use bulletproofs::r1cs::R1CSProof;
use merkle::{Hash, MerkleItem, MerkleTree};
use musig::Signature;
use serde::{Deserialize, Serialize};

/// Header metadata for the transaction
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct TxHeader {
    /// Version of the transaction
    pub version: u32,

    /// Timestamp before which tx is invalid, compatible with Bitcoin
    pub locktime: u32,
}

pub struct ExternalTx {
    /// Header metadata
    pub header: TxHeader,

    /// Script representing the transaction
    pub script: Vec<u8>,

    /// Aggregated signature of the txid
    pub signature: Signature,

    /// Constraint system proof for all the constraints
    pub proof: R1CSProof,
}

pub struct InternalTx {}

/// Transaction ID is a unique 32-byte identifier of a transaction effects represented by `TxLog`.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxID(pub Hash);

/// Entry in a transaction log. All entries are hashed into a [transaction ID](TxID).
///
/// Linear-value variants (`Output` carries a `Cell`) prevent us from
/// deriving `Clone`/`Debug`/`Serialize`/`Deserialize` here; downstream
/// code wanting those should hash entries to bytes first or wrap.
pub enum TxEntry {
    /// Plain data entry created by `log` instruction. Contains arbitrary binary string.
    Data(Vec<u8>),

    /// Output: a newly sealed cell, emitted by the `output` opcode.
    Output(crate::Cell),
    // Future variants (preserved here as comments for the historical record):
    // Input(CellID), Issue(...), Retire(...), Fee(u64), Send(Message), etc.
}