use std::convert::Infallible;

use bitcoin::Txid;
use flame_chain_service::InMemoryChain;

use super::*;
use crate::minter::journal::in_memory::InMemoryMinterJournal;

struct UnusedSender;

impl EndorsementSender for UnusedSender {
    type Error = Infallible;
    type TransactionId = Txid;
}

fn manager(
    endorsement_policy: EndorsementPolicy,
) -> MinterManager<InMemoryChain, UnusedSender, InMemoryMinterJournal> {
    MinterManager::new(
        Arc::new(InMemoryChain::default()),
        Arc::new(UnusedSender),
        Arc::new(InMemoryMinterJournal::new()),
        endorsement_policy,
    )
}

#[tokio::test]
async fn manual_policy_does_not_start_a_worker() {
    let manager = manager(EndorsementPolicy::Manual);
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
    let manager = manager(EndorsementPolicy::Auto);
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
