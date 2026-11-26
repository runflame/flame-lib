use std::collections::VecDeque;

use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash};
use futures::{StreamExt, TryStreamExt, stream};
use thiserror::Error;

mod genesis;
pub mod impls;
mod index;
mod source;
mod storage;
mod types;
mod view;

pub use genesis::{GenesisBlockError, GenesisBlockWaiter};
pub use index::BlockIndex;
pub use source::{ChainSource, SourceError};
pub use storage::{BtcStorage, StorageError};
pub use types::{BlockId, BtcEvent, Prevouts};
pub use view::{BtcView, OutOfSync};

pub const DEFAULT_MAX_BLOCKS_PER_EVENT: usize = 100;

pub const DEFAULT_PARALLEL_DOWNLOADS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WatchError {
    #[error("bitcoin source: {0}")]
    Source(#[from] SourceError),
    #[error("block storage: {0}")]
    Storage(#[from] StorageError),
    #[error("the best block of the source {best} is below the genesis block {genesis_id}")]
    SourceBehindGenesis { best: BlockId, genesis_id: BlockId },
    #[error("the chain of the source has {found} in place of the genesis block {genesis_id}")]
    GenesisBlockReplaced { genesis_id: BlockId, found: BlockId },
    #[error("neither the storage nor the source knows the block {block} of the view")]
    ViewBlockUnknown { block: BlockId },
}

impl WatchError {
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            WatchError::GenesisBlockReplaced { .. } | WatchError::ViewBlockUnknown { .. }
        )
    }
}

struct Branch {
    fork: BlockId,
    /// Blocks of the view above `fork`, newest first. The first event
    /// removes them.
    removed: Vec<BlockId>,
    /// Blocks of the source above `fork`, oldest first.
    blocks: VecDeque<BlockId>,
}

pub struct BtcWatcher<S, B> {
    source: S,
    storage: B,
    view: BtcView,
    pending: Option<Branch>,
    /// Blocks still in the storage that have to leave it: removed by an event
    /// that has already gone out, or stored for a path that was dropped.
    stale: Vec<BlockId>,
    max_blocks_per_event: usize,
    parallel_downloads: usize,
}

impl<S: ChainSource, B: BtcStorage> BtcWatcher<S, B> {
    pub fn new(source: S, storage: B, genesis_id: BlockId) -> Self {
        Self::resume(source, storage, BtcView::new(genesis_id))
    }

    pub async fn from_best(source: S, storage: B, genesis_id: BlockId) -> Result<Self, WatchError> {
        // Any best block differs from the zero hash, so the source answers
        // at once.
        let best = source.wait_for_new_block(BlockHash::all_zeros()).await?;
        if best.height < genesis_id.height {
            return Err(WatchError::SourceBehindGenesis { best, genesis_id });
        }
        let at_genesis = if best.height == genesis_id.height {
            Some(best)
        } else {
            source.block_at(genesis_id.height).await?
        };
        match at_genesis {
            Some(found) if found == genesis_id => Ok(Self::resume(
                source,
                storage,
                BtcView::restore(genesis_id, best),
            )),
            Some(found) => Err(WatchError::GenesisBlockReplaced { genesis_id, found }),
            None => Err(SourceError(format!(
                "the best chain of the source no longer reaches the genesis block {genesis_id}"
            ))
            .into()),
        }
    }

    pub fn resume(source: S, storage: B, view: BtcView) -> Self {
        BtcWatcher {
            source,
            storage,
            view,
            pending: None,
            stale: Vec::new(),
            max_blocks_per_event: DEFAULT_MAX_BLOCKS_PER_EVENT,
            parallel_downloads: DEFAULT_PARALLEL_DOWNLOADS,
        }
    }

    pub fn max_blocks_per_event(mut self, blocks: usize) -> Self {
        assert!(blocks > 0, "an event has to name at least one block");
        self.max_blocks_per_event = blocks;
        self
    }

    pub fn parallel_downloads(mut self, blocks: usize) -> Self {
        assert!(blocks > 0, "a download needs at least one request");
        self.parallel_downloads = blocks;
        self
    }

    pub fn view(&self) -> &BtcView {
        &self.view
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn storage(&self) -> &B {
        &self.storage
    }

    /// To cancel, drop the future. The view changes in one step at the very
    /// end, so neither cancellation nor an error corrupts it, and the call
    /// can be repeated.
    pub async fn next(&mut self) -> Result<BtcEvent, WatchError> {
        loop {
            // The blocks the previous event removed leave the storage now, not
            // earlier: until the view moved they were its chain. The list is
            // cleared only when they are gone, so a failed or dropped call
            // loses none.
            self.storage.remove_many(&self.stale).await?;
            self.stale.clear();

            if self.pending.is_none() {
                let best = self
                    .source
                    .wait_for_new_block(self.view.best().hash)
                    .await?;
                // The wait of the source timed out.
                if best.hash == self.view.best().hash {
                    continue;
                }
                self.pending = Some(self.find_branch(best).await?);
            }
            let branch = self.pending.as_ref().expect("a path is pending");
            // An event names no more blocks than the storage keeps: the
            // consumer has to find every one of them there.
            let most = self
                .max_blocks_per_event
                .min(self.storage.capacity().unwrap_or(usize::MAX));
            let blocks: Vec<BlockId> = branch.blocks.iter().take(most).copied().collect();

            // Blocks reach the storage before the consumer hears of them.
            let stored: Vec<bool> = stream::iter(blocks.clone())
                .map(|id| self.download(id))
                .buffered(self.parallel_downloads)
                .try_collect()
                .await?;
            if let Some(gone) = stored.iter().position(|stored| !stored) {
                // No event named the blocks stored for this path, so they go
                // out too.
                self.pending = None;
                self.stale.extend(&blocks);
                return Err(SourceError(format!(
                    "the source no longer has block {}",
                    blocks[gone]
                ))
                .into());
            }

            // The best chain of the source may have moved since the path was
            // found. If the block the view is about to move to is no longer
            // on it, nothing is reported.
            let last = blocks.last().copied().unwrap_or(branch.fork);
            if self.source.block_at(last.height).await? != Some(last) {
                self.pending = None;
                self.stale.extend(&blocks);
                continue;
            }

            // No awaits below: the event, the view and the rest of the path
            // change together.
            let branch = self.pending.as_mut().expect("a path is pending");
            let event = if branch.removed.is_empty() {
                BtcEvent::NewBlocks { blocks }
            } else {
                BtcEvent::Reorg {
                    fork: branch.fork,
                    removed: std::mem::take(&mut branch.removed),
                    blocks,
                }
            };
            self.view
                .apply(&event)
                .expect("a path found from this view starts at its best block");
            branch.blocks.drain(..event.blocks().len());
            if branch.blocks.is_empty() {
                self.pending = None;
            }
            self.stale.extend(event.removed());
            return Ok(event);
        }
    }

    async fn find_branch(&self, best: BlockId) -> Result<Branch, WatchError> {
        let genesis_id = self.view.genesis_id();
        if best.height < genesis_id.height {
            return Err(WatchError::SourceBehindGenesis { best, genesis_id });
        }
        let mut ours = self.view.best();
        let mut theirs = best;
        let mut removed = Vec::new();
        let mut blocks = VecDeque::new();
        while ours != theirs {
            if theirs.height == genesis_id.height && theirs != genesis_id {
                return Err(WatchError::GenesisBlockReplaced {
                    genesis_id,
                    found: theirs,
                });
            }
            if theirs.height >= ours.height {
                let header = self.header(theirs).await?.ok_or_else(|| {
                    SourceError(format!("the source does not know block {theirs}"))
                })?;
                blocks.push_front(theirs);
                theirs = theirs.parent(&header);
            } else {
                let header = self
                    .header(ours)
                    .await?
                    .ok_or(WatchError::ViewBlockUnknown { block: ours })?;
                removed.push(ours);
                ours = ours.parent(&header);
            }
        }
        Ok(Branch {
            fork: ours,
            removed,
            blocks,
        })
    }

    async fn header(&self, id: BlockId) -> Result<Option<Header>, WatchError> {
        if let Some(header) = self.storage.header(id).await? {
            return Ok(Some(header));
        }
        let Some(header) = self.source.header(id).await? else {
            return Ok(None);
        };
        if header.block_hash() != id.hash {
            return Err(SourceError(format!(
                "the source answered with block {} instead of {id}",
                header.block_hash()
            ))
            .into());
        }
        Ok(Some(header))
    }

    async fn download(&self, id: BlockId) -> Result<bool, WatchError> {
        if self.storage.header(id).await?.is_some() {
            return Ok(true);
        }
        let Some((block, prevouts)) = self.source.block(id.hash).await? else {
            return Ok(false);
        };
        if block.block_hash() != id.hash {
            return Err(SourceError(format!(
                "the source answered with block {} instead of {id}",
                block.block_hash()
            ))
            .into());
        }
        if !has_prevouts_for_every_input(&block, &prevouts) {
            return Err(SourceError(format!(
                "the prevouts of block {id} do not match its transactions"
            ))
            .into());
        }
        self.storage.put(id, block, prevouts).await?;
        Ok(true)
    }
}

fn has_prevouts_for_every_input(block: &Block, prevouts: &Prevouts) -> bool {
    let prevouts = prevouts.as_slice();
    block.txdata.len() == prevouts.len()
        && block
            .txdata
            .iter()
            .zip(prevouts)
            .all(|(transaction, spent)| transaction.input.len() == spent.len())
}
