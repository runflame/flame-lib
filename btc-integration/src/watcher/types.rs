use std::fmt;

use bitcoin::block::Header;
use bitcoin::{BlockHash, TxOut};

/// Ids are ordered by height first, so a store keyed by `BlockId` is ordered
/// along the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId {
    pub height: u64,
    pub hash: BlockHash,
}

impl BlockId {
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

    pub fn as_slice(&self) -> &[Vec<Option<TxOut>>] {
        &self.0
    }
}

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
    pub fn blocks(&self) -> &[BlockId] {
        match self {
            BtcEvent::NewBlocks { blocks } | BtcEvent::Reorg { blocks, .. } => blocks,
        }
    }

    pub fn removed(&self) -> &[BlockId] {
        match self {
            BtcEvent::NewBlocks { .. } => &[],
            BtcEvent::Reorg { removed, .. } => removed,
        }
    }
}
