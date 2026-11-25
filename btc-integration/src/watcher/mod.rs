//! Following Bitcoin: the best chain of a Bitcoin node as a stream of events.
//!
//! Data flow:
//!
//! ```text
//!  ChainSource ───blocks───▶ BtcWatcher ───BtcEvent───▶ consumer
//!  bitcoind over RPC           │    ▲                   flamed::Node, the producer, later BFT:
//!  or MemoryBtc                ▼    │                   keeps its own BtcView
//!                            BtcStorage ◀────reads───── and loads a block when it needs one
//!                            BlockId -> Block, Prevouts
//! ```
//!
//! Who knows what:
//!
//! | Part | Knows | Does not know |
//! |---|---|---|
//! | [`BlockIndex`] | the header of a block; the ancestor of a block at a height | where the headers live |
//! | [`ChainSource`] | how to fetch a block with its prevouts and how to wait for a new one | what happens to them |
//! | [`BtcStorage`] | blocks and their prevouts by [`BlockId`] | which chain is the best one |
//! | [`BtcView`] | the genesis block and the best block; how an event moves it | network, time, async |
//! | [`BtcWatcher`] | in which order to ask the source and the storage | clocks, tasks, retries, block production |
//! | [`GenesisBlockWaiter`] | how the genesis producer picks the genesis block | everything after genesis |
//! | consumer | [`BtcEvent`] and its own [`BtcView`] | that Bitcoin lives somewhere outside |
//!
//! The implementations of [`ChainSource`] and [`BtcStorage`] are in [`impls`].
//!
//! Nobody holds the chain in memory. A block points to the previous one, blocks
//! live in [`BtcStorage`], and the watcher and the consumer keep only the best
//! block.
//!
//! Blocks, headers, transactions and hashes are the types of the `bitcoin`
//! crate. Two types are added here. [`BlockId`]: Bitcoin's types carry no
//! height. [`Prevouts`]: a Bitcoin block does not carry the outputs its inputs
//! spend.
//!
//! Usage examples are the tests of this crate (`tests/watcher.rs`,
//! `tests/genesis.rs`).

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

/// The most blocks one event names. The watcher downloads them before it
/// returns the event.
pub const DEFAULT_MAX_BLOCKS_PER_EVENT: usize = 100;

/// How many blocks of one event the watcher downloads at the same time.
pub const DEFAULT_PARALLEL_DOWNLOADS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WatchError {
    /// The source is unreachable or answered inconsistently. The view is
    /// untouched: [`BtcWatcher::next`] can be called again.
    #[error("bitcoin source: {0}")]
    Source(#[from] SourceError),
    /// The storage did not answer. The view is untouched, the call can be
    /// repeated.
    #[error("block storage: {0}")]
    Storage(#[from] StorageError),
    /// The best block of the source is below the genesis block: the
    /// Bitcoin node is still syncing. Retry later.
    #[error("the best block of the source {best} is below the genesis block {genesis_id}")]
    SourceBehindGenesis { best: BlockId, genesis_id: BlockId },
    /// The chain of the source does not pass through the genesis block:
    /// a reorg replaced it, or the source is another network. The node must
    /// stop.
    #[error("the chain of the source has {found} in place of the genesis block {genesis_id}")]
    GenesisBlockReplaced {
        genesis_id: BlockId,
        /// The block of the source at the height of the genesis block.
        found: BlockId,
    },
    /// Neither the storage nor the source knows a block of the view, so the
    /// blocks to remove on the way to the chain of the source cannot be
    /// found. Bitcoin Core keeps the header of every block it has seen: the
    /// view comes from another Bitcoin node. The consumer has to start again
    /// from a view the source knows.
    #[error("neither the storage nor the source knows the block {block} of the view")]
    ViewBlockUnknown { block: BlockId },
}

impl WatchError {
    /// Retrying after a fatal error is pointless: the node stops.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            WatchError::GenesisBlockReplaced { .. } | WatchError::ViewBlockUnknown { .. }
        )
    }
}

/// The path from the best block of the view to the best block of the source:
/// found, but not yet handed to the consumer in full.
///
/// This is a queue, not history: it drains as events go out. It is long only
/// while the node catches up with the source.
struct Branch {
    /// The last block the view and the source share. The first event needs it
    /// if that event removes blocks.
    fork: BlockId,
    /// Blocks of the view above `fork`, newest first. The first event
    /// removes them.
    removed: Vec<BlockId>,
    /// Blocks of the source above `fork`, oldest first.
    blocks: VecDeque<BlockId>,
}

/// Follows the best block of Bitcoin.
///
/// The watcher has no task, no clock and no timers of its own. It gives out
/// events on request: [`BtcWatcher::next`]. Waiting, retrying after an error
/// and stopping belong to the shell that holds it.
///
/// The watcher does not hold the chain in memory: it has a view (the genesis
/// block and the best block), and the blocks are in the storage `B`.
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
    /// A watcher that starts from the genesis block: the first event reports
    /// the chain above it.
    ///
    /// The storage comes from the caller, who keeps a handle of its own to
    /// read the blocks the events name.
    pub fn new(source: S, storage: B, genesis_id: BlockId) -> Self {
        Self::resume(source, storage, BtcView::new(genesis_id))
    }

    /// A watcher that starts from the best block of the source: its view is
    /// already there, and the first event reports what changes after this
    /// call.
    ///
    /// This is for a consumer that needs the height of Bitcoin and not the
    /// blocks below it, like block production. Nothing between the genesis
    /// block and the best block is requested. The consumer starts with
    /// the same view: [`BtcWatcher::view`].
    ///
    /// The source is asked twice: for its best block, and for the block of its
    /// best chain at the height of the genesis block, which has to be
    /// the genesis block. The errors are those of [`BtcWatcher::next`].
    pub async fn from_best(source: S, storage: B, genesis_id: BlockId) -> Result<Self, WatchError> {
        // Any best block differs from the zero hash, so the source answers
        // at once.
        let best = source.wait_for_new_block(BlockHash::all_zeros()).await?;
        if best.height < genesis_id.height {
            return Err(WatchError::SourceBehindGenesis { best, genesis_id });
        }
        // The two answers are not one snapshot of the source. A best chain
        // that changed between them shows up in the next event.
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

    /// Continues from `view`: the first event is the difference from it.
    ///
    /// This is how a watcher is brought back after a restart. The consumer
    /// saves the view, which is two block ids. If the storage survived the
    /// restart, the blocks in it are not requested from the source again; if
    /// it did not, the watcher takes them from the source.
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

    /// Limits the number of blocks in one event.
    pub fn max_blocks_per_event(mut self, blocks: usize) -> Self {
        assert!(blocks > 0, "an event has to name at least one block");
        self.max_blocks_per_event = blocks;
        self
    }

    /// Limits the number of blocks downloaded at the same time. With 1 the
    /// watcher asks the source for one block after another.
    pub fn parallel_downloads(mut self, blocks: usize) -> Self {
        assert!(blocks > 0, "a download needs at least one request");
        self.parallel_downloads = blocks;
        self
    }

    /// The Bitcoin view as the consumer has received it so far.
    pub fn view(&self) -> &BtcView {
        &self.view
    }

    /// Moves the view to `block`: the next event is the difference from it.
    ///
    /// This is for a consumer that could not apply the events it was given.
    /// It goes back to the last block it has applied and receives the events
    /// from there again. `block` does not have to be on the chain of the
    /// view: from a block that a reorg removed, the next event is that reorg.
    ///
    /// Nothing is asked here. The next call takes the blocks the storage
    /// still holds from it and the rest from the source. A block that neither
    /// of them knows is [`WatchError::ViewBlockUnknown`] in that call.
    ///
    /// # Panics
    ///
    /// If `block` is neither the genesis block nor above it.
    pub fn rewind_to(&mut self, block: BlockId) {
        self.view = BtcView::restore(self.view.genesis_id(), block);
        // The path was found from the best block the view had.
        self.pending = None;
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn storage(&self) -> &B {
        &self.storage
    }

    /// The next change of the view.
    ///
    /// - A call reports the path from the best block of the view to the best
    ///   block of the source. For a watcher made with [`BtcWatcher::new`] the
    ///   first one is the chain above the genesis block. While nothing
    ///   changes, the call waits.
    /// - An event names at most [`BtcWatcher::max_blocks_per_event`] blocks,
    ///   and no more than the storage keeps ([`BtcStorage::capacity`]): a long
    ///   branch takes several calls.
    /// - The blocks of an event are in the storage by the time it is returned.
    ///   The blocks an event removed leave the storage on the next call.
    /// - To cancel, drop the future. The view changes in one step at the very
    ///   end, so neither cancellation nor an error corrupts it, and the call
    ///   can be repeated.
    /// - If the best block of the source left and came back, there is no event.
    pub async fn next(&mut self) -> Result<BtcEvent, WatchError> {
        loop {
            // 1. Drop the blocks the previous event removed. Not earlier:
            //    until the view moved they were its chain, and after it moved
            //    there are no awaits left in that call. The list is cleared
            //    only when they are gone, so a failed or dropped call loses
            //    none: the next call removes them again.
            self.storage.remove_many(&self.stale).await?;
            self.stale.clear();

            // 2. No path in progress: wait until the best block of the source
            //    differs from our best block. The call waits inside the
            //    source; it comes back with the same block only when the wait
            //    of the source timed out, and then the watcher waits again.
            //    This is not a busy loop: the pace is the timeout of the
            //    source.
            if self.pending.is_none() {
                let best = self
                    .source
                    .wait_for_new_block(self.view.best().hash)
                    .await?;
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

            // 3. Blocks reach the storage before the consumer hears of them.
            //    A failed call keeps what it has stored, and the next one
            //    requests only the rest.
            if let Some(gone) = self.download_all(&blocks).await? {
                // The block left the best chain while the path was waiting,
                // and the source dropped it. Forget the path: the next call
                // finds a new one from the best block of the source. No event
                // named the blocks stored for this path, so they go out too.
                self.pending = None;
                self.stale.extend(&blocks);
                return Err(SourceError(format!("the source no longer has block {gone}")).into());
            }

            // 4. The best chain of the source may have moved since the path
            //    was found. If the block the view is about to move to is no
            //    longer on it, nothing is reported: forget the path, and the
            //    blocks stored for it go out.
            let last = blocks.last().copied().unwrap_or(branch.fork);
            if self.source.block_at(last.height).await? != Some(last) {
                self.pending = None;
                self.stale.extend(&blocks);
                continue;
            }

            // 5. No awaits below: the event, the view and the rest of the path
            //    change together.
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

    /// Finds the path from the best block of the view to the best block of the
    /// source.
    ///
    /// Walks down from both blocks through `prev_blockhash`, each time from the
    /// higher one, until they meet. The block where they meet is the common
    /// ancestor. This needs no chain in memory and reads as many headers as
    /// there are blocks that changed.
    ///
    /// Does not change the view: everything found stays in local variables.
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
            // The chain of the source reached the height of the genesis
            // block, and the block there is another one.
            if theirs.height == genesis_id.height && theirs != genesis_id {
                return Err(WatchError::GenesisBlockReplaced {
                    genesis_id,
                    found: theirs,
                });
            }
            if theirs.height >= ours.height {
                // A block of the source that the view does not have.
                let header = self.header(theirs).await?.ok_or_else(|| {
                    SourceError(format!("the source does not know block {theirs}"))
                })?;
                blocks.push_front(theirs);
                theirs = theirs.parent(&header);
            } else {
                // A block of the view that is not on the chain of the source.
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

    /// The header of a block: from the storage, or from the source if the
    /// block is not stored. `None` means neither knows the block.
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

    /// Makes sure every block of an event is in the storage. Returns a block
    /// the source does not have, if there is one.
    ///
    /// The path is already known, block by block, so it does not matter in
    /// which order the blocks arrive: several are requested at once.
    async fn download_all(&self, blocks: &[BlockId]) -> Result<Option<BlockId>, WatchError> {
        'download: loop {
            let stored: Vec<bool> = stream::iter(blocks.iter().copied())
                .map(|id| self.download(id))
                .buffered(self.parallel_downloads)
                .try_collect()
                .await?;
            if let Some(gone) = stored.iter().position(|stored| !stored) {
                return Ok(Some(blocks[gone]));
            }
            if self.storage.capacity().is_none() {
                return Ok(None);
            }
            // A bounded storage makes room for a new block by dropping the
            // block it has held the longest. After `rewind_to`, or with a
            // storage that survived a restart, that can be a block of this
            // very event: it is requested again. A block stored in this call
            // is not dropped by it: an event names no more blocks than the
            // storage keeps. So every round leaves fewer blocks to request.
            for &id in blocks {
                if self.storage.header(id).await?.is_none() {
                    continue 'download;
                }
            }
            return Ok(None);
        }
    }

    /// Makes sure the block is in the storage, fetching it from the source if
    /// it is not. `false` means the source does not have the block.
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

/// Whether `block` has an entry in `prevouts` for every transaction, and in
/// it one for every input.
fn has_prevouts_for_every_input(block: &Block, prevouts: &Prevouts) -> bool {
    let prevouts = prevouts.as_slice();
    block.txdata.len() == prevouts.len()
        && block
            .txdata
            .iter()
            .zip(prevouts)
            .all(|(transaction, spent)| transaction.input.len() == spent.len())
}
