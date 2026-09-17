use std::future::Future;

use crate::consensus::{DoubleSign, IncludedAcquisition, WeightedVote};
use btc_integration::{BtcBlockTip, MinterP2wsh};
use flamechain::CoreFlameHeight;

pub trait ConsensusStorage {
    type Error;

    fn store_acquisition(
        &self,
        btc_block: BtcBlockTip,
        acquisition: &IncludedAcquisition,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn store_vote(
        &self,
        btc_block: BtcBlockTip,
        vote: &WeightedVote,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_minter_vote_for_height(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> impl Future<Output = Result<Option<WeightedVote>, Self::Error>> + Send;

    fn add_double_sign(
        &self,
        double_sign: &DoubleSign,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn get_double_sign(
        &self,
        minter: &MinterP2wsh,
        flame_height: CoreFlameHeight,
    ) -> impl Future<Output = Result<Option<DoubleSign>, Self::Error>> + Send;
}
