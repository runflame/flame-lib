use std::future::Future;

use flamechain::BlockHash;

pub trait ProvisionalStorage {
    type State;
    type Error;

    fn get_state(
        &self,
        tip: BlockHash,
    ) -> impl Future<Output = Result<Option<Self::State>, Self::Error>> + Send;
    fn store_state(
        &self,
        tip: BlockHash,
        state: &Self::State,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
