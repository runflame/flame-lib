//! The seven methods `flamed` serves.
//!
//! The macro generates `FlamedApiServer`, which `flamed` implements, and
//! `FlamedApiClient`, blanket-implemented for every jsonrpsee client, so the
//! wire is written once and a wallet, an explorer or a monitor speaks it
//! without repeating a single method name.
//!
//! Nothing else belongs in this module: attributes on the trait are discarded
//! with it, so a lint allow would have to be module-level, and an item named
//! `core` or `jsonrpsee` here would shadow the paths the macro emits.

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;

use crate::types::{
    BlockTxEnvelope, ContractId, ContractResult, PredicatePoint, ProofResult, ScanResult,
    TipResult, TxId, TxStatusResult,
};

/// The `flamed` JSON-RPC 2.0 API. Parameters are positional, in the order
/// written; method names carry no prefix.
#[rpc(server, client)]
pub trait FlamedApi {
    /// The current tip: its hash, its height, and the contract accumulator root.
    #[method(name = "tip")]
    async fn tip(&self) -> RpcResult<TipResult>;

    /// One contract's membership proof, or what became of it.
    #[method(name = "proof")]
    async fn proof(&self, id: ContractId) -> RpcResult<ProofResult>;

    /// The same question for up to `MAX_PROOF_IDS` contracts at once.
    #[method(name = "proofs")]
    async fn proofs(&self, ids: Vec<ContractId>) -> RpcResult<Vec<(ContractId, ProofResult)>>;

    /// One contract as the node archived it. `NOT_FOUND` if it has none.
    #[method(name = "contract")]
    async fn contract(&self, id: ContractId) -> RpcResult<ContractResult>;

    /// Offers a transaction to the mempool. `INVALID_BYTES` if it does not
    /// decode, `MEMPOOL_REJECTED` if the pool refuses it.
    #[method(name = "submit_tx")]
    async fn submit_tx(&self, block_tx: BlockTxEnvelope) -> RpcResult<TxId>;

    /// Whether a transaction is unknown, waiting, or confirmed.
    #[method(name = "tx_status")]
    async fn tx_status(&self, txid: TxId) -> RpcResult<TxStatusResult>;

    /// Every contract created at `since_height` or later under one of these
    /// predicates. `LIMIT_EXCEEDED` past `MAX_SCAN_PREDICATES`.
    #[method(name = "scan")]
    async fn scan(
        &self,
        predicates: Vec<PredicatePoint>,
        since_height: u64,
    ) -> RpcResult<ScanResult>;
}
