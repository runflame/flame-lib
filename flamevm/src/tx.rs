use bulletproofs::r1cs::R1CSProof;
use curve25519_dalek::ristretto::CompressedRistretto;
// `MerkleItem` / `MerkleTree` join when Phase 17 wires
// `TxID::from_log(txlog)` via the merkle tree.
use merkle::Hash;
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

    /// Input: a consumed cell's identity. Emitted by `input` (external-only).
    /// Commits the cell that the transaction has consumed without re-storing
    /// the payload — the existence of the cell is independently asserted by
    /// the Utreexo proof outside the VM.
    Input(crate::cell::CellID),

    /// Output: a newly sealed cell, emitted by the `output` opcode.
    Output(crate::Cell),

    /// Issuance: an asset value has just been *created* into circulation.
    /// Carries `(qty_point, flv_point)` — Pedersen commitments to the
    /// quantity and flavor scalars.
    ///
    /// For the cleartext issuance branch (`issue` with `Int253` qty),
    /// both commitments are unblinded (blinding factor = 0). For the
    /// encrypted branch (Phase 11/12), the commitments are the live
    /// blinded points whose openings are proven through the constraint
    /// system.
    Issue(CompressedRistretto, CompressedRistretto),

    /// Retirement: an asset value has been *destroyed* from circulation.
    /// Same `(qty_point, flv_point)` shape as `Issue`. Cleartext or
    /// encrypted symmetrically.
    Retire(CompressedRistretto, CompressedRistretto),
    // Future variants (preserved here as comments for the historical record):
    // Fee(u64), Send(Message), etc.
}