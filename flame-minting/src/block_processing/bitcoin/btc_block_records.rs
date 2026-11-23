use btc_integration::{
    AuthenticatedMintingEndorsement, BitcoinRecords, Burn, RecordsError,
    watcher::{BlockId, BtcStorage},
};

/// A Bitcoin block as consensus sees it: which block it is, and its burns
/// and endorsements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcBlockRecords {
    pub id: BlockId,
    pub burns: Vec<Burn>,
    pub endorsements: Vec<AuthenticatedMintingEndorsement>,
}

impl BtcBlockRecords {
    /// Reads the block `id` from the storage of the watcher.
    pub async fn from_storage<B: BtcStorage>(
        storage: &B,
        id: BlockId,
    ) -> Result<Self, RecordsError> {
        let BitcoinRecords {
            burns,
            endorsements,
        } = BitcoinRecords::from_storage(storage, id).await?;
        Ok(Self {
            id,
            burns,
            endorsements,
        })
    }
}
