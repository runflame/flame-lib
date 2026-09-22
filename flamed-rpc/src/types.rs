//! What travels on the wire: one newtype per kind of thing, and one result
//! type per method.
//!
//! Every newtype has a public inner field, so a conversion at a call site is
//! a one-liner and this crate never has to name a `flamevm` or `flamechain`
//! type. One kind of newtype per thing is what stops a block id being passed
//! where a txid goes — with positional parameters, nothing else would catch
//! it.

use serde::{Deserialize, Serialize};

use crate::codec::{base64_newtype, hex32_newtype};

hex32_newtype! {
    /// A block hash.
    BlockId;
    /// A transaction id.
    TxId;
    /// A contract id.
    ContractId;
    /// A predicate's compressed point.
    PredicatePoint;
}

base64_newtype! {
    /// A Utreexo proof, in `readerwriter`'s encoding.
    ProofBytes;
    /// A contract, as its `CellEnvelope`.
    ContractEnvelope;
    /// A block transaction, as `BlockTx::to_bytes` gives it.
    BlockTxEnvelope;
}

/// The chain's current tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipResult {
    /// The tip block's hash.
    pub hash: BlockId,
    /// Its height; genesis is 0.
    pub height: u64,
    /// The root of the contract accumulator at that tip.
    #[serde(with = "crate::codec::hex32")]
    pub contract_root: [u8; 32],
}

/// What the node knows about one contract's membership.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ProofResult {
    /// Unspent at the tip, with a proof valid against it.
    Unspent {
        /// The membership proof.
        proof: ProofBytes,
    },
    /// Spent, by this transaction at this height.
    Spent {
        /// The height of the block that spent it.
        height: u64,
        /// The transaction that spent it.
        txid: TxId,
    },
    /// The node has never seen this contract.
    Unknown,
}

/// One contract as the node archived it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractResult {
    /// The height that created it.
    pub height: u64,
    /// The transaction that created it.
    pub txid: TxId,
    /// The predicate that locks it.
    pub predicate: PredicatePoint,
    /// Its bytes, as published.
    pub bytes: ContractEnvelope,
}

/// Where a transaction is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum TxStatusResult {
    /// Neither in the mempool nor in the chain.
    Unknown,
    /// Waiting in this node's mempool.
    Mempool,
    /// In a block.
    Confirmed {
        /// The height of that block.
        height: u64,
        /// Its hash.
        block: BlockId,
    },
}

/// Where and how a contract was spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpentAt {
    /// The height of the spending block.
    pub height: u64,
    /// The spending transaction.
    pub txid: TxId,
}

/// One contract a scan found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanEntry {
    /// The contract's id.
    pub id: ContractId,
    /// The height that created it.
    pub height: u64,
    /// The transaction that created it.
    pub txid: TxId,
    /// The predicate that locks it.
    pub predicate: PredicatePoint,
    /// Its bytes, as published.
    pub bytes: ContractEnvelope,
    /// Absent while the contract is unspent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent: Option<SpentAt>,
}

/// Everything a scan found, and the tip it was answered at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    /// The node's height when it answered.
    pub tip_height: u64,
    /// The contracts, in height order.
    pub outputs: Vec<ScanEntry>,
}

/// The most predicates one `scan` may name.
///
/// It bounds the question, not the answer: a predicate with a long history
/// still walks every contract under it. Bounding the work would take a cap on
/// hits and a cursor, which this wire format does not have.
pub const MAX_SCAN_PREDICATES: usize = 1024;

/// The most ids one `proofs` may name.
pub const MAX_PROOF_IDS: usize = 1024;

/// Server-defined JSON-RPC error codes, in the -32000..-32099 range.
pub mod codes {
    /// The node has no such contract.
    pub const NOT_FOUND: i32 = -32001;
    /// The mempool refused a submitted transaction.
    pub const MEMPOOL_REJECTED: i32 = -32002;
    /// The request named more items than the method allows.
    pub const LIMIT_EXCEEDED: i32 = -32003;
    /// A blob did not decode into what it claims to be.
    pub const INVALID_BYTES: i32 = -32004;
}
