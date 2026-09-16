use std::future::Future;

use flamechain::BlockHash;

pub trait CanonicalStorage {
    type State;
    type Error;

    fn get_state(
        &self,
    ) -> impl Future<Output = Result<Option<(BlockHash, Self::State)>, Self::Error>> + Send;
    fn commit_state(
        &self,
        tip: BlockHash,
        state: &Self::State,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
