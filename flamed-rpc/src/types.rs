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
    /// An actor hash.
    ActorId;
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
    /// An output's note: the `Data` entry after it, byte for byte.
    NoteEnvelope;
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
    /// The `Data` entry right after it in its log, if any: the note a
    /// confidential output's recipient opens. Absent when none followed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteEnvelope>,
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

/// Execution category and its data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionData {
    /// A signed transaction submitted by a client.
    External,
    /// Successful asynchronous message execution.
    Internal {
        /// Execution which sent the received message.
        parent: TxId,
        /// Target of the received message.
        actor: ActorId,
    },
    /// Failed message delivery. Effects describe its refund.
    InternalFailed {
        /// Execution which sent the received message.
        parent: TxId,
        /// Target of the received message.
        actor: ActorId,
        /// VM error text. This text is not a consensus field.
        error: String,
    },
}

/// Summary of a confirmed execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionSummary {
    /// Transaction / execution identifier.
    pub id: TxId,
    /// Execution category and its data.
    pub execution: ExecutionData,
    /// Transaction fee in sparks, as an exact decimal string.
    /// This value excludes storage fees.
    pub fee_sparks: String,
    /// Number of consumed outputs.
    pub inputs: u32,
    /// Number of created outputs, including refunds.
    pub outputs: u32,
    /// Number of outgoing asynchronous messages.
    pub messages: u32,
}

/// The state committed by a block header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCommitment {
    /// Specialized Utreexo accumulator commitment.
    #[serde(with = "crate::codec::hex32")]
    pub contracts: [u8; 32],
    /// Cell Trie commitment to actor content and persistent availability.
    #[serde(with = "crate::codec::hex32")]
    pub actors: [u8; 32],
    /// Available storage units committed by the block.
    pub available_storage_units: u64,
}

/// The complete block header. Block headers contain no timestamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Consensus protocol version.
    pub version: u32,
    /// Block height; genesis is zero.
    pub height: u64,
    /// Opaque authenticated Bitcoin/core-block identity supplied by the caller.
    #[serde(with = "crate::codec::hex32")]
    pub core_block_hash: [u8; 32],
    /// Previous block identifier, as committed in the header.
    pub parent: BlockId,
    /// Committed witness root, as hex.
    #[serde(with = "crate::codec::hex32")]
    pub witness_root: [u8; 32],
    /// Committed effects root, as hex.
    #[serde(with = "crate::codec::hex32")]
    pub effects_root: [u8; 32],
    /// State committed by the block.
    pub state: StateCommitment,
}

/// A block header with its identifier and execution counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSummary {
    /// Block identifier.
    pub id: BlockId,
    /// Complete consensus header.
    pub header: BlockHeader,
    /// External transaction count.
    pub transactions: u32,
    /// Internal execution count, including failures.
    pub internal: u32,
    /// Failed internal execution count.
    pub failed: u32,
    /// Serialized block size in bytes; zero for synthetic genesis.
    pub size_bytes: u64,
}

/// A block and its execution summaries in consensus order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockResult {
    /// Header summary.
    pub summary: BlockSummary,
    /// Both external and internal executions.
    pub executions: Vec<TransactionSummary>,
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
    /// The node has no such contract or block.
    pub const NOT_FOUND: i32 = -32001;
    /// The mempool refused a submitted transaction.
    pub const MEMPOOL_REJECTED: i32 = -32002;
    /// The request named more items than the method allows.
    pub const LIMIT_EXCEEDED: i32 = -32003;
    /// A blob did not decode into what it claims to be.
    pub const INVALID_BYTES: i32 = -32004;
}
