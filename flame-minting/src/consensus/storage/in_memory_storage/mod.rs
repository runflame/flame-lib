use std::{
    collections::{BTreeMap, HashMap, btree_map::Entry},
    fmt,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use btc_integration::{BtcBlockTip, MinterP2wsh};
use flamechain::{BlockTip, CoreBlockTip, CoreFlameHeight};

use crate::consensus::{
    DoubleSign, IncludedAcquisition, IncludedVote, MintingProtocolParams, WeightedBlockHeader,
    WeightedVote,
};

use super::{ConsensusStorage, MinterAcquisitions};

mod acquisition_storage;
mod double_sign_storage;
mod vote_storage;
mod weighted_header_storage;

use acquisition_storage::AcquisitionStorage;
use double_sign_storage::DoubleSignStorage;
use vote_storage::VoteStorage;
use weighted_header_storage::WeightedHeaderStorage;

#[derive(Clone, Default)]
pub struct InMemoryConsensusStorage {
    state: Arc<RwLock<StorageState>>,
}

#[derive(Default)]
struct StorageState {
    acquisitions: AcquisitionStorage,
    votes: VoteStorage,
    weighted_headers: WeightedHeaderStorage,
    double_signs: DoubleSignStorage,
}

impl InMemoryConsensusStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn read_state(
        &self,
    ) -> Result<RwLockReadGuard<'_, StorageState>, InMemoryConsensusStorageError> {
        self.state
            .read()
            .map_err(|_| InMemoryConsensusStorageError::LockPoisoned)
    }

    fn write_state(
        &self,
    ) -> Result<RwLockWriteGuard<'_, StorageState>, InMemoryConsensusStorageError> {
        self.state
            .write()
            .map_err(|_| InMemoryConsensusStorageError::LockPoisoned)
    }
}

fn insert_unique<K: Ord, V: PartialEq>(
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

    async fn store_acquisition(
        &self,
        btc_block: BtcBlockTip,
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

    async fn store_vote(
        &self,
        btc_block: BtcBlockTip,
        vote: &WeightedVote,
    ) -> Result<(), Self::Error> {
        self.write_state()?.votes.store_vote(btc_block, vote)
    }

    async fn get_votes_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Vec<WeightedVote>, Self::Error> {
        self.read_state()?.votes.get_votes_for_block(tip)
    }

    async fn get_minter_vote_for_height(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> Result<Option<WeightedVote>, Self::Error> {
        self.read_state()?
            .votes
            .get_minter_vote_for_height(minter, flame_height)
    }

    async fn remove_vote(&self, vote: &WeightedVote) -> Result<(), Self::Error> {
        self.write_state()?.votes.remove_vote(vote)
    }

    async fn store_pending_vote(
        &self,
        tip: CoreBlockTip,
        vote: &IncludedVote,
    ) -> Result<(), Self::Error> {
        self.write_state()?.votes.store_pending_vote(tip, vote)
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
}

#[derive(Debug, PartialEq, Eq)]
pub enum InMemoryConsensusStorageError {
    LockPoisoned,
    BitcoinBlockMismatch,
    CoreBlockMismatch,
    BlockTipMismatch,
    ConflictingRecord,
    InvalidDoubleSign,
}

impl fmt::Display for InMemoryConsensusStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(formatter, "consensus storage lock poisoned"),
            Self::BitcoinBlockMismatch => {
                write!(formatter, "Bitcoin block does not match the record")
            }
            Self::CoreBlockMismatch => write!(formatter, "core block does not match the vote"),
            Self::BlockTipMismatch => {
                write!(formatter, "block tip does not match the weighted header")
            }
            Self::ConflictingRecord => write!(formatter, "conflicting consensus storage record"),
            Self::InvalidDoubleSign => write!(
                formatter,
                "double sign must contain conflicting votes from the specified minter at the specified height"
            ),
        }
    }
}

impl std::error::Error for InMemoryConsensusStorageError {}
