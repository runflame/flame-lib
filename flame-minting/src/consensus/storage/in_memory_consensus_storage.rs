use super::InMemoryConsensusStorageError;
use super::acquisition_storage::AcquisitionStorage;
use super::double_sign_storage::DoubleSignStorage;
use super::endorsement_storage::EndorsementStorage;
use super::weighted_header_storage::WeightedHeaderStorage;
use super::{
    AcquisitionIndex, ConsensusStorage, EndorsementIndex, MinterAcquisitions,
    MinterEndorsementState,
};
use crate::consensus::{
    DoubleSign, IncludedAcquisition, IncludedEndorsement, MintingProtocolParams,
    PendingEndorsements, WeightedBlockHeader, WeightedEndorsement,
};
use btc_integration::{MinterP2wsh, watcher::BlockId};
use flamechain::{BlockTip, CoreBlockTip, CoreFlameHeight};
use std::{
    collections::{BTreeMap, HashMap, btree_map::Entry},
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

#[derive(Clone, Default)]
pub struct InMemoryConsensusStorage {
    state: Arc<RwLock<ConsensusStorageState>>,
}

#[derive(Default)]
pub(super) struct ConsensusStorageState {
    pub(super) acquisitions: AcquisitionStorage,
    pub(super) endorsements: EndorsementStorage,
    pub(super) weighted_headers: WeightedHeaderStorage,
    pub(super) double_signs: DoubleSignStorage,
}

impl InMemoryConsensusStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub(super) fn read_state(
        &self,
    ) -> Result<RwLockReadGuard<'_, ConsensusStorageState>, InMemoryConsensusStorageError> {
        self.state
            .read()
            .map_err(|_| InMemoryConsensusStorageError::LockPoisoned)
    }

    fn write_state(
        &self,
    ) -> Result<RwLockWriteGuard<'_, ConsensusStorageState>, InMemoryConsensusStorageError> {
        self.state
            .write()
            .map_err(|_| InMemoryConsensusStorageError::LockPoisoned)
    }
}

pub(super) fn insert_unique<K: Ord, V: PartialEq>(
    entries: &mut BTreeMap<K, V>,
    key: K,
    value: V,
) -> Result<(), InMemoryConsensusStorageError> {
    match entries.entry(key) {
        Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        Entry::Occupied(entry) if entry.get() == &value => Ok(()),
        Entry::Occupied(_) => Err(InMemoryConsensusStorageError::ConflictingRecord),
    }
}

impl ConsensusStorage for InMemoryConsensusStorage {
    type Error = InMemoryConsensusStorageError;

    async fn get_minter_endorsement_state(
        &self,
        minter: &MinterP2wsh,
    ) -> Result<MinterEndorsementState, Self::Error> {
        let state = self.read_state()?;
        let (endorsements, pending_endorsements) = state.endorsements.for_minter(minter);
        Ok(MinterEndorsementState {
            endorsements,
            pending_endorsements,
            double_signs: state.double_signs.for_minter(minter),
        })
    }

    async fn remove_endorsement_inclusion(
        &self,
        endorsement: &IncludedEndorsement,
    ) -> Result<(), Self::Error> {
        let mut state = self.write_state()?;
        state.endorsements.remove_inclusion(endorsement);
        state.double_signs.remove_inclusion(endorsement);
        Ok(())
    }

    async fn get_acquisition(
        &self,
        index: AcquisitionIndex,
    ) -> Result<Option<IncludedAcquisition>, Self::Error> {
        Ok(self.read_state()?.acquisitions.get_acquisition(index))
    }

    async fn restore_acquisition(
        &self,
        index: AcquisitionIndex,
        previous: Option<&IncludedAcquisition>,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .acquisitions
            .restore_acquisition(index, previous)
    }

    async fn store_acquisition(
        &self,
        btc_block: BlockId,
        acquisition: &IncludedAcquisition,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .acquisitions
            .store_acquisition(btc_block, acquisition)
    }

    async fn get_acquisitions_by_minters(
        &self,
        btc_height: u64,
    ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, Self::Error> {
        let state = self.read_state()?;
        let mut minters = state.acquisitions.get_acquisitions_by_minters(btc_height)?;
        for (minter, acquisitions) in &mut minters {
            acquisitions.is_double_signed = state.double_signs.is_minter_double_signed(minter);
        }
        Ok(minters)
    }

    async fn get_active_minter_acquisitions_at_height(
        &self,
        minter: &MinterP2wsh,
        btc_height: u64,
        params: &MintingProtocolParams,
    ) -> Result<MinterAcquisitions, Self::Error> {
        let state = self.read_state()?;
        let mut acquisitions = state
            .acquisitions
            .get_active_minter_acquisitions_at_height(minter, btc_height, params)?;
        acquisitions.is_double_signed = state.double_signs.is_minter_double_signed(minter);
        Ok(acquisitions)
    }

    async fn store_endorsement(
        &self,
        btc_block: BlockId,
        endorsement: &WeightedEndorsement,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .endorsements
            .store_endorsement(btc_block, endorsement)
    }

    async fn get_endorsements_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Vec<WeightedEndorsement>, Self::Error> {
        self.read_state()?
            .endorsements
            .get_endorsements_for_block(tip)
    }

    async fn get_minter_endorsement_for_height(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> Result<Option<WeightedEndorsement>, Self::Error> {
        self.read_state()?
            .endorsements
            .get_minter_endorsement_for_height(minter, flame_height)
    }

    async fn remove_endorsement(
        &self,
        endorsement: &WeightedEndorsement,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .endorsements
            .remove_endorsement(endorsement)
    }

    async fn store_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        endorsement: &IncludedEndorsement,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .endorsements
            .store_pending_endorsement(tip, endorsement)
    }

    async fn restore_endorsement(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
        previous: Option<&WeightedEndorsement>,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .endorsements
            .restore_endorsement(minter, flame_height, previous)
    }

    async fn get_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
    ) -> Result<Option<IncludedEndorsement>, Self::Error> {
        Ok(self
            .read_state()?
            .endorsements
            .get_pending_endorsement(tip, index))
    }

    async fn restore_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
        previous: Option<&IncludedEndorsement>,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .endorsements
            .restore_pending_endorsement(tip, index, previous)
    }

    async fn get_pending_endorsements_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<PendingEndorsements, Self::Error> {
        Ok(self
            .read_state()?
            .endorsements
            .get_pending_endorsements_for_block(tip))
    }

    async fn get_block_tip_with_most_weight(&self) -> Result<Option<BlockTip>, Self::Error> {
        Ok(self
            .read_state()?
            .weighted_headers
            .get_block_tip_with_most_weight())
    }

    async fn get_cumulative_weight(
        &self,
        tip: BlockTip,
    ) -> Result<Option<WeightedBlockHeader>, Self::Error> {
        Ok(self
            .read_state()?
            .weighted_headers
            .get_cumulative_weight(tip))
    }

    async fn store_cumulative_weight(
        &self,
        tip: BlockTip,
        weighted_block: &WeightedBlockHeader,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .weighted_headers
            .store_cumulative_weight(tip, weighted_block)
    }

    async fn restore_cumulative_weight(
        &self,
        tip: BlockTip,
        previous: Option<&WeightedBlockHeader>,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .weighted_headers
            .restore_cumulative_weight(tip, previous)
    }

    async fn add_double_sign(&self, double_sign: &DoubleSign) -> Result<(), Self::Error> {
        self.write_state()?
            .double_signs
            .add_double_sign(double_sign)
    }

    async fn get_double_sign(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> Result<Option<DoubleSign>, Self::Error> {
        Ok(self
            .read_state()?
            .double_signs
            .get_double_sign(minter, flame_height))
    }

    async fn is_minter_double_signed(&self, minter: &MinterP2wsh) -> Result<bool, Self::Error> {
        Ok(self
            .read_state()?
            .double_signs
            .is_minter_double_signed(minter))
    }

    async fn restore_double_sign(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
        previous: Option<&DoubleSign>,
    ) -> Result<(), Self::Error> {
        self.write_state()?
            .double_signs
            .restore_double_sign(minter, flame_height, previous)
    }
}
