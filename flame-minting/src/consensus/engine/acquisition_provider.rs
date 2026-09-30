use std::collections::{HashMap, hash_map::Entry};

use btc_integration::MinterP2wsh;

use crate::consensus::{
    ConsensusStorage, IncludedAcquisition, MinterAcquisitions, MintingProtocolParams,
};

pub struct AcquisitionProvider<'a, C> {
    pub storage: &'a C,
    pub new_acquisitions: &'a [IncludedAcquisition],
    pub params: &'a MintingProtocolParams,
}

impl<C: ConsensusStorage> AcquisitionProvider<'_, C> {
    pub async fn load_through(
        &self,
        height: u64,
    ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, C::Error> {
        let mut acquisitions = self.storage.get_acquisitions_by_minters(height).await?;
        for acquisition in self
            .new_acquisitions
            .iter()
            .filter(|acquisition| acquisition.btc_block.height <= height)
        {
            let minter = acquisition.acquisition.data().minter_p2wsh;
            let record = match acquisitions.entry(minter) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let is_double_signed = self.storage.is_minter_double_signed(&minter).await?;
                    entry.insert(MinterAcquisitions {
                        is_double_signed,
                        acquisitions: vec![],
                    })
                }
            };
            record.acquisitions.push(acquisition.clone());
        }
        Ok(acquisitions)
    }

    pub async fn active_at(
        &self,
        target_height: u64,
    ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, C::Error> {
        let mut acquisitions = self.load_through(target_height).await?;
        acquisitions.retain(|_, minter| {
            minter
                .acquisitions
                .retain(|acquisition| acquisition.is_active_at(target_height, self.params));
            !minter.acquisitions.is_empty()
        });
        Ok(acquisitions)
    }
}
