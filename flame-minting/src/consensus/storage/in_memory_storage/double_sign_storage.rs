use std::collections::BTreeMap;

use btc_integration::MinterP2wsh;
use flamechain::CoreFlameHeight;

use crate::consensus::DoubleSign;

use super::InMemoryConsensusStorageError;

#[derive(Default)]
pub(super) struct DoubleSignStorage {
    signs: BTreeMap<MinterP2wsh, BTreeMap<CoreFlameHeight, DoubleSign>>,
}

impl DoubleSignStorage {
    pub(super) fn add_double_sign(
        &mut self,
        double_sign: &DoubleSign,
    ) -> Result<(), InMemoryConsensusStorageError> {
        validate_double_sign(double_sign)?;
        let stored = self
            .signs
            .entry(double_sign.minter)
            .or_default()
            .entry(double_sign.target_flame_height)
            .or_insert_with(|| DoubleSign {
                minter: double_sign.minter,
                target_flame_height: double_sign.target_flame_height,
                votes: Vec::new(),
            });
        for vote in &double_sign.votes {
            if !stored.votes.contains(vote) {
                stored.votes.push(vote.clone());
            }
        }
        Ok(())
    }

    pub(super) fn get_double_sign(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> Option<DoubleSign> {
        self.signs.get(minter)?.get(&flame_height).cloned()
    }

    pub(super) fn is_minter_double_signed(&self, minter: &MinterP2wsh) -> bool {
        self.signs.contains_key(minter)
    }
}

fn validate_double_sign(double_sign: &DoubleSign) -> Result<(), InMemoryConsensusStorageError> {
    let Some(first) = double_sign.votes.first() else {
        return Err(InMemoryConsensusStorageError::InvalidDoubleSign);
    };
    let matching_votes = double_sign.votes.iter().all(|vote| {
        vote.vote.auth().minter().p2wsh() == double_sign.minter
            && vote.block_height() == double_sign.target_flame_height
    });
    let conflicting_hashes = double_sign
        .votes
        .iter()
        .any(|vote| vote.block_hash() != first.block_hash());
    if !matching_votes || !conflicting_hashes {
        return Err(InMemoryConsensusStorageError::InvalidDoubleSign);
    }
    Ok(())
}
