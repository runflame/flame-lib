//! The data exchanged between the source, the watcher, the storage and
//! consumers.

use std::fmt;

use bitcoin::block::Header;
use bitcoin::{BlockHash, TxOut};

/// Identifies a block: its height and hash.
///
/// Bitcoin's own types carry no height: neither [`bitcoin::Block`] nor its
/// header knows where the block stands in the chain. `BlockId` is the key under
/// which blocks are stored.
///
/// Ids are ordered by height first, so a store keyed by `BlockId` is ordered
/// along the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId {
    pub height: u64,
    pub hash: BlockHash,
}

impl BlockId {
    /// The id of the previous block, read from the header of this one.
    ///
    /// The block at height 0 has no previous block; do not call this for it.
    pub fn parent(&self, header: &Header) -> BlockId {
        BlockId {
            height: self.height - 1,
            hash: header.prev_blockhash,
        }
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.height, self.hash)
    }
}

/// The outputs the inputs of a block spend.
///
/// An input names the output it spends by transaction id and index. The
/// output itself, its script and its value, is in an earlier block. Whoever
/// has to check what an input spends needs it: an endorsement, for one, is
/// checked against the script of the output it spends. Bitcoin Core keeps
/// these outputs as the undo data of a block and returns them with
/// `getblock <hash> 3`.
///
/// There is one entry per transaction of the block, in block order, and in it
/// one per input, in input order. `None` means there is no output to give: a
/// coinbase input spends none, and the source may not have the undo data of a
/// block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prevouts(Vec<Vec<Option<TxOut>>>);

impl Prevouts {
    pub fn new(prevouts: Vec<Vec<Option<TxOut>>>) -> Self {
        Prevouts(prevouts)
    }

    /// One entry per transaction of the block, and in it one per input.
    pub fn as_slice(&self) -> &[Vec<Option<TxOut>>] {
        &self.0
    }
}

/// A change of the Bitcoin view. This is the only thing the watcher emits.
///
/// An event describes one step from the previous best block of the view to a
/// new one and names only the blocks that changed. The blocks themselves are in
/// the storage by the time the event is emitted; a consumer that needs one
/// loads it by id.
///
/// One event does not have to reach the best block of the source: a long branch
/// arrives as several events (see [`crate::watcher::BtcWatcher::max_blocks_per_event`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtcEvent {
    /// New blocks on top of the previous best block, oldest first. Never empty.
    NewBlocks { blocks: Vec<BlockId> },
    /// The chain switched to another branch. First `removed` are undone (from
    /// the best block down, never empty), then `blocks` are applied (oldest first).
    ///
    /// `fork` is the last block the old and the new branch share. `blocks` may
    /// be empty: that is a rollback to `fork` with nothing new on top.
    Reorg {
        fork: BlockId,
        removed: Vec<BlockId>,
        blocks: Vec<BlockId>,
    },
}

impl BtcEvent {
    /// The new blocks, oldest first.
    pub fn blocks(&self) -> &[BlockId] {
        match self {
            BtcEvent::NewBlocks { blocks } | BtcEvent::Reorg { blocks, .. } => blocks,
        }
    }

    /// The removed blocks, newest first. `NewBlocks` removes nothing.
    pub fn removed(&self) -> &[BlockId] {
        match self {
            BtcEvent::NewBlocks { .. } => &[],
            BtcEvent::Reorg { removed, .. } => removed,
        }
    }
}
