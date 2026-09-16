use std::future::Future;

pub trait CoreBlockNotifier {
    type Error;

    fn notify_core_block_needed(
        &mut self,
        btc_block_height: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
