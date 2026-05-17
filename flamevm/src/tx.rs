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

    /// Program representing the transaction
    pub program: Vec<u8>,

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
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum TxEntry {
    /// Transaction [header](self::TxHeader).
    /// This entry is not present in the transaction log, but used only for computing a TxID.
    //Header(TxHeader),
    /// Asset issuance entry that consists of a _flavor commitment_ and a _quantity commitment_.
    //Issue(CompressedRistretto, CompressedRistretto),
    /// Asset retirement entry that consists of a _flavor commitment_ and a _quantity commitment_.
    //Retire(CompressedRistretto, CompressedRistretto),
    /// Input entry that signals that a contract was spent. Contains the [ID](crate::contract::ContractID) of a contract.
    //Input(ContractID),
    /// Output entry that signals that a contract was created. Contains the [Contract](crate::contract::Contract).
    //Output(Contract),
    /// Amount of fee being paid (transaction may have multiple fee entries).
    //Fee(u64),
    /// Plain data entry created by `log` instruction. Contains arbitrary binary string.
    Data(Vec<u8>),
}