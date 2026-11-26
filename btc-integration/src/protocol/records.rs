use bitcoin::Block;
use thiserror::Error;

use crate::protocol::{
    AuthenticatedMintingEndorsement, Burn, MintingEndorsementProcessingError,
    validate_transaction_endorsements,
};
use crate::watcher::{BlockId, BtcStorage, Prevouts, StorageError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcBlockRecords {
    pub id: BlockId,
    pub burns: Vec<Burn>,
    pub endorsements: Vec<AuthenticatedMintingEndorsement>,
}

impl BtcBlockRecords {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtcRecordsEvent {
    NewBlocks {
        blocks: Vec<BtcBlockRecords>,
    },
    Reorg {
        fork: BlockId,
        removed: Vec<BlockId>,
        blocks: Vec<BtcBlockRecords>,
    },
}

impl BtcRecordsEvent {
    pub fn blocks(&self) -> &[BtcBlockRecords] {
        match self {
            BtcRecordsEvent::NewBlocks { blocks } | BtcRecordsEvent::Reorg { blocks, .. } => blocks,
        }
    }

    pub fn removed(&self) -> &[BlockId] {
        match self {
            BtcRecordsEvent::NewBlocks { .. } => &[],
            BtcRecordsEvent::Reorg { removed, .. } => removed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BtcRecordsError {
    #[error("block storage: {0}")]
    Storage(#[from] StorageError),
    #[error("block {0} is not in the storage")]
    NotStored(BlockId),
    #[error("the endorsements of the block cannot be checked: {0}")]
    Endorsements(#[from] MintingEndorsementProcessingError),
}
