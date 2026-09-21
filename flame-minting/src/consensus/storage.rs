use std::collections::HashMap;
use std::future::Future;

use crate::consensus::{DoubleSign, IncludedAcquisition, WeightedBlockHeader, WeightedVote};
use btc_integration::{BtcBlockTip, MinterP2wsh};
use flamechain::{BlockTip, CoreFlameHeight};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinterAcquisitions {
    pub is_double_signed: bool,
    pub acquisitions: Vec<IncludedAcquisition>,
}

pub trait ConsensusStorage {
    type Error;

    fn get_cumulative_weight(
        &self,
        tip: BlockTip,
    ) -> impl Future<Output = Result<Option<WeightedBlockHeader>, Self::Error>> + Send;

    fn store_acquisition(
        &self,
        btc_block: BtcBlockTip,
        acquisition: &IncludedAcquisition,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Returns acquisitions recorded through `btc_height`, including immature and expired ones.
    /// Acquisition activity is evaluated by consensus at each vote's target Bitcoin height.
    fn get_acquisitions_by_minters(
        &self,
        btc_height: u64,
    ) -> impl Future<Output = Result<HashMap<MinterP2wsh, MinterAcquisitions>, Self::Error>> + Send;

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

    fn is_minter_double_signed(
        &self,
        minter: &MinterP2wsh,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;
}
