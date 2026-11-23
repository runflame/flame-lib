use bitcoin::Block;
use thiserror::Error;

use crate::protocol::{
    AuthenticatedMintingEndorsement, Burn, MintingEndorsementProcessingError,
    validate_transaction_endorsements,
};
use crate::watcher::{BlockId, BtcStorage, Prevouts, StorageError};

/// The Flame transactions of one Bitcoin block, in block order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitcoinRecords {
    pub burns: Vec<Burn>,
    pub endorsements: Vec<AuthenticatedMintingEndorsement>,
}

impl BitcoinRecords {
    /// Reads the burns and the endorsements of `block`.
    ///
    /// `prevouts` are the outputs the inputs of the block spend. A burn does
    /// not need them. An endorsement counts only if its input spends an output
    /// of the minter it names, so an input that carries an endorsement and
    /// has no prevout is an error, not a skipped endorsement.
    pub fn from_block(
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
        let mut records = BitcoinRecords::default();
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
        block: BlockId,
    ) -> Result<Self, RecordsError> {
        let stored = storage.block(block).await?;
        let prevouts = storage.prevouts(block).await?;
        let (Some(stored), Some(prevouts)) = (stored, prevouts) else {
            return Err(RecordsError::NotStored(block));
        };
        Ok(Self::from_block(&stored, &prevouts)?)
    }
}

/// Why the Flame transactions of a stored block could not be read.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RecordsError {
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
