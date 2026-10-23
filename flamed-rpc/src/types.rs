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
    /// A cell hash.
    CellId;
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
    /// An actor's raw VM bytecode.
    ActorCode;
    /// An actor's public state as a `CellEnvelope`.
    ActorStateEnvelope;
    /// One query argument or result, as a VM Value `CellEnvelope`.
    ValueEnvelope;
}

/// A read-only actor result at one committed chain tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorQueryResult {
    /// Block against whose state the getter ran.
    pub block: BlockId,
    /// Height of that block.
    pub height: u64,
    /// One data Value (Scalar, Point, Cell, or recursively data-only portable Dict).
    pub value: ValueEnvelope,
    /// VM execution gas consumed, excluding transport decoding.
    pub gas_used: u64,
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

/// An archived contract, its spend status, and its public payload.
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
    /// The block that created it, including genesis.
    pub created_block: BlockId,
    /// Spend status at `tip`. An unspent proof is valid against this tip.
    pub status: ProofResult,
    /// Chain tip at which the spend status was read.
    pub tip: TipResult,
    /// Contract anchor, as hex.
    #[serde(with = "crate::codec::hex32")]
    pub anchor: [u8; 32],
    /// Public payload or available cells when some bodies are missing.
    /// Confidential tokens expose commitments only.
    pub decoded_payload: Option<TxValue>,
    /// Why the payload could not be decoded.
    pub payload_error: Option<String>,
}

/// Current actor state, code, storage, and decoded views.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorResult {
    /// The requested actor address.
    pub id: ActorId,
    /// The height at which all fields were read.
    pub height: u64,
    /// The block hash at that height.
    pub block: BlockId,
    /// Combined code/data root, including a whole-root cut when frozen.
    pub root_hash: CellId,
    /// Whether the whole actor content root is pruned.
    pub frozen: bool,
    /// Code child commitment; unavailable while the whole root is frozen.
    pub code_hash: Option<CellId>,
    /// Data child commitment; unavailable while the whole root is frozen.
    pub state_hash: Option<CellId>,
    /// Root program payload length; descendants are read on demand.
    pub code_size: u64,
    /// Logical state size in encoded cell bytes.
    pub state_size: u64,
    /// Resident code/state bytes plus lease records.
    pub storage_used: u64,
    /// Capacity of leases valid at this height, in bytes.
    pub storage_capacity: u64,
    /// Native code Cell envelope as base64. Null if the root is unavailable.
    pub code: Option<ActorCode>,
    /// State envelope as base64. Null if the root body is unavailable.
    /// Descendant cells can still be missing.
    pub state: Option<ActorStateEnvelope>,
    /// Public state or available cells when some bodies are missing.
    pub decoded_state: Option<TxValue>,
    /// Why the state could not be decoded.
    pub state_error: Option<String>,
    /// Instructions in bytecode order, up to the first parse error.
    pub instructions: Vec<InstructionView>,
    /// Missing code or a parse error with its byte offset.
    pub code_error: Option<String>,
}

/// One decoded VM instruction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionView {
    /// Byte offset in the original code.
    pub offset: u64,
    /// Instruction mnemonic and complete inline arguments.
    pub text: String,
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
    /// The creating block's time, in Unix seconds; see [`BlockSummary::time`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
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

/// One contract an address can spend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnspentEntry {
    /// The contract's id.
    pub id: ContractId,
    /// The transaction that created it.
    pub txid: TxId,
    /// Its bytes, as published.
    pub bytes: ContractEnvelope,
    /// The `Data` entry right after it in its log, if any. Absent when none
    /// followed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteEnvelope>,
    /// The height that created it. Absent while its transaction is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    /// The creating block's time, in Unix seconds; see [`BlockSummary::time`].
    /// Absent while pending, or when the indexer has no time for the block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
    /// While pending: when this node admitted its transaction, in Unix
    /// seconds. Absent once confirmed, or when the node has restarted since
    /// admitting it; see [`PendingTransaction::received_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<u64>,
    /// A membership proof valid against `tip`. Absent while its transaction
    /// is pending: a contract the chain has not inserted has nothing to prove.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<ProofBytes>,
}

/// What one address can spend, and the tip it was answered at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnspentResult {
    /// The tip every proof is valid against.
    pub tip: TipResult,
    /// The predicate the address resolved to.
    pub predicate: PredicatePoint,
    /// Confirmed contracts in creation order, then pending ones in mempool
    /// admission order.
    pub outputs: Vec<UnspentEntry>,
}

/// One contract a history entry created or spent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryContract {
    /// The contract's id.
    pub id: ContractId,
    /// The predicate that locks it: which of the asked addresses it is.
    pub predicate: PredicatePoint,
    /// Its bytes, as published.
    pub bytes: ContractEnvelope,
    /// The `Data` entry right after it in its creating log, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteEnvelope>,
}

/// One transaction that created or spent a contract of the asked addresses.
///
/// An entry never changes once the transaction has confirmed: it says what
/// happened, not what is true now. `unspent` says what is true now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The transaction, or all zeros for the genesis allocation.
    pub txid: TxId,
    /// Its height. Absent while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    /// Its block's time, in Unix seconds; see [`BlockSummary::time`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
    /// When this node admitted it, in Unix seconds; see
    /// [`TransactionResult::received_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<u64>,
    /// Its fee in sparks, as an exact decimal string.
    pub fee_sparks: String,
    /// Contracts it created for the asked addresses: by address, then in
    /// log order.
    pub created: Vec<HistoryContract>,
    /// Contracts of the asked addresses it spent, each with the bytes and
    /// note it was created with: by address, then in log order.
    pub spent: Vec<HistoryContract>,
    /// Who else's contracts it spent: on a receive, who paid. Each
    /// predicate once, in log order.
    #[serde(default)]
    pub others_spent: Vec<Counterparty>,
    /// Who else it created contracts for: on a send, who was paid; on a
    /// receive, the payer's change. Each predicate once, in log order.
    #[serde(default)]
    pub others_created: Vec<Counterparty>,
}

/// The other side of a history entry: a predicate none of the asked
/// addresses resolved to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counterparty {
    /// The predicate.
    pub predicate: PredicatePoint,
    /// Its tracking address, `f1…` / `t1…`, when it is one. A receiving
    /// address cannot be rebuilt from a predicate: its encryption key is
    /// not on chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// A page of history, and the cursors around it.
///
/// A cursor is a readable string for debugging, not a format: pass it back
/// as it came, and expect it to change without notice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryResult {
    /// The tip this page was answered at.
    pub tip: TipResult,
    /// The predicate each address resolved to, in the order asked.
    pub predicates: Vec<PredicatePoint>,
    /// Confirmed entries: newest first on a first or older page, oldest
    /// first on a newer page.
    pub entries: Vec<HistoryEntry>,
    /// On a first or older page, where older entries continue. Null when
    /// there are none, and always on a newer page.
    pub older: Option<String>,
    /// On a first or newer page, where to read what comes after this page.
    /// Null on an older page.
    pub newer: Option<String>,
    /// On a newer page: whether entries past `newer` already exist, so it
    /// is worth asking again right away. False on other pages.
    pub has_newer: bool,
    /// With `with_pending`, on a first or newer page: the mempool's
    /// transactions that touch the asked addresses, in admission order,
    /// without a height or time. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Vec<HistoryEntry>>,
    /// Whether `pending` was cut at [`MAX_PAGE_SIZE`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_truncated: bool,
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
    /// Flames issued for the burns of the core blocks the block moved its
    /// anchor over. Effects are the reward outputs.
    Mint,
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
    /// The header's `locktime`, as signed: `0` for no lock, otherwise a
    /// height or Unix time as in Bitcoin. The chain does not check it yet.
    /// Zero for internal executions.
    pub locktime: u32,
}

/// A public execution effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum TxEntry {
    /// The transaction header.
    Header {
        /// Protocol version.
        version: u32,
        /// Transaction lock time.
        locktime: u32,
    },
    /// Bytes published by the execution.
    Data {
        /// All bytes, encoded as base64.
        #[serde(with = "crate::codec::base64")]
        bytes: Vec<u8>,
    },
    /// A consumed contract.
    Input {
        /// Contract identifier.
        contract: ContractId,
    },
    /// A received message.
    Receive {
        /// Message identifier.
        #[serde(with = "crate::codec::hex32")]
        message: [u8; 32],
    },
    /// A created contract.
    Output {
        /// Contract identifier.
        contract: ContractId,
        /// Predicate that locks the contract.
        predicate: PredicatePoint,
        /// Contract anchor.
        #[serde(with = "crate::codec::hex32")]
        anchor: [u8; 32],
        /// Complete public payload, including available cells.
        payload: TxValue,
    },
    /// A newly deployed actor.
    ActorDeploy {
        /// Actor identifier.
        actor: ActorId,
        /// Constructor code Cell envelope, encoded as base64.
        #[serde(with = "crate::codec::base64")]
        code: Vec<u8>,
        /// Code root hash.
        #[serde(with = "crate::codec::hex32")]
        code_hash: [u8; 32],
    },
    /// An actor state update.
    ActorSave {
        /// Actor identifier.
        actor: ActorId,
        /// State root hash.
        #[serde(with = "crate::codec::hex32")]
        state_hash: [u8; 32],
        /// Saved state, including available cells.
        state: TxValue,
    },
    /// Authenticated hydration of the actor's complete code/data image.
    ActorRestore {
        /// Actor identifier.
        actor: ActorId,
        /// Factual root of the supplied partial image.
        state_hash: CellId,
        /// Supplied native Cell graph, encoded as base64.
        state: ActorStateEnvelope,
    },
    /// An actor code update.
    SetCode {
        /// Actor identifier.
        actor: ActorId,
        /// New code Cell envelope, encoded as base64.
        #[serde(with = "crate::codec::base64")]
        code: Vec<u8>,
        /// Code root hash.
        #[serde(with = "crate::codec::hex32")]
        code_hash: [u8; 32],
    },
    /// Removal of an actor.
    ActorDestroy {
        /// Actor identifier.
        actor: ActorId,
    },
    /// An outgoing asynchronous message.
    Send {
        /// Message identifier.
        #[serde(with = "crate::codec::hex32")]
        message: [u8; 32],
        /// Message destination or constructor code.
        target: ActorTarget,
        /// Sending actor, if present.
        caller: Option<ActorId>,
        /// Message anchor.
        #[serde(with = "crate::codec::hex32")]
        anchor: [u8; 32],
        /// One portable application value.
        payload: TxValue,
        /// Gas limit as an exact decimal string.
        gas_limit: String,
        /// Predicate for the gas refund.
        refund_predicate: PredicatePoint,
    },
    /// Storage purchased by an actor.
    StoragePurchase {
        /// Actor identifier.
        actor: ActorId,
        /// Number of storage bytes.
        bytes: u64,
        /// Height at which the storage expires.
        expiry_height: u64,
        /// Storage fee as an exact scalar string.
        fee_sparks: String,
    },
    /// Transaction fee, excluding storage fees.
    Fee {
        /// Fee in sparks as an exact decimal string.
        sparks: String,
    },
    /// Public asset issuance.
    IssuePublic {
        /// Issued quantity as an exact scalar string.
        quantity: String,
        /// Asset flavor as an exact scalar string.
        flavor: String,
    },
    /// Confidential asset issuance.
    IssuePrivate {
        /// Compressed quantity commitment.
        #[serde(with = "crate::codec::hex32")]
        quantity_commitment: [u8; 32],
        /// Compressed flavor commitment.
        #[serde(with = "crate::codec::hex32")]
        flavor_commitment: [u8; 32],
    },
    /// Retired assets.
    Retire {
        /// Compressed quantity commitment.
        #[serde(with = "crate::codec::hex32")]
        quantity_commitment: [u8; 32],
        /// Compressed flavor commitment.
        #[serde(with = "crate::codec::hex32")]
        flavor_commitment: [u8; 32],
    },
}

/// A message destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorTarget {
    /// An existing actor addressed by its immutable ID.
    Address {
        /// Actor identifier.
        actor: ActorId,
    },
    /// An actor addressed with its native constructor graph.
    Init {
        /// Actor identifier, equal to the code root hash.
        actor: ActorId,
        /// Available constructor graph; absent if even its root is unavailable.
        ctor: Option<ActorCode>,
    },
    /// An actor addressed with an authenticated partial state image.
    Restore {
        /// Actor identifier.
        actor: ActorId,
        /// Factual root of the supplied image.
        state_hash: CellId,
        /// Available image; absent if even its root is unavailable.
        state: Option<ActorStateEnvelope>,
    },
}

/// A public VM value.
/// Scalar strings use signed decimal when the magnitude fits in `u128`.
/// Larger scalars use `0x` followed by their canonical little-endian bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TxValue {
    /// A scalar.
    Scalar {
        /// Exact scalar string.
        value: String,
    },
    /// A dictionary with all its values available.
    Dict {
        /// Entries in ascending scalar-key order.
        entries: Vec<DictEntry>,
    },
    /// A compressed curve point.
    Point {
        /// Compressed point bytes.
        #[serde(with = "crate::codec::hex32")]
        hex: [u8; 32],
    },
    /// A public token.
    ClearToken {
        /// Quantity as an exact scalar string.
        quantity: String,
        /// Flavor as an exact scalar string.
        flavor: String,
    },
    /// A confidential token.
    Token {
        /// Compressed quantity commitment.
        #[serde(with = "crate::codec::hex32")]
        quantity_commitment: [u8; 32],
        /// Compressed flavor commitment.
        #[serde(with = "crate::codec::hex32")]
        flavor_commitment: [u8; 32],
    },
    /// A Cell value, or a value with missing bodies that prevent full decoding.
    Cells {
        /// Value root hash. The root body can also be absent.
        root: CellId,
        /// All reachable available cells, ordered by hash.
        /// References to absent cells remain hashes.
        cells: Vec<ValueCell>,
    },
}

/// One entry in a public dictionary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictEntry {
    /// Key as an exact scalar string.
    pub key: String,
    /// Public value.
    pub value: TxValue,
}

/// One available body from a Cell value or partially archived value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueCell {
    /// Cell identifier.
    pub id: CellId,
    /// Complete cell payload, encoded as base64.
    #[serde(with = "crate::codec::base64")]
    pub data: Vec<u8>,
    /// Child hashes in cell reference order.
    pub refs: Vec<CellId>,
}

/// One confirmed execution, its location, and its public effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionResult {
    /// Execution summary.
    pub summary: TransactionSummary,
    /// Containing block height.
    pub height: u64,
    /// Containing block identifier.
    pub block: BlockId,
    /// The containing block's time, in Unix seconds; see [`BlockSummary::time`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_time: Option<u64>,
    /// When this node admitted it to its mempool, in Unix seconds. Absent
    /// when this node never held it pending, or has restarted since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<u64>,
    /// Canonical public effect log as a base64 CellEnvelope.
    #[serde(with = "crate::codec::base64")]
    pub log: Vec<u8>,
    /// Public effects in execution order. Absent unless `decode_effects` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<TxEntry>>,
}

/// A page of confirmed executions, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionsResult {
    /// Execution summaries in this page.
    pub transactions: Vec<TransactionSummary>,
    /// Whether more matching executions precede this page.
    /// Use the last execution's id as `before` for the next page.
    pub has_more: bool,
}

/// One transaction waiting in the mempool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransaction {
    /// Transaction identifier.
    pub id: TxId,
    /// Full transaction, limits, and proofs as a base64 BlockTx envelope.
    pub block_tx: BlockTxEnvelope,
    /// Offered transaction fee in sparks, as an exact decimal string.
    pub fee_sparks: String,
    /// Witness size counted by the mempool, in bytes.
    pub witness_bytes: u64,
    /// When this node admitted it, in Unix seconds.
    pub received_at: u64,
    /// The header's `locktime`, as signed; see [`TransactionSummary::locktime`].
    pub locktime: u32,
}

/// A bounded view of the current mempool, in admission order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransactionsResult {
    /// The first transactions in the mempool, up to the requested limit.
    pub transactions: Vec<PendingTransaction>,
    /// Total number of transactions in the mempool at the time of this response.
    pub total: u64,
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

/// The complete block header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Consensus protocol version.
    pub version: u32,
    /// Block height; genesis is zero.
    pub height: u64,
    /// Opaque authenticated Bitcoin/core-block identity supplied by the caller.
    #[serde(with = "crate::codec::hex32")]
    pub core_block_hash: [u8; 32],
    /// That core block's height. A block that moves it issues the flames
    /// its mint proof burned for.
    pub core_height: u64,
    /// The minter's clock when it built the block, in Unix seconds; several
    /// blocks may share one core block.
    pub time: u64,
    /// Previous block identifier, as committed in the header.
    pub parent: BlockId,
    /// Committed witness root, as hex.
    #[serde(with = "crate::codec::hex32")]
    pub witness_root: [u8; 32],
    /// Committed mint proof root, as hex.
    #[serde(with = "crate::codec::hex32")]
    pub mint_root: [u8; 32],
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
    /// The header's `time`, in Unix seconds. Absent for genesis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
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

/// A page of blocks, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlocksResult {
    /// Block summaries in this page.
    pub blocks: Vec<BlockSummary>,
    /// Whether older blocks remain.
    /// Use the last block's header height as `before` for the next page.
    pub has_more: bool,
}

/// Maximum entries in a `blocks`, `transactions`, or `pending_transactions` response.
pub const MAX_PAGE_SIZE: u32 = 100;

/// The most addresses one `history` call may name.
pub const MAX_HISTORY_ADDRESSES: usize = 256;

/// The most predicates one `scan` may name.
///
/// It bounds the question, not the answer: a predicate with a long history
/// still walks every contract under it. Bounding the work would take a cap on
/// hits and a cursor, which this wire format does not have.
pub const MAX_SCAN_PREDICATES: usize = 1024;

/// The most ids one `proofs` may name.
pub const MAX_PROOF_IDS: usize = 1024;

/// Maximum execution gas for an offchain actor query.
pub const MAX_QUERY_GAS: u64 = 1_000_000;

/// Maximum decoded envelope bytes for either a query argument or result.
pub const MAX_QUERY_BYTES: usize = 64 * 1024;

/// Server-defined JSON-RPC error codes, in the -32000..-32099 range.
pub mod codes {
    /// The node has no such actor, contract, block, or confirmed execution.
    pub const NOT_FOUND: i32 = -32001;
    /// The mempool refused a submitted transaction.
    pub const MEMPOOL_REJECTED: i32 = -32002;
    /// The request named more items than the method allows.
    pub const LIMIT_EXCEEDED: i32 = -32003;
    /// A blob did not decode into what it claims to be.
    pub const INVALID_BYTES: i32 = -32004;
    /// The expected query block is no longer the current tip.
    pub const STALE_BLOCK: i32 = -32005;
    /// Actor query execution failed, including gas exhaustion or a write attempt.
    pub const QUERY_FAILED: i32 = -32006;
    /// An address did not decode, or is for another network. Not `-32007`:
    /// jsonrpsee answers oversized requests with that, and reserves
    /// `-32005..=-32011` for its own server errors.
    pub const INVALID_ADDRESS: i32 = -32012;
}
