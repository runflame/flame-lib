use bitcoin::Block;
use thiserror::Error;

use crate::protocol::{
    AuthenticatedMintingEndorsement, Burn, MintingEndorsementProcessingError,
    validate_transaction_endorsements,
};
use crate::watcher::{BlockId, BtcStorage, Prevouts, StorageError};

/// A Bitcoin block as Flame sees it: which block it is, and its burns and
/// endorsements in block order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcBlockRecords {
    pub id: BlockId,
    pub burns: Vec<Burn>,
    pub endorsements: Vec<AuthenticatedMintingEndorsement>,
}

impl BtcBlockRecords {
    /// Reads the burns and the endorsements of `block`, which is the block
    /// `id`.
    ///
    /// `prevouts` are the outputs the inputs of the block spend. A burn does
    /// not need them. An endorsement counts only if its input spends an output
    /// of the minter it names, so an input that carries an endorsement and
    /// has no prevout is an error, not a skipped endorsement.
    pub fn from_block(
        id: BlockId,
        block: &Block,
        prevouts: &Prevouts,
    ) -> Result<Self, MintingEndorsementProcessingError> {
        let prevouts = prevouts.as_slice();
        if prevouts.len() != block.txdata.len() {
            return Err(
                MintingEndorsementProcessingError::TransactionCountMismatch {
                    transaction_count: block.txdata.len(),
                    prevout_count: prevouts.len(),
                },
            );
        }
        let mut records = BtcBlockRecords {
            id,
            burns: Vec::new(),
            endorsements: Vec::new(),
        };
        for (transaction, prevouts) in block.txdata.iter().zip(prevouts) {
            records.burns.extend(Burn::from_tx(transaction));
            records
                .endorsements
                .extend(validate_transaction_endorsements(transaction, prevouts)?);
        }
        Ok(records)
    }

    /// Reads the burns and the endorsements of a block that is in `storage`.
    ///
    /// The watcher stores the blocks of an event before it returns the event,
    /// so this is how a consumer reads the new blocks an event names.
    pub async fn from_storage<B: BtcStorage>(
        storage: &B,
        id: BlockId,
    ) -> Result<Self, BtcRecordsError> {
        let stored = storage.block(id).await?;
        let prevouts = storage.prevouts(id).await?;
        let (Some(stored), Some(prevouts)) = (stored, prevouts) else {
            return Err(BtcRecordsError::NotStored(id));
        };
        Ok(Self::from_block(id, &stored, &prevouts)?)
    }
}

/// A change of the best chain of Bitcoin, like [`crate::watcher::BtcEvent`],
/// with the Flame transactions of the new blocks read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtcRecordsEvent {
    /// New blocks on top of the best block, oldest first.
    NewBlocks { blocks: Vec<BtcBlockRecords> },
    /// The best chain switched to another branch: the blocks above `fork`
    /// were removed, newest first, and `blocks` took their place, oldest
    /// first.
    Reorg {
        fork: BlockId,
        removed: Vec<BlockId>,
        blocks: Vec<BtcBlockRecords>,
    },
}

impl BtcRecordsEvent {
    /// The new blocks, oldest first.
    pub fn blocks(&self) -> &[BtcBlockRecords] {
        match self {
            BtcRecordsEvent::NewBlocks { blocks } | BtcRecordsEvent::Reorg { blocks, .. } => blocks,
        }
    }

    /// The removed blocks, newest first. `NewBlocks` removes nothing.
    pub fn removed(&self) -> &[BlockId] {
        match self {
            BtcRecordsEvent::NewBlocks { .. } => &[],
            BtcRecordsEvent::Reorg { removed, .. } => removed,
        }
    }
}

/// Why the Flame transactions of a stored block could not be read.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BtcRecordsError {
    /// The storage did not answer.
    #[error("block storage: {0}")]
    Storage(#[from] StorageError),
    /// The block is not in the storage: no event has named it, or the storage
    /// has dropped it since.
    #[error("block {0} is not in the storage")]
    NotStored(BlockId),
    /// The endorsements of the block cannot be checked: an output that one of
    /// them spends is missing.
    #[error("the endorsements of the block cannot be checked: {0}")]
    Endorsements(#[from] MintingEndorsementProcessingError),
}
