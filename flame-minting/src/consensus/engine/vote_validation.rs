use std::collections::HashMap;

use btc_integration::{AuthenticatedMintingVote, IndexedBlock, MinterIdentity, MinterP2wsh};
use flame_storage::ChainStorage;
use flamechain::CoreFlameHeight;

use crate::consensus::{
    ConsensusStorage, DoubleSign, IncludedVote, MinterAcquisitions, MintingProtocolParams,
    WeightedVote,
};

pub struct VoteValidator<'a, C, H> {
    pub new_block: &'a IndexedBlock,
    pub protocol_params: &'a MintingProtocolParams,
    pub consensus_storage: &'a C,
    pub chain_storage: &'a H,
    pub acquisitions: &'a HashMap<MinterP2wsh, MinterAcquisitions>,
}

impl<C: ConsensusStorage, H: ChainStorage> VoteValidator<'_, C, H> {
    pub async fn validate(
        &self,
    ) -> Result<VoteValidationResult, VoteValidationError<C::Error, H::Error>> {
        let mut minters: HashMap<MinterIdentity, MinterVotes> = HashMap::new();

        for vote in &self.new_block.votes {
            match self
                .check_eligibility(vote)
                .await
                .map_err(VoteValidationError::ChainStorage)?
            {
                VoteEligibility::Rejected => {}
                VoteEligibility::Pending(vote) => {
                    minters
                        .entry(vote.vote.auth().minter().clone())
                        .or_default()
                        .pending
                        .push(vote);
                }
                VoteEligibility::Eligible(vote) => {
                    let minter = vote.vote.auth().minter();
                    let votes = minters.entry(minter.clone()).or_default();
                    let stored = if votes.by_height.contains_key(&vote.block_height()) {
                        None
                    } else {
                        self.consensus_storage
                            .get_minter_vote_for_height(&minter.p2wsh(), vote.block_height())
                            .await
                            .map_err(VoteValidationError::ConsensusStorage)?
                    };
                    votes.record_vote(vote, stored);
                }
            }
        }

        let mut result = VoteValidationResult::default();
        for votes in minters.into_values() {
            votes.append_to(&mut result);
        }
        Ok(result)
    }

    async fn check_eligibility(
        &self,
        vote: &AuthenticatedMintingVote,
    ) -> Result<VoteEligibility, H::Error> {
        let Some(minter) = self.acquisitions.get(&vote.auth().minter().p2wsh()) else {
            return Ok(VoteEligibility::Rejected);
        };
        if minter.is_double_signed || minter.acquisitions.is_empty() {
            return Ok(VoteEligibility::Rejected);
        }

        let included = IncludedVote {
            btc_block: self.new_block.btc_block_tip,
            vote: vote.clone(),
        };
        let Some(core) = self
            .chain_storage
            .get_core_block_header(vote.block_tip())
            .await?
        else {
            return Ok(VoteEligibility::Pending(included));
        };
        let target = u64::from(core.target_btc_height);
        if included.btc_block.height < target
            || !minter
                .acquisitions
                .iter()
                .any(|acquisition| acquisition.is_active_at(target, self.protocol_params))
        {
            return Ok(VoteEligibility::Rejected);
        }

        Ok(VoteEligibility::Eligible(included))
    }
}

enum VoteEligibility {
    Rejected,
    Pending(IncludedVote),
    Eligible(IncludedVote),
}

#[derive(Default)]
struct MinterVotes {
    by_height: HashMap<CoreFlameHeight, HeightVotes>,
    pending: Vec<IncludedVote>,
    removed_votes: Vec<WeightedVote>,
}

impl MinterVotes {
    fn record_vote(&mut self, vote: IncludedVote, stored: Option<WeightedVote>) {
        let height = vote.block_height();
        let previous = self
            .by_height
            .remove(&height)
            .or_else(|| stored.map(|vote| HeightVotes::Single(VoteOrigin::Stored(vote))));
        let state = match previous {
            Some(state) => state.record_vote(vote, &mut self.removed_votes),
            None => HeightVotes::Single(VoteOrigin::Current(vote)),
        };
        self.by_height.insert(height, state);
    }

    fn append_to(self, result: &mut VoteValidationResult) {
        let disqualified = self
            .by_height
            .values()
            .any(|state| matches!(state, HeightVotes::DoubleSigned(_)));

        for state in self.by_height.into_values() {
            match state {
                HeightVotes::Single(VoteOrigin::Current(vote)) if !disqualified => {
                    result.valid_votes.push(vote);
                }
                HeightVotes::DoubleSigned(sign) => result.double_signs.push(sign),
                _ => {}
            }
        }
        if !disqualified {
            result.pending_votes.extend(self.pending);
        }
        result.removed_votes.extend(self.removed_votes);
    }
}

enum HeightVotes {
    Single(VoteOrigin),
    DoubleSigned(DoubleSign),
}

impl HeightVotes {
    fn record_vote(self, vote: IncludedVote, removed_votes: &mut Vec<WeightedVote>) -> Self {
        match self {
            Self::Single(previous) if previous.original().block_hash() == vote.block_hash() => {
                Self::Single(previous)
            }
            Self::Single(previous) => {
                let first = match previous {
                    VoteOrigin::Current(first) => first,
                    VoteOrigin::Stored(stored) => {
                        let first = stored.original.clone();
                        removed_votes.push(stored);
                        first
                    }
                };
                Self::DoubleSigned(DoubleSign {
                    minter: vote.vote.auth().minter().p2wsh(),
                    target_flame_height: vote.block_height(),
                    votes: vec![first, vote],
                })
            }
            Self::DoubleSigned(mut sign) => {
                sign.votes.push(vote);
                Self::DoubleSigned(sign)
            }
        }
    }
}

enum VoteOrigin {
    Current(IncludedVote),
    Stored(WeightedVote),
}

impl VoteOrigin {
    fn original(&self) -> &IncludedVote {
        match self {
            Self::Current(vote) => vote,
            Self::Stored(vote) => &vote.original,
        }
    }
}

#[derive(Default)]
pub(in crate::consensus) struct VoteValidationResult {
    pub valid_votes: Vec<IncludedVote>,
    pub double_signs: Vec<DoubleSign>,
    pub removed_votes: Vec<WeightedVote>,
    pub pending_votes: Vec<IncludedVote>,
}

#[derive(Debug)]
pub(in crate::consensus) enum VoteValidationError<C, H> {
    ConsensusStorage(C),
    ChainStorage(H),
}
