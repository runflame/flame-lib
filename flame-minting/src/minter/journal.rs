use std::future::Future;

use btc_integration::{AuthenticatedMintingVote, MintingVoteData};

pub trait MinterJournal {
    type Error;
    type TransactionId;

    fn write_reserve_vote(
        &self,
        vote: &MintingVoteData,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn write_sent_vote(
        &self,
        vote: &MintingVoteData,
        transaction_id: &Self::TransactionId,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn write_committed_vote(
        &self,
        vote: &AuthenticatedMintingVote,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
