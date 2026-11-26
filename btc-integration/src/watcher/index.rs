use std::future::Future;
use std::sync::Arc;

use bitcoin::block::Header;

use crate::watcher::types::BlockId;

pub trait BlockIndex: Send + Sync {
    type Error;

    fn header(
        &self,
        id: BlockId,
    ) -> impl Future<Output = Result<Option<Header>, Self::Error>> + Send;

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
