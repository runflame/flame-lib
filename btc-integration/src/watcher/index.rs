//! The block index: headers by block id.

use std::future::Future;
use std::sync::Arc;

use bitcoin::block::Header;

use crate::watcher::types::BlockId;

/// Anything that can give the header of a block: a storage or a source.
///
/// The name is Bitcoin Core's. There the tree of block headers is the block
/// index, and `CBlockIndex::GetAncestor` is [`BlockIndex::ancestor`].
pub trait BlockIndex: Send + Sync {
    /// What a failed read returns.
    type Error;

    /// The header of a block. `None` means the block is not known here.
    fn header(
        &self,
        id: BlockId,
    ) -> impl Future<Output = Result<Option<Header>, Self::Error>> + Send;

    /// The ancestor of `block` at `height`.
    ///
    /// This is how a consumer gets to the block it wants to check, for example
    /// the anchor block of a slot.
    ///
    /// `None` means `height` is above `block`, or a header on the way is
    /// missing.
    ///
    /// The provided body walks from `block` down through `prev_blockhash`, one
    /// header at a time, and works for any implementation. An implementation
    /// that can do better overrides it: a database finds the ancestor in one
    /// query.
    fn ancestor(
        &self,
        block: BlockId,
        height: u64,
    ) -> impl Future<Output = Result<Option<BlockId>, Self::Error>> + Send {
        async move {
            let mut id = block;
            while id.height > height {
                let Some(header) = self.header(id).await? else {
                    return Ok(None);
                };
                id = id.parent(&header);
            }
            Ok((id.height == height).then_some(id))
        }
    }
}

/// A shared index is an index: whoever holds it by `Arc` hands out clones.
impl<I: BlockIndex> BlockIndex for Arc<I> {
    type Error = I::Error;

    fn header(
        &self,
        id: BlockId,
    ) -> impl Future<Output = Result<Option<Header>, Self::Error>> + Send {
        self.as_ref().header(id)
    }

    fn ancestor(
        &self,
        block: BlockId,
        height: u64,
    ) -> impl Future<Output = Result<Option<BlockId>, Self::Error>> + Send {
        self.as_ref().ancestor(block, height)
    }
}
