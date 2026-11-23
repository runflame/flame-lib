//! Choosing the genesis block of a new network.

use bitcoin::BlockHash;
use bitcoin::hashes::Hash;
use thiserror::Error;
use tracing::info;

use crate::watcher::source::{ChainSource, SourceError};
use crate::watcher::types::BlockId;

/// Waits for Bitcoin to reach the height at which a new network starts, and
/// gives the genesis block: the block of the best chain at that height.
///
/// Only the node that produces genesis needs this. It is started ahead of time,
/// waits for the Bitcoin block, and builds the genesis core block on it. Every
/// other node knows the genesis block from the Flame genesis and attaches to
/// Bitcoin with [`crate::watcher::BtcWatcher`] right away.
///
/// The waiter logs every best block of Bitcoin it sees on the way.
///
/// The wait has to begin before Bitcoin passes the height. If the first answer
/// of the source is already above it, the wait fails with
/// [`GenesisBlockError::Missed`].
pub struct GenesisBlockWaiter {
    height: u64,
    /// The best block the source reported last. `None` until its first answer.
    seen: Option<BlockId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum GenesisBlockError {
    /// The source is unreachable or answered inconsistently. Call again to go
    /// on waiting.
    #[error("bitcoin source: {0}")]
    Source(#[from] SourceError),
    /// Bitcoin was already above the genesis height when the wait began.
    #[error("Bitcoin is at block {best}, above the genesis height {height}")]
    Missed { height: u64, best: BlockId },
}

impl GenesisBlockWaiter {
    pub fn new(height: u64) -> Self {
        GenesisBlockWaiter { height, seen: None }
    }

    /// The genesis block, once Bitcoin has it.
    ///
    /// After [`GenesisBlockError::Source`] the same wait continues with the next
    /// call: what the source reported before the error is not forgotten.
    pub async fn genesis_id<S: ChainSource>(
        &mut self,
        source: &S,
    ) -> Result<BlockId, GenesisBlockError> {
        loop {
            if let Some(best) = self.seen.filter(|best| best.height >= self.height) {
                // Several blocks may have arrived at once: the genesis
                // block is the ancestor of the best one at the genesis
                // height.
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
