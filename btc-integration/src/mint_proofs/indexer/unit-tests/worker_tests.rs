use std::{collections::BTreeMap, sync::Arc};

use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use flamechain::{BlockHash, FlameNetwork};

use super::{IndexerWorker, IndexerWorkerError};
use crate::mint_proofs::indexer::{
    bitcoin_chain_update_planner::BitcoinChainUpdatePlanner,
    indexer::MintingProofUpdate,
    test_support::{FakeRpc, TestChain, block_tip, proof, recv_update},
};
use crate::mint_proofs::minting_proof_storage::{InMemoryMintingProofStorage, MintingProofStorage};

type TestWorker = IndexerWorker<FakeRpc, InMemoryMintingProofStorage>;

fn worker(
    rpc: Arc<FakeRpc>,
    cancellation_token: CancellationToken,
) -> (
    TestWorker,
    Arc<InMemoryMintingProofStorage>,
    broadcast::Receiver<Arc<MintingProofUpdate>>,
) {
    let storage = Arc::new(InMemoryMintingProofStorage::new());
    let (updates, receiver) = broadcast::channel(16);
    (
        IndexerWorker::new(
            rpc,
            Arc::clone(&storage),
            FlameNetwork::Regtest,
            updates,
            cancellation_token,
        ),
        storage,
        receiver,
    )
}

#[tokio::test]
async fn bootstrap_indexes_latest_twenty_blocks_for_selected_network() {
    let oldest_tip = block_tip(6, 6);
    let current_tip = block_tip(25, 25);
    let excluded_tip = block_tip(5, 5);
    let oldest = proof([0x11; 32], 1_000, oldest_tip);
    let current = proof([0x11; 32], 2_000, current_tip);
    let excluded = proof([0x22; 32], 3_000, excluded_tip);
    let other_network = {
        let mut prf = proof([0x11; 32], 4_000, current_tip);
        prf.minting_proof_data.network = FlameNetwork::Testnet;
        prf
    };
    let chain = TestChain::linear(5, 25)
        .proofs(excluded_tip, &[excluded])
        .proofs(oldest_tip, std::slice::from_ref(&oldest))
        .proofs(current_tip, &[current.clone(), other_network]);
    let rpc = FakeRpc::new(chain, current_tip);
    let (worker, storage, mut updates) = worker(rpc, CancellationToken::new());

    assert_eq!(worker.bootstrap().await.unwrap(), current_tip);
    assert_eq!(
        storage.get(BlockHash::from([0x11; 32])).await,
        vec![oldest, current]
    );
    assert!(storage.get(BlockHash::from([0x22; 32])).await.is_empty());
    assert!(matches!(
        recv_update(&mut updates).await.as_ref(),
        MintingProofUpdate::NewBlocks(_)
    ));
}

#[tokio::test]
async fn extension_stores_and_groups_new_proofs_for_selected_network() {
    let initial_tip = block_tip(100, 100);
    let next_tip = block_tip(101, 101);
    let first = proof([0x11; 32], 1_000, next_tip);
    let second = proof([0x11; 32], 2_000, next_tip);
    let other = proof([0x22; 32], 3_000, next_tip);
    let other_network = {
        let mut prf = proof([0x11; 32], 4_000, next_tip);
        prf.minting_proof_data.network = FlameNetwork::Testnet;
        prf
    };
    let chain = TestChain::linear(81, 101).proofs(
        next_tip,
        &[first.clone(), second.clone(), other.clone(), other_network],
    );
    let rpc = FakeRpc::new(chain, initial_tip);
    let (worker, storage, mut updates) = worker(Arc::clone(&rpc), CancellationToken::new());
    worker.bootstrap().await.unwrap();
    let mut planner = BitcoinChainUpdatePlanner::new(initial_tip, Arc::clone(&rpc));

    rpc.set_best_tip(next_tip).await;
    assert_eq!(
        worker.handle_new_tip(next_tip, &mut planner).await.unwrap(),
        next_tip
    );

    assert_eq!(
        storage.get(BlockHash::from([0x11; 32])).await,
        vec![first.clone(), second.clone()]
    );
    assert_eq!(
        storage.get(BlockHash::from([0x22; 32])).await,
        vec![other.clone()]
    );
    assert_eq!(
        recv_update(&mut updates).await.as_ref(),
        &MintingProofUpdate::NewBlocks(BTreeMap::from([
            (BlockHash::from([0x11; 32]), vec![first, second]),
            (BlockHash::from([0x22; 32]), vec![other]),
        ]))
    );
}

#[tokio::test]
async fn reorg_replaces_discarded_proofs_atomically() {
    let ancestor = block_tip(0, 0);
    let old_tip = block_tip(11, 1);
    let replacement_tip = block_tip(21, 1);
    let new_tip = block_tip(22, 2);
    let discarded = proof([0x11; 32], 1_000, old_tip);
    let replacement = proof([0x11; 32], 2_000, replacement_tip);
    let additional = proof([0x22; 32], 3_000, new_tip);
    let chain = TestChain::new()
        .block(ancestor, None, &[])
        .block(
            old_tip,
            Some(ancestor.hash),
            std::slice::from_ref(&discarded),
        )
        .block(
            replacement_tip,
            Some(ancestor.hash),
            std::slice::from_ref(&replacement),
        )
        .block(
            new_tip,
            Some(replacement_tip.hash),
            std::slice::from_ref(&additional),
        );
    let rpc = FakeRpc::new(chain, old_tip);
    let (worker, storage, mut updates) = worker(Arc::clone(&rpc), CancellationToken::new());
    worker.bootstrap().await.unwrap();
    recv_update(&mut updates).await;
    let mut planner = BitcoinChainUpdatePlanner::new(old_tip, Arc::clone(&rpc));

    rpc.set_best_tip(new_tip).await;
    worker.handle_new_tip(new_tip, &mut planner).await.unwrap();

    assert_eq!(
        recv_update(&mut updates).await.as_ref(),
        &MintingProofUpdate::Reorg {
            deleted_proofs: BTreeMap::from([(old_tip.hash, vec![discarded])]),
            new_proofs: BTreeMap::from([
                (BlockHash::from([0x11; 32]), vec![replacement.clone()]),
                (BlockHash::from([0x22; 32]), vec![additional.clone()]),
            ]),
        }
    );
    assert_eq!(
        storage.get(BlockHash::from([0x11; 32])).await,
        vec![replacement]
    );
    assert_eq!(
        storage.get(BlockHash::from([0x22; 32])).await,
        vec![additional]
    );
}

#[tokio::test]
async fn reorg_is_replanned_when_best_tip_changes_during_fetch() {
    let ancestor = block_tip(0, 0);
    let old_tip = block_tip(11, 1);
    let announced_block = block_tip(21, 1);
    let announced_tip = block_tip(22, 2);
    let winning_block = block_tip(31, 1);
    let winning_middle = block_tip(32, 2);
    let winning_tip = block_tip(33, 3);
    let old_proof = proof([0x11; 32], 1_000, old_tip);
    let superseded = proof([0x11; 32], 2_000, announced_block);
    let winning = proof([0x11; 32], 3_000, winning_block);
    let additional = proof([0x22; 32], 4_000, winning_tip);
    let chain = TestChain::new()
        .block(ancestor, None, &[])
        .block(
            old_tip,
            Some(ancestor.hash),
            std::slice::from_ref(&old_proof),
        )
        .block(
            announced_block,
            Some(ancestor.hash),
            std::slice::from_ref(&superseded),
        )
        .block(announced_tip, Some(announced_block.hash), &[])
        .block(
            winning_block,
            Some(ancestor.hash),
            std::slice::from_ref(&winning),
        )
        .block(winning_middle, Some(winning_block.hash), &[])
        .block(
            winning_tip,
            Some(winning_middle.hash),
            std::slice::from_ref(&additional),
        );
    let rpc = FakeRpc::new(chain, old_tip);
    let blocker = rpc.block_transactions_for(announced_block.hash).await;
    let (worker, storage, mut updates) = worker(Arc::clone(&rpc), CancellationToken::new());
    worker.bootstrap().await.unwrap();
    recv_update(&mut updates).await;
    let mut planner = BitcoinChainUpdatePlanner::new(old_tip, Arc::clone(&rpc));
    rpc.set_best_tip(announced_tip).await;

    let update = worker.handle_new_tip(announced_tip, &mut planner);
    tokio::pin!(update);
    tokio::select! {
        () = blocker.wait_until_requested() => {}
        result = &mut update => panic!("update completed before the blocked request: {result:?}"),
    }
    rpc.set_best_tip(winning_tip).await;
    blocker.release();
    assert_eq!(update.await.unwrap(), winning_tip);

    assert_eq!(
        recv_update(&mut updates).await.as_ref(),
        &MintingProofUpdate::Reorg {
            deleted_proofs: BTreeMap::from([(old_tip.hash, vec![old_proof])]),
            new_proofs: BTreeMap::from([
                (BlockHash::from([0x11; 32]), vec![winning.clone()]),
                (BlockHash::from([0x22; 32]), vec![additional.clone()]),
            ]),
        }
    );
    assert_eq!(
        storage.get(BlockHash::from([0x11; 32])).await,
        vec![winning]
    );
    assert_eq!(
        storage.get(BlockHash::from([0x22; 32])).await,
        vec![additional]
    );
}

#[tokio::test]
async fn failed_reorg_fetch_preserves_the_previous_chain() {
    let ancestor = block_tip(0, 0);
    let old_tip = block_tip(11, 1);
    let new_tip = block_tip(21, 1);
    let old_proof = proof([0x11; 32], 1_000, old_tip);
    let replacement = proof([0x11; 32], 2_000, new_tip);
    let chain = TestChain::new()
        .block(ancestor, None, &[])
        .block(
            old_tip,
            Some(ancestor.hash),
            std::slice::from_ref(&old_proof),
        )
        .block(
            new_tip,
            Some(ancestor.hash),
            std::slice::from_ref(&replacement),
        );
    let rpc = FakeRpc::new(chain, old_tip);
    let (worker, storage, mut updates) = worker(Arc::clone(&rpc), CancellationToken::new());
    worker.bootstrap().await.unwrap();
    recv_update(&mut updates).await;
    let mut planner = BitcoinChainUpdatePlanner::new(old_tip, Arc::clone(&rpc));
    rpc.fail_transactions_for(new_tip.hash).await;
    rpc.set_best_tip(new_tip).await;

    let error = worker
        .handle_new_tip(new_tip, &mut planner)
        .await
        .expect_err("replacement block fetch must fail");

    assert!(matches!(error, IndexerWorkerError::RpcError(_)));
    assert_eq!(
        storage.get(BlockHash::from([0x11; 32])).await,
        vec![old_proof]
    );
}

#[tokio::test]
async fn cancellation_aborts_all_concurrent_bootstrap_fetches() {
    let genesis = block_tip(0, 0);
    let tip = block_tip(1, 1);
    let chain = TestChain::new()
        .block(genesis, None, &[])
        .block(tip, Some(genesis.hash), &[]);
    let rpc = FakeRpc::new(chain, tip);
    let genesis_blocker = rpc.block_transactions_for(genesis.hash).await;
    let tip_blocker = rpc.block_transactions_for(tip.hash).await;
    let cancellation_token = CancellationToken::new();
    let (worker, storage, _) = worker(rpc, cancellation_token.clone());

    let bootstrap = worker.bootstrap();
    tokio::pin!(bootstrap);
    tokio::select! {
        () = async {
            tokio::join!(
                genesis_blocker.wait_until_requested(),
                tip_blocker.wait_until_requested(),
            );
        } => {}
        result = &mut bootstrap => panic!("bootstrap completed before cancellation: {result:?}"),
    }
    cancellation_token.cancel();

    assert!(matches!(
        bootstrap.await,
        Err(IndexerWorkerError::Cancelled)
    ));
    assert!(storage.is_empty().await);
}

#[tokio::test]
async fn run_rebootstraps_after_a_failed_live_update() {
    let initial_tip = block_tip(0, 0);
    let next_tip = block_tip(1, 1);
    let indexed_proof = proof([0x11; 32], 1_000, next_tip);
    let chain = TestChain::new().block(initial_tip, None, &[]).block(
        next_tip,
        Some(initial_tip.hash),
        std::slice::from_ref(&indexed_proof),
    );
    let rpc = FakeRpc::new(chain, initial_tip);
    let cancellation_token = CancellationToken::new();
    let (worker, storage, mut updates) = worker(Arc::clone(&rpc), cancellation_token.clone());
    let (bootstrap_sender, bootstrap_result) = oneshot::channel();
    let run = tokio::spawn(worker.run(bootstrap_sender));

    bootstrap_result.await.expect("bootstrap succeeds");
    rpc.fail_transactions_for(next_tip.hash).await;
    rpc.set_best_tip(next_tip).await;
    rpc.announce(next_tip);
    rpc.wait_for_transaction_calls(next_tip.hash, 1).await;
    rpc.allow_transactions_for(next_tip.hash).await;

    let expected = BTreeMap::from([(BlockHash::from([0x11; 32]), vec![indexed_proof.clone()])]);
    assert_eq!(
        recv_update(&mut updates).await.as_ref(),
        &MintingProofUpdate::NewBlocks(expected)
    );
    assert_eq!(
        storage.get(BlockHash::from([0x11; 32])).await,
        vec![indexed_proof]
    );

    cancellation_token.cancel();
    run.await.expect("worker task did not panic");
}
