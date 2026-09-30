use std::collections::{BTreeMap, HashMap};

use btc_integration::{BtcBlockTip, MinterP2wsh};
use corepc_client::bitcoin::Txid;

use crate::consensus::{IncludedAcquisition, MinterAcquisitions, MintingProtocolParams};

use super::{InMemoryConsensusStorageError, insert_unique};

#[derive(Default)]
pub(super) struct AcquisitionStorage {
    acquisitions: BTreeMap<(Txid, usize), IncludedAcquisition>,
}

impl AcquisitionStorage {
    pub(super) fn store_acquisition(
        &mut self,
        btc_block: BtcBlockTip,
        acquisition: &IncludedAcquisition,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if btc_block != acquisition.btc_block {
            return Err(InMemoryConsensusStorageError::BitcoinBlockMismatch);
        }
        let key = (
            acquisition.acquisition.txid(),
            acquisition.acquisition.output_index(),
        );
        insert_unique(&mut self.acquisitions, key, acquisition.clone())
    }

    pub(super) fn get_acquisitions_by_minters(
        &self,
        btc_height: u64,
    ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, InMemoryConsensusStorageError> {
        let mut minters = HashMap::new();
        for acquisition in self.acquisitions.values() {
            if acquisition.btc_block.height <= btc_height {
                minters
                    .entry(acquisition.acquisition.data().minter_p2wsh)
                    .or_insert_with(|| MinterAcquisitions {
                        is_double_signed: false,
                        acquisitions: Vec::new(),
                    })
                    .acquisitions
                    .push(acquisition.clone());
            }
        }
        Ok(minters)
    }

    pub(super) fn get_active_minter_acquisitions_at_height(
        &self,
        minter: &MinterP2wsh,
        btc_height: u64,
        params: &MintingProtocolParams,
    ) -> Result<MinterAcquisitions, InMemoryConsensusStorageError> {
        Ok(MinterAcquisitions {
            is_double_signed: false,
            acquisitions: self
                .acquisitions
                .values()
                .filter(|acquisition| {
                    acquisition.acquisition.data().minter_p2wsh == *minter
                        && acquisition.is_active_at(btc_height, params)
                })
                .cloned()
                .collect(),
        })
    }
}
