use std::sync::Arc;

use btc_integration::MintingVoteData;
use flamechain::Block;

use super::{MinterJournal, VoteReservation, ports::VoteSender};

#[derive(Debug, PartialEq, Eq)]
pub enum VoteError<S, J> {
    NotCoreBlock,
    Journal(J),
    Send(S),
}

pub struct Voter<S, J> {
    sender: Arc<S>,
    journal: Arc<J>,
}

impl<S, J> Voter<S, J>
where
    S: VoteSender,
    J: MinterJournal<TransactionId = S::TransactionId> + Send + Sync,
{
    pub fn new(sender: Arc<S>, journal: Arc<J>) -> Self {
        Self { sender, journal }
    }

    pub async fn vote(&self, block: &Block) -> Result<(), VoteError<S::Error, J::Error>> {
        let core = block
            .header
            .core_block
            .as_ref()
            .ok_or(VoteError::NotCoreBlock)?;
        let hash = block.header.id();
        let vote = MintingVoteData::V1 {
            flame_block_height: core.height.as_u32(),
            flame_block_hash: hash,
        };
        let reservation = self
            .journal
            .write_reserve_vote(&vote)
            .await
            .map_err(VoteError::Journal)?;
        if reservation == VoteReservation::AlreadyReserved {
            return Ok(());
        }
        let transaction_id = self
            .sender
            .send_vote(core.height.as_u32(), hash)
            .await
            .map_err(VoteError::Send)?;
        self.journal
            .write_sent_vote(&vote, &transaction_id)
            .await
            .map_err(VoteError::Journal)?;
        log::info!(
            "sent vote for core block {} ({hash:?})",
            core.height.as_u32()
        );
        Ok(())
    }
}
