use std::collections::{BTreeMap, btree_map::Entry};

use btc_integration::{BtcBlockTip, MinterP2wsh};
use corepc_client::bitcoin::Txid;
use flamechain::{CoreBlockTip, CoreFlameHeight};

use crate::consensus::{IncludedVote, WeightedVote};

use super::{InMemoryConsensusStorageError, insert_unique};

#[derive(Default)]
pub(super) struct VoteStorage {
    votes: BTreeMap<(MinterP2wsh, CoreFlameHeight), WeightedVote>,
    pub(super) pending_votes: BTreeMap<CoreBlockTip, BTreeMap<(Txid, usize), IncludedVote>>,
}

impl VoteStorage {
    pub(super) fn store_vote(
        &mut self,
        btc_block: BtcBlockTip,
        vote: &WeightedVote,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if btc_block != vote.original.btc_block {
            return Err(InMemoryConsensusStorageError::BitcoinBlockMismatch);
        }
        let key = (
            vote.original.vote.auth().minter().p2wsh(),
            vote.original.block_height(),
        );
        insert_unique(&mut self.votes, key, vote.clone())
    }

    pub(super) fn get_votes_for_block(
        &self,
        tip: CoreBlockTip,
    ) -> Result<Vec<WeightedVote>, InMemoryConsensusStorageError> {
        Ok(self
            .votes
            .values()
            .filter(|vote| vote.original.vote.block_tip() == tip)
            .cloned()
            .collect())
    }

    pub(super) fn get_minter_vote_for_height(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> Result<Option<WeightedVote>, InMemoryConsensusStorageError> {
        Ok(self.votes.get(&(*minter, flame_height)).cloned())
    }

    pub(super) fn remove_vote(
        &mut self,
        vote: &WeightedVote,
    ) -> Result<(), InMemoryConsensusStorageError> {
        let key = (
            vote.original.vote.auth().minter().p2wsh(),
            vote.original.block_height(),
        );
        if let Entry::Occupied(entry) = self.votes.entry(key) {
            if entry.get().original != vote.original {
                return Err(InMemoryConsensusStorageError::ConflictingRecord);
            }
            entry.remove();
        }
        Ok(())
    }

    pub(super) fn store_pending_vote(
        &mut self,
        tip: CoreBlockTip,
        vote: &IncludedVote,
    ) -> Result<(), InMemoryConsensusStorageError> {
        if tip != vote.vote.block_tip() {
            return Err(InMemoryConsensusStorageError::CoreBlockMismatch);
        }
        let key = (vote.vote.txid(), vote.vote.auth().input_index);
        insert_unique(
            self.pending_votes.entry(tip).or_default(),
            key,
            vote.clone(),
        )
    }
}
