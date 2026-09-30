use std::convert::Infallible;

use flame_minting::core_block_notifier::CoreBlockNotifier;

pub struct UnusedNotifier;

impl CoreBlockNotifier for UnusedNotifier {
    type Error = Infallible;

    async fn notify_core_block_needed(&mut self, _: u64) -> Result<(), Self::Error> {
        unreachable!("the test supplies the core block")
    }
}
