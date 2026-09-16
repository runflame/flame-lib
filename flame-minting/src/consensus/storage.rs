use std::future::Future;

use btc_integration::{Acquisition, AuthenticatedMintingVote, BtcBlockTip};

pub trait ConsensusStorage {
    type Error;

    fn store_acquisition(
        &self,
        btc_block: BtcBlockTip,
        acquisition: &Acquisition,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn store_vote(
        &self,
        btc_block: BtcBlockTip,
        vote: &AuthenticatedMintingVote,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
