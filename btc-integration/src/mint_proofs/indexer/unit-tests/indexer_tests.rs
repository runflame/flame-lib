use std::sync::Arc;

use crate::mint_proofs::{
    indexer::{
        indexer::MintingProofUpdate,
        test_support::{FakeRpc, TestChain, block_tip, recv_update},
    },
    minting_proof_storage::InMemoryMintingProofStorage,
};

use super::{MintProofIndexer, StartupError};

fn indexer(rpc: Arc<FakeRpc>) -> Arc<MintProofIndexer<FakeRpc, InMemoryMintingProofStorage>> {
    Arc::new(MintProofIndexer::new(
        rpc,
        Arc::new(InMemoryMintingProofStorage::new()),
        7,
    ))
}

fn genesis_rpc() -> Arc<FakeRpc> {
    let genesis = block_tip(0, 0);
    FakeRpc::new(TestChain::new().block(genesis, None, &[]), genesis)
}

#[tokio::test]
async fn duplicate_startup_is_rejected() {
    let indexer = indexer(genesis_rpc());
    indexer.startup().await.expect("start indexer");

    let error = indexer
        .startup()
        .await
        .expect_err("duplicate startup must fail");

    assert!(matches!(error, StartupError::AlreadyRunningError));
    indexer.shutdown().await.expect("shut down indexer");
}

#[tokio::test]
async fn shutdown_is_idempotent() {
    let indexer = indexer(genesis_rpc());
    indexer.startup().await.expect("start indexer");

    indexer.shutdown().await.expect("first shutdown");
    indexer.shutdown().await.expect("second shutdown");
}

#[tokio::test]
async fn startup_launches_the_background_worker() {
    let genesis = block_tip(0, 0);
    let next_tip = block_tip(1, 1);
    let rpc = FakeRpc::new(TestChain::linear(0, 1), genesis);
    let indexer = indexer(Arc::clone(&rpc));
    let mut updates = indexer.subscribe();
    indexer.startup().await.expect("start indexer");

    rpc.set_best_tip(next_tip).await;
    rpc.announce(next_tip);

    assert_eq!(
        recv_update(&mut updates).await.as_ref(),
        &MintingProofUpdate::NewBlocks(Default::default())
    );
    indexer.shutdown().await.expect("shut down indexer");
}

#[tokio::test]
async fn indexer_can_restart_after_shutdown() {
    let indexer = indexer(genesis_rpc());

    indexer.startup().await.expect("first startup");
    indexer.shutdown().await.expect("first shutdown");
    indexer.startup().await.expect("second startup");
    indexer.shutdown().await.expect("second shutdown");
}

#[tokio::test]
async fn startup_waits_until_bootstrap_completes() {
    let genesis = block_tip(0, 0);
    let rpc = FakeRpc::new(TestChain::new().block(genesis, None, &[]), genesis);
    let blocker = rpc.block_transactions_for(genesis.hash).await;
    let indexer = indexer(rpc);
    let startup_indexer = Arc::clone(&indexer);
    let startup = tokio::spawn(async move { startup_indexer.startup().await });

    blocker.wait_until_requested().await;
    assert!(!startup.is_finished());
    blocker.release();

    startup
        .await
        .expect("startup task did not panic")
        .expect("bootstrap succeeds");
    indexer.shutdown().await.expect("shut down indexer");
}

#[tokio::test]
async fn failed_bootstrap_is_retried_until_it_succeeds() {
    let genesis = block_tip(0, 0);
    let rpc = FakeRpc::new(TestChain::new().block(genesis, None, &[]), genesis);
    rpc.fail_transactions_for(genesis.hash).await;
    let indexer = indexer(Arc::clone(&rpc));
    let startup_indexer = Arc::clone(&indexer);
    let startup = tokio::spawn(async move { startup_indexer.startup().await });

    rpc.wait_for_transaction_calls(genesis.hash, 1).await;
    assert!(!startup.is_finished());
    rpc.allow_transactions_for(genesis.hash).await;
    rpc.wait_for_transaction_calls(genesis.hash, 2).await;

    tokio::time::timeout(std::time::Duration::from_secs(3), startup)
        .await
        .expect("bootstrap retry timeout")
        .expect("startup task did not panic")
        .expect("bootstrap retry succeeds");
    indexer.shutdown().await.expect("shut down indexer");
}

#[tokio::test]
async fn shutdown_waits_for_cancelled_startup() {
    let genesis = block_tip(0, 0);
    let rpc = FakeRpc::new(TestChain::new().block(genesis, None, &[]), genesis);
    let blocker = rpc.block_transactions_for(genesis.hash).await;
    let indexer = indexer(rpc);
    let startup_indexer = Arc::clone(&indexer);
    let startup = tokio::spawn(async move { startup_indexer.startup().await });

    blocker.wait_until_requested().await;
    indexer.shutdown().await.expect("cancel startup");

    let error = startup
        .await
        .expect("startup task did not panic")
        .expect_err("startup must report cancellation");
    assert!(matches!(error, StartupError::WorkerStopped));
}
