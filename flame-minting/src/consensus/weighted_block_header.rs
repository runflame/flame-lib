use std::ops::Deref;

use flamechain::BlockHeader;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightedBlockHeader {
    pub header: BlockHeader,
    pub parent_weight: u64,
    pub effective_power: u64,
}

impl Deref for WeightedBlockHeader {
    type Target = BlockHeader;

    fn deref(&self) -> &Self::Target {
        &self.header
    }
}
