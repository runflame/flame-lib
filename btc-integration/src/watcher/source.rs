//! The port to Bitcoin.

use std::future::Future;
use std::sync::Arc;

use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash};
use thiserror::Error;

use crate::watcher::index::BlockIndex;
use crate::watcher::types::{BlockId, Prevouts};

/// Everything the watcher asks a Bitcoin node.
///
/// Implementations:
/// - `watcher::impls::MemorySource`: Bitcoin in memory, for tests and for a
///   devnet without bitcoind (the `memory-btc` feature);
/// - [`crate::watcher::impls::RpcSource`]: RPC to bitcoind
///   (`getblockheader`, `getblock <hash> 3`, `getblockhash`,
///   `waitfornewblock`).
///
/// The methods return `impl Future + Send`, like the traits in flame-minting.
/// Such a future runs under any executor: tokio, commonware, or a single poll
/// in a test.
///
/// The watcher has no clock, so it cannot give up on a call. A source over a
/// network sets its own timeout and answers with an error when it expires.
pub trait ChainSource: BlockIndex<Error = SourceError> {
    /// The whole block and the outputs its inputs spend. `None` means the
    /// source does not have the block.
    fn block(
        &self,
        hash: BlockHash,
    ) -> impl Future<Output = Result<Option<(Block, Prevouts)>, SourceError>> + Send;

    /// Waits until the best block of the source is not `known`, and returns it.
    ///
    /// The watcher relies on two properties:
    /// - if the best block already differs, the answer comes at once. So a
    ///   change cannot be missed, even if the previous call was dropped halfway;
    /// - an implementation may return `known` again when its own wait timed out,
    ///   and only then. The watcher asks again at once, so a source that
    ///   answers `known` without waiting makes it spin.
    fn wait_for_new_block(
        &self,
        known: BlockHash,
    ) -> impl Future<Output = Result<BlockId, SourceError>> + Send;

    /// The block of the best chain at `height`. `None` means the best chain
    /// is shorter.
    ///
    /// The provided body asks for the best block and walks down from it, one
    /// header at a time. A source that can do better overrides it: Bitcoin
    /// Core answers in one request.
    fn block_at(
        &self,
        height: u64,
    ) -> impl Future<Output = Result<Option<BlockId>, SourceError>> + Send {
        async move {
            // Any best block differs from the zero hash, so the source
            // answers at once.
            let best = self.wait_for_new_block(BlockHash::all_zeros()).await?;
            self.ancestor(best, height).await
        }
    }
}

/// A shared source is a source: a watcher takes its source by value, and the
/// owner of the source keeps it for the next watcher.
impl<S: ChainSource> ChainSource for Arc<S> {
    fn block(
        &self,
        hash: BlockHash,
    ) -> impl Future<Output = Result<Option<(Block, Prevouts)>, SourceError>> + Send {
        self.as_ref().block(hash)
    }

    fn wait_for_new_block(
        &self,
        known: BlockHash,
    ) -> impl Future<Output = Result<BlockId, SourceError>> + Send {
        self.as_ref().wait_for_new_block(known)
    }

    fn block_at(
        &self,
        height: u64,
    ) -> impl Future<Output = Result<Option<BlockId>, SourceError>> + Send {
        self.as_ref().block_at(height)
    }
}

/// The source did not answer or answered inconsistently. A retry may help.
///
/// "No such block" is not an error: it is `Ok(None)`.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct SourceError(pub String);
