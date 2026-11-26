use super::InMemoryConsensusStorageError;
use super::in_memory_consensus_storage::insert_unique;
use crate::consensus::{
    AcquisitionIndex, IncludedAcquisition, MinterAcquisitions, MintingProtocolParams,
};
use btc_integration::{MinterP2wsh, watcher::BlockId};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub(super) struct AcquisitionStorage {
    acquisitions: BTreeMap<AcquisitionIndex, IncludedAcquisition>,
}

impl AcquisitionStorage {
    pub(super) fn get_acquisition(&self, index: AcquisitionIndex) -> Option<IncludedAcquisition> {
        self.acquisitions.get(&index).cloned()
    }

    pub(super) fn restore_acquisition(
        &mut self,
        index: AcquisitionIndex,
        previous: Option<&IncludedAcquisition>,
    ) -> Result<(), InMemoryConsensusStorageError> {
        match previous {
            Some(previous) => {
                if AcquisitionIndex::from(&previous.acquisition) != index {
                    return Err(InMemoryConsensusStorageError::AcquisitionKeyMismatch);
                }
                self.acquisitions.insert(index, previous.clone());
            }
            None => {
                self.acquisitions.remove(&index);
            }
        }
        Ok(())
    }

    pub(super) fn store_acquisition(
        &mut self,
        btc_block: BlockId,
        acquisition: &IncludedAcquisition,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if btc_block != acquisition.btc_block {
            return Err(InMemoryConsensusStorageError::BitcoinBlockMismatch);
        }
        let index = AcquisitionIndex::from(&acquisition.acquisition);
        insert_unique(&mut self.acquisitions, index, acquisition.clone())
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

#[cfg(test)]
mod tests;
