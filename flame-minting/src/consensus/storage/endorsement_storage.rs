use super::InMemoryConsensusStorageError;
use super::in_memory_consensus_storage::insert_unique;
use crate::consensus::{
    EndorsementIndex, IncludedEndorsement, PendingEndorsements, WeightedEndorsement,
};
use btc_integration::{MinterP2wsh, watcher::BlockId};
use flamechain::{CoreBlockTip, CoreFlameHeight};
use std::collections::{BTreeMap, btree_map::Entry};

#[derive(Default)]
pub(super) struct EndorsementStorage {
    endorsements: BTreeMap<(MinterP2wsh, CoreFlameHeight), WeightedEndorsement>,
    pub(super) pending_endorsements:
        BTreeMap<CoreBlockTip, BTreeMap<EndorsementIndex, IncludedEndorsement>>,
}

impl EndorsementStorage {
    pub(super) fn for_minter(
        &self,
        minter: &MinterP2wsh,
    ) -> (Vec<WeightedEndorsement>, Vec<IncludedEndorsement>) {
        let endorsements = self
            .endorsements
            .range((*minter, 0.into())..=(*minter, u32::MAX.into()))
            .map(|(_, entry)| entry.clone())
            .collect();
        let pending = self
            .pending_endorsements
            .values()
            .flat_map(|entries| entries.values())
            .filter(|entry| entry.endorsement.auth().minter().p2wsh() == *minter)
            .cloned()
            .collect();
        (endorsements, pending)
    }

    pub(super) fn remove_inclusion(&mut self, included: &IncludedEndorsement) {
        let minter = included.endorsement.auth().minter().p2wsh();
        if let Entry::Occupied(entry) = self.endorsements.entry((minter, included.block_height())) {
            if same_inclusion(&entry.get().original, included) {
                entry.remove();
            }
        }
        if let Entry::Occupied(mut group) = self
            .pending_endorsements
            .entry(included.endorsement.block_tip())
        {
            let index = EndorsementIndex::from(&included.endorsement);
            if group
                .get()
                .get(&index)
                .is_some_and(|entry| same_inclusion(entry, included))
            {
                group.get_mut().remove(&index);
            }
            if group.get().is_empty() {
                group.remove();
            }
        }
    }

    pub(super) fn store_endorsement(
        &mut self,
        btc_block: BlockId,
        endorsement: &WeightedEndorsement,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if btc_block != endorsement.original.btc_block {
            return Err(InMemoryConsensusStorageError::BitcoinBlockMismatch);
        }
        let key = (
            endorsement.original.endorsement.auth().minter().p2wsh(),
            endorsement.original.block_height(),
        );
        insert_unique(&mut self.endorsements, key, endorsement.clone())
    }

    pub(super) fn get_endorsements_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Vec<WeightedEndorsement>, InMemoryConsensusStorageError> {
        Ok(self
            .endorsements
            .values()
            .filter(|endorsement| endorsement.original.endorsement.block_tip() == tip)
            .cloned()
            .collect())
    }

    pub(super) fn get_minter_endorsement_for_height(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> Result<Option<WeightedEndorsement>, InMemoryConsensusStorageError> {
        Ok(self.endorsements.get(&(*minter, flame_height)).cloned())
    }

    pub(super) fn remove_endorsement(
        &mut self,
        endorsement: &WeightedEndorsement,
    ) -> Result<(), InMemoryConsensusStorageError> {
        let key = (
            endorsement.original.endorsement.auth().minter().p2wsh(),
            endorsement.original.block_height(),
        );
        if let Entry::Occupied(entry) = self.endorsements.entry(key) {
            if entry.get().original != endorsement.original {
                return Err(InMemoryConsensusStorageError::ConflictingRecord);
            }
            entry.remove();
        }
        Ok(())
    }

    pub(super) fn store_pending_endorsement(
        &mut self,
        tip: CoreBlockTip,
        endorsement: &IncludedEndorsement,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if tip != endorsement.endorsement.block_tip() {
            return Err(InMemoryConsensusStorageError::CoreBlockMismatch);
        }
        let index = EndorsementIndex::from(&endorsement.endorsement);
        insert_unique(
            self.pending_endorsements.entry(tip).or_default(),
            index,
            endorsement.clone(),
        )
    }

    pub(super) fn restore_endorsement(
        &mut self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
        previous: Option<&WeightedEndorsement>,
    ) -> Result<(), InMemoryConsensusStorageError> {
        let key = (*minter, flame_height);
        match previous {
            Some(previous) => {
                if previous.original.endorsement.auth().minter().p2wsh() != *minter
                    || previous.original.block_height() != flame_height
                {
                    return Err(InMemoryConsensusStorageError::EndorsementKeyMismatch);
                }
                self.endorsements.insert(key, previous.clone());
            }
            None => {
                self.endorsements.remove(&key);
            }
        }
        Ok(())
    }

    pub(super) fn get_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
    ) -> Option<IncludedEndorsement> {
        self.pending_endorsements.get(&tip)?.get(&index).cloned()
    }

    pub(super) fn get_pending_endorsements_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> PendingEndorsements {
        let Some(pending) = self.pending_endorsements.get(&tip) else {
            return Vec::new();
        };
        let mut endorsements: Vec<_> = pending.values().cloned().collect();
        endorsements.sort_by_key(|included| {
            (
                included.btc_block.height,
                EndorsementIndex::from(&included.endorsement),
            )
        });
        endorsements
    }

    pub(super) fn restore_pending_endorsement(
        &mut self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
        previous: Option<&IncludedEndorsement>,
    ) -> Result<(), InMemoryConsensusStorageError> {
        match previous {
            Some(previous) => {
                if previous.endorsement.block_tip() != tip {
                    return Err(InMemoryConsensusStorageError::CoreBlockMismatch);
                }
                if EndorsementIndex::from(&previous.endorsement) != index {
                    return Err(InMemoryConsensusStorageError::PendingEndorsementKeyMismatch);
                }
                self.pending_endorsements
                    .entry(tip)
                    .or_default()
                    .insert(index, previous.clone());
            }
            None => {
                if let Entry::Occupied(mut entry) = self.pending_endorsements.entry(tip) {
                    entry.get_mut().remove(&index);
                    if entry.get().is_empty() {
                        entry.remove();
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) fn same_inclusion(first: &IncludedEndorsement, second: &IncludedEndorsement) -> bool {
    first.btc_block == second.btc_block
        && EndorsementIndex::from(&first.endorsement) == EndorsementIndex::from(&second.endorsement)
}

#[cfg(test)]
mod tests;
