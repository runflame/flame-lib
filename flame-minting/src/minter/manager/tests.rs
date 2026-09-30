use std::convert::Infallible;

use corepc_client::bitcoin::Txid;
use flame_chain_service::InMemoryChain;
use flamechain::BlockHash;

use super::*;
use crate::minter::journal::in_memory::InMemoryMinterJournal;

struct UnusedSender;

impl VoteSender for UnusedSender {
    type Error = Infallible;
    type TransactionId = Txid;

    async fn send_vote(&self, _: u32, _: BlockHash) -> Result<Txid, Self::Error> {
        unreachable!()
    }
}

fn manager(
    vote_policy: VotePolicy,
) -> MinterManager<InMemoryChain, UnusedSender, InMemoryMinterJournal> {
    MinterManager::new(
        Arc::new(InMemoryChain::default()),
        Arc::new(UnusedSender),
        Arc::new(InMemoryMinterJournal::new()),
        vote_policy,
    )
}

#[tokio::test]
async fn manual_policy_does_not_start_a_worker() {
    let manager = manager(VotePolicy::Manual);
    let (updates, subscription) = watch::channel(None);

    manager.startup(subscription).await.unwrap();

    assert_eq!(updates.receiver_count(), 0);
    manager.shutdown().await.unwrap();
    manager.startup(updates.subscribe()).await.unwrap();
    assert_eq!(updates.receiver_count(), 0);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn auto_policy_starts_a_single_worker_and_supports_restart() {
    let manager = manager(VotePolicy::Auto);
    let (updates, subscription) = watch::channel(None);

    manager.startup(subscription).await.unwrap();

    assert_eq!(updates.receiver_count(), 1);
    assert!(matches!(
        manager.startup(updates.subscribe()).await,
        Err(StartupError::AlreadyRunning)
    ));
    manager.shutdown().await.unwrap();
    assert_eq!(updates.receiver_count(), 0);

    manager.startup(updates.subscribe()).await.unwrap();
    assert_eq!(updates.receiver_count(), 1);
    manager.shutdown().await.unwrap();
    assert_eq!(updates.receiver_count(), 0);
}
