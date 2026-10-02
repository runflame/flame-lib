//! The JSON-RPC 2.0 server.
//!
//! Two rules shape this module. The node lives behind a
//! `std::sync::Mutex`, and every call into it — the cheap ones too, for
//! uniformity — runs inside `spawn_blocking`, so no runtime thread ever
//! waits on that lock or on proof verification. And a `NodeError` maps to
//! exactly one error code, so a caller can branch on the code and read the
//! text for a human.

use std::net::SocketAddr;
use std::sync::Arc;

use flamed_rpc::{
    async_trait, codes, BlockId, BlockResult, BlockTxEnvelope, ContractEnvelope, ContractId,
    ContractResult, ErrorCode, ErrorObjectOwned, FlamedApiServer, PredicatePoint, ProofBytes,
    ProofResult, RpcResult, ScanResult, TipResult, TxId, TxStatusResult, MAX_PROOF_IDS,
    MAX_SCAN_PREDICATES,
};
use jsonrpsee::server::{BatchRequestConfig, Server, ServerConfig, ServerHandle};

use crate::cells::proof_bytes;
use crate::node::{Node, NodeError, ProofStatus, SharedNode, TxStatus};

/// The largest request this node will read. A full 1024-predicate `scan`
/// is around 70 KiB of hex, so this leaves room for a handful of them and
/// not for a thousand.
const MAX_REQUEST_BYTES: u32 = 1024 * 1024;

/// The largest reply it will send. A `scan` over a busy predicate is the
/// only thing that approaches it.
const MAX_RESPONSE_BYTES: u32 = 64 * 1024 * 1024;

/// The most calls one batch may carry.
const MAX_BATCH_CALLS: u32 = 16;

/// Serves the `flamed-rpc` API over one shared node.
///
/// A local type because it has to be: `FlamedApiServer` is foreign and `Arc`
/// is not `#[fundamental]`, so `impl FlamedApiServer for Arc<Mutex<Node>>`
/// is an orphan-rule error.
#[derive(Clone)]
pub struct FlamedRpc {
    node: SharedNode,
}

impl FlamedRpc {
    /// Wraps a shared node.
    pub fn new(node: SharedNode) -> Self {
        Self { node }
    }

    /// Runs one piece of work with the node locked, off the runtime.
    async fn with_node<T, F>(&self, work: F) -> Result<T, ErrorObjectOwned>
    where
        F: FnOnce(&mut Node) -> Result<T, NodeError> + Send + 'static,
        T: Send + 'static,
    {
        let node = Arc::clone(&self.node);
        let joined = tokio::task::spawn_blocking(move || {
            // A poisoned lock means a handler panicked part-way through a
            // mutation and the indexes may disagree with the chain. Refuse
            // to answer from that state rather than paper over it.
            let mut node = node.lock().map_err(|_| {
                internal("the node state is poisoned: an earlier operation panicked")
            })?;
            work(&mut node).map_err(ErrorObjectOwned::from)
        })
        .await;

        match joined {
            Ok(result) => result,
            Err(error) => Err(internal(format!("the node task failed: {error}"))),
        }
    }
}

#[async_trait]
impl FlamedApiServer for FlamedRpc {
    async fn block(&self, height: u64) -> RpcResult<BlockResult> {
        self.with_node(move |node| node.block(height)).await
    }

    async fn tip(&self) -> RpcResult<TipResult> {
        self.with_node(|node| {
            let tip = node.tip();
            Ok(TipResult {
                hash: BlockId(tip.hash.into_bytes()),
                height: tip.height,
                contract_root: tip.contract_root,
            })
        })
        .await
    }

    async fn proof(&self, id: ContractId) -> RpcResult<ProofResult> {
        self.with_node(move |node| Ok(proof_result(node.proof(&id.0))))
            .await
    }

    async fn proofs(&self, ids: Vec<ContractId>) -> RpcResult<Vec<(ContractId, ProofResult)>> {
        // Refused before a blocking thread is spent on it, and before the
        // lock is taken: the point of the bound is that no one request holds
        // the node for an unbounded time.
        check_limit(MAX_PROOF_IDS, ids.len())?;
        self.with_node(move |node| {
            let raw: Vec<_> = ids.iter().map(|id| id.0).collect();
            Ok(node
                .proofs(&raw)
                .into_iter()
                .map(|(id, status)| (ContractId(id), proof_result(status)))
                .collect())
        })
        .await
    }

    async fn contract(&self, id: ContractId) -> RpcResult<ContractResult> {
        self.with_node(move |node| {
            let record = node
                .contract(&id.0)
                .ok_or_else(|| NodeError::UnknownContract(id.to_string()))?;
            Ok(ContractResult {
                height: record.height,
                txid: TxId(record.txid.0),
                predicate: PredicatePoint(record.predicate),
                bytes: ContractEnvelope(record.contract.clone()),
            })
        })
        .await
    }

    async fn submit_tx(&self, block_tx: BlockTxEnvelope) -> RpcResult<TxId> {
        self.with_node(move |node| Ok(TxId(node.submit(&block_tx.0)?.0)))
            .await
    }

    async fn tx_status(&self, txid: TxId) -> RpcResult<TxStatusResult> {
        self.with_node(move |node| {
            Ok(match node.tx_status(&flamevm::TxID(txid.0)) {
                TxStatus::Unknown => TxStatusResult::Unknown,
                TxStatus::Mempool => TxStatusResult::Mempool,
                TxStatus::Confirmed { height, block } => TxStatusResult::Confirmed {
                    height,
                    block: BlockId(block.into_bytes()),
                },
            })
        })
        .await
    }

    async fn scan(
        &self,
        predicates: Vec<PredicatePoint>,
        since_height: u64,
    ) -> RpcResult<ScanResult> {
        check_limit(MAX_SCAN_PREDICATES, predicates.len())?;
        self.with_node(move |node| {
            let raw: Vec<[u8; 32]> = predicates.iter().map(|point| point.0).collect();
            Ok(ScanResult {
                tip_height: node.tip().height,
                outputs: node.scan(&raw, since_height),
            })
        })
        .await
    }
}

/// Binds an HTTP server, starts it, and says where it listens.
///
/// The `ServerHandle` has to be kept: dropping the last clone stops the
/// server. Awaiting `stopped()` on it is what keeps `flamed run` running.
pub async fn serve(
    node: SharedNode,
    bind: SocketAddr,
) -> std::io::Result<(SocketAddr, ServerHandle)> {
    // `MAX_SCAN_PREDICATES` and `MAX_PROOF_IDS` bound one call, and
    // jsonrpsee would otherwise accept an unlimited batch inside a 10 MiB
    // body — roughly a hundred full-sized scans in one request, each
    // taking the node lock. The bound has to be on the request too.
    let config = ServerConfig::builder()
        .max_request_body_size(MAX_REQUEST_BYTES)
        .max_response_body_size(MAX_RESPONSE_BYTES)
        .set_batch_request_config(BatchRequestConfig::Limit(MAX_BATCH_CALLS))
        .build();
    let server = Server::builder().set_config(config).build(bind).await?;
    // Resolves a port of 0 to the one the OS gave, which is how a test
    // avoids racing for a fixed port.
    let addr = server.local_addr()?;
    let handle = server.start(FlamedRpc::new(node).into_rpc());
    Ok((addr, handle))
}

/// One `NodeError`, one code. The last arm is the point of the split: those
/// mean the node is broken, not that the request was.
impl From<NodeError> for ErrorObjectOwned {
    fn from(error: NodeError) -> Self {
        let code = match &error {
            NodeError::UnknownContract(_) | NodeError::NotFound(_) => codes::NOT_FOUND,
            NodeError::Mempool(_) => codes::MEMPOOL_REJECTED,
            NodeError::Limit { .. } => codes::LIMIT_EXCEEDED,
            NodeError::Decode(_) => codes::INVALID_BYTES,
            _ => ErrorCode::InternalError.code(),
        };
        ErrorObjectOwned::owned(code, error.to_string(), None::<()>)
    }
}

fn check_limit(max: usize, got: usize) -> Result<(), ErrorObjectOwned> {
    if got > max {
        return Err(NodeError::Limit { max, got }.into());
    }
    Ok(())
}

fn internal(message: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(ErrorCode::InternalError.code(), message.into(), None::<()>)
}

fn proof_result(status: ProofStatus) -> ProofResult {
    match status {
        ProofStatus::Unspent(proof) => ProofResult::Unspent {
            proof: ProofBytes(proof_bytes(&proof)),
        },
        ProofStatus::Spent { height, txid } => ProofResult::Spent {
            height,
            txid: TxId(txid.0),
        },
        ProofStatus::Unknown => ProofResult::Unknown,
    }
}
