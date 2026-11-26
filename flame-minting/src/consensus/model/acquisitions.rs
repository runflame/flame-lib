use crate::consensus::MintingProtocolParams;
use bitcoin::Txid;
use btc_integration::{Burn, watcher::BlockId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncludedAcquisition {
    pub btc_block: BlockId,
    pub acquisition: Burn,
}

impl IncludedAcquisition {
    pub fn duration(&self, params: &MintingProtocolParams) -> u16 {
        self.acquisition
            .data()
            .duration
            .unwrap_or(params.default_acquisition_duration.get())
    }

    pub fn is_active_at(&self, target_btc_height: u64, params: &MintingProtocolParams) -> bool {
        // X + maturity <= target < X + maturity + duration, without overflowing.
        target_btc_height
            .checked_sub(self.btc_block.height)
            .and_then(|age| age.checked_sub(u64::from(params.acquisition_maturity)))
            .is_some_and(|active_age| active_age < u64::from(self.duration(params)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinterAcquisitions {
    pub is_double_signed: bool,
    pub acquisitions: Vec<IncludedAcquisition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AcquisitionIndex {
    pub txid: Txid,
    pub output_index: usize,
}

impl From<&Burn> for AcquisitionIndex {
    fn from(acquisition: &Burn) -> Self {
        Self {
            txid: acquisition.txid(),
            output_index: acquisition.output_index(),
        }
    }
}
