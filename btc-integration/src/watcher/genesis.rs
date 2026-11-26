use bitcoin::BlockHash;
use bitcoin::hashes::Hash;
use thiserror::Error;
use tracing::info;

use crate::watcher::source::{ChainSource, SourceError};
use crate::watcher::types::BlockId;

pub struct GenesisBlockWaiter {
    height: u64,
    seen: Option<BlockId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum GenesisBlockError {
    #[error("bitcoin source: {0}")]
    Source(#[from] SourceError),
    #[error("Bitcoin is at block {best}, above the genesis height {height}")]
    Missed { height: u64, best: BlockId },
}

impl GenesisBlockWaiter {
    pub fn new(height: u64) -> Self {
        GenesisBlockWaiter { height, seen: None }
    }

    pub async fn genesis_id<S: ChainSource>(
        &mut self,
        source: &S,
    ) -> Result<BlockId, GenesisBlockError> {
        loop {
            if let Some(best) = self.seen.filter(|best| best.height >= self.height) {
                let genesis_id = source.ancestor(best, self.height).await?;
                return genesis_id.ok_or_else(|| {
                    SourceError(format!(
                        "the source does not know the chain below block {best}"
                    ))
                    .into()
                });
            }
            // Before the first answer nothing is known. Any best block differs
            // from the zero hash, so the source answers at once.
            let known = self
                .seen
                .map_or_else(BlockHash::all_zeros, |best| best.hash);
            let best = source.wait_for_new_block(known).await?;
            // A wait of the source that timed out answers with the same block.
            if self.seen != Some(best) {
                info!(n = best.height, hash = %best.hash, "bitcoin new block");
            }
            if self.seen.is_none() && best.height > self.height {
                return Err(GenesisBlockError::Missed {
                    height: self.height,
                    best,
                });
            }
            self.seen = Some(best);
        }
    }
}
