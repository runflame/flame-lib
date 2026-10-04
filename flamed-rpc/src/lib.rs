#![deny(missing_docs)]
//! The JSON-RPC 2.0 protocol of the `flamed` node, written once for the
//! daemon and for everything that talks to it.
//!
//! The crate names no `flamevm` or `flamechain` type and needs none: every
//! newtype here has a public inner field, so a conversion is a one-liner at
//! the call site — `BlockId(chain.tip().into_bytes())`, `TxId(txid.0)` — and
//! a wire type never has to know what it decodes into. That knowledge belongs
//! to the caller, and leaving it there is what lets an explorer, a monitor or
//! a WASM client use these methods without the VM behind them.

pub mod api;
pub mod codec;
pub mod types;

pub use api::{FlamedApiClient, FlamedApiServer};
pub use types::{
    codes, ActorCode, ActorId, ActorResult, ActorStateEnvelope, ActorTarget, BlockHeader, BlockId,
    BlockResult, BlockSummary, BlockTxEnvelope, CellId, ContractEnvelope, ContractId,
    ContractResult, DictEntry, ExecutionData, InstructionView, NoteEnvelope, PredicatePoint,
    ProofBytes, ProofResult, ScanEntry, ScanResult, SpentAt, StateCommitment, TipResult,
    TransactionResult, TransactionSummary, TransactionsResult, TxEntry, TxId, TxStatusResult,
    TxValue, ValueCell, MAX_PAGE_SIZE, MAX_PROOF_IDS, MAX_SCAN_PREDICATES,
};

/// Required on any `impl FlamedApiServer`.
pub use jsonrpsee::core::async_trait;
/// How a client call fails.
pub use jsonrpsee::core::ClientError;
/// The return type of every method: `Result<T, ErrorObjectOwned>`.
pub use jsonrpsee::core::RpcResult;
/// An HTTP client for this API, so a consumer never has to name jsonrpsee.
pub use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
/// The error a handler returns and a client reads a code out of.
pub use jsonrpsee::types::{ErrorCode, ErrorObject, ErrorObjectOwned};

#[cfg(test)]
mod tests;
