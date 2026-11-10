//! The overlay as `btc-integration` sees it: connected, indexed and reorged over HTTP.

use std::{num::NonZeroUsize, sync::Arc};

use btc_integration::{
    AcquisitionData, BitcoinConfig, BitcoinConnectionError, BitcoinNetwork, BitcoinRpcAuth,
    BtcBlockTip, HistoryChange, MinterP2wsh,
    btc::rpc::{ChainSource, Core31RpcApi, SenderRpc},
    connect_indexer,
};
use btc_overlay::{Overlay, headers::MemoryHeaders, rpc, state::State};
use corepc_client::bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, absolute,
    hashes::Hash, transaction,
};
use flamevm::Predicate;

struct Served {
    headers: Arc<MemoryHeaders>,
    overlay: Arc<Overlay>,
    url: String,
}

async fn serve() -> Served {
    let headers = Arc::new(MemoryHeaders::new(30));
    let overlay = Arc::new(Overlay::new(
        Arc::clone(&headers) as _,
        State::in_memory(),
        None,
    ));
    overlay.sync().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = rpc::router(Arc::clone(&overlay));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Served {
        headers,
        overlay,
        url,
    }
}

fn config(url: &str, network: BitcoinNetwork) -> BitcoinConfig {
    BitcoinConfig {
        node_rpc_url: url.into(),
        // The overlay accepts any credentials.
        auth: BitcoinRpcAuth::UserPass("flame".into(), "overlay".into()),
        network,
    }
}

fn rpc(url: &str) -> Core31RpcApi {
    Core31RpcApi::new(
        url,
        BitcoinRpcAuth::UserPass("flame".into(), "overlay".into()),
    )
    .unwrap()
}

fn acquisition(amount: Amount) -> Transaction {
    let data = AcquisitionData::new(
        MinterP2wsh::from([7; 32]),
        Predicate::opaque(Predicate::unspendable_key()),
    );
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([1; 32]), 0),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: amount,
            script_pubkey: data.to_script(),
        }],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_an_overlay_node_accepts_the_overlay() {
    let served = serve().await;
    connect_indexer(&config(&served.url, BitcoinNetwork::Overlay))
        .await
        .unwrap();
    let error = connect_indexer(&config(&served.url, BitcoinNetwork::Mainnet))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            &error,
            BitcoinConnectionError::UnexpectedNetwork { actual, .. } if actual == "overlay"
        ),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_the_headers_and_twenty_blocks_of_history() {
    let served = serve().await;
    let rpc = rpc(&served.url);
    let tip = rpc.best_block_tip().await.unwrap();
    assert_eq!(tip.height, 29);

    let header = rpc.block_header_info(tip.hash).await.unwrap();
    assert_eq!(header.tip, tip);
    assert_eq!(
        rpc.block_hash_at_height(28).await.unwrap(),
        header.previous_block_hash.unwrap()
    );
    assert!(rpc.block_hash_at_height(30).await.is_err());
    assert!(rpc.block_hash_at_height(9).await.is_ok());
    assert!(rpc.block_hash_at_height(8).await.is_err());

    let block = rpc
        .transactions_with_prevouts_in_block(tip.hash)
        .await
        .unwrap();
    assert_eq!(block.len(), 1, "coinbase only");
    assert!(block[0].transaction.is_coinbase());
    assert_eq!(rpc.transactions_in_block(tip.hash).await.unwrap().len(), 1);

    // A long poll returns the unchanged tip on timeout, and the new one once it arrives.
    assert_eq!(rpc.wait_for_new_block(tip.hash, 50).await.unwrap(), tip);
    served.headers.mine(0);
    served.overlay.sync().await.unwrap();
    assert_eq!(
        rpc.wait_for_new_block(tip.hash, 0).await.unwrap().height,
        30
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_injected_burn_is_indexed_and_follows_a_reorg() {
    let served = serve().await;
    let rpc = rpc(&served.url);
    let indexer = connect_indexer(&config(&served.url, BitcoinNetwork::Overlay))
        .await
        .unwrap();
    indexer.startup().await.unwrap();
    let cursor = rpc.best_block_tip().await.unwrap();

    let burn = acquisition(Amount::from_sat(50_000));
    let refused = rpc.send_raw_transaction(&burn, None).await;
    assert!(refused.is_err(), "a burn needs an allowance");
    let txid = rpc
        .send_raw_transaction(&burn, Some(Amount::from_sat(50_000)))
        .await
        .unwrap();

    served.headers.mine(0);
    served.overlay.sync().await.unwrap();
    let update = history(&indexer, cursor).await;
    let blocks = update.get_indexed_blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].acquisitions.len(), 1);
    assert_eq!(blocks[0].acquisitions[0].txid(), txid);
    let first = blocks[0].btc_block_tip;

    // The block is replaced by a longer branch; the burn moves into the branch's first block.
    served.headers.mine(1);
    served.headers.mine(0);
    served.overlay.sync().await.unwrap();
    let update = history(&indexer, first).await;
    match &update.change {
        HistoryChange::Reorg {
            removed_block_tips,
            new_blocks,
        } => {
            assert_eq!(removed_block_tips, &vec![first]);
            assert_eq!(new_blocks.len(), 2);
            assert_eq!(new_blocks[0].acquisitions[0].txid(), txid);
            assert!(new_blocks[1].acquisitions.is_empty());
        }
        change => panic!("expected a reorg, got {change:?}"),
    }

    // The orphaned block still serves what it served before.
    let orphaned = rpc
        .transactions_with_prevouts_in_block(first.hash)
        .await
        .unwrap();
    assert_eq!(orphaned[1].transaction.compute_txid(), txid);
    indexer.shutdown().await.unwrap();
}

/// Waits until the indexer has seen the overlay's tip, then reads history from `cursor`.
async fn history(
    indexer: &btc_integration::ProtocolIndexer<Core31RpcApi>,
    cursor: BtcBlockTip,
) -> btc_integration::HistoryUpdate {
    let mut tips = indexer.subscribe();
    let read = async {
        loop {
            let update = indexer
                .get_history(cursor, NonZeroUsize::new(10).unwrap())
                .await;
            match update {
                Ok(update) if update.next_cursor != cursor => return update,
                _ => {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), tips.changed())
                        .await;
                }
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), read)
        .await
        .expect("the indexer reaches the overlay tip")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_minter_endorses_through_an_always_funded_wallet() {
    use btc_integration::{
        BitcoinConnection, BlockHash, IdentityConfig, IdentityManager,
        identity::storage::InMemorySecretStorage,
    };

    let served = serve().await;
    let storage = Arc::new(InMemorySecretStorage::default());
    let identity = IdentityManager::new(
        storage.clone(),
        IdentityConfig {
            flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        },
    )
    .startup()
    .await
    .unwrap()
    .clone();
    // The same call a minter makes against regtest; only the URL and network differ.
    let connection = BitcoinConnection::endorsements(
        config(&served.url, BitcoinNetwork::Overlay),
        identity.clone(),
        storage,
    )
    .await
    .unwrap();
    let sender = connection.get_sender();
    let indexer = connection.create_indexer();
    indexer.startup().await.unwrap();
    let cursor = rpc(&served.url).best_block_tip().await.unwrap();

    // More endorsements within one Bitcoin block than the wallet's floor of coins.
    for height in 0..25 {
        sender
            .send_endorsement(height, BlockHash::from([height as u8; 32]))
            .await
            .unwrap();
    }
    served.headers.mine(0);
    served.overlay.sync().await.unwrap();
    let update = history(&indexer, cursor).await;
    let block = &update.get_indexed_blocks()[0];
    assert_eq!(block.endorsements.len(), 25);
    for endorsement in &block.endorsements {
        assert_eq!(endorsement.auth().minter(), &identity);
    }

    // Later endorsements spend change and minted coins alike.
    sender
        .send_endorsement(25, BlockHash::from([25; 32]))
        .await
        .unwrap();
    served.headers.mine(0);
    served.overlay.sync().await.unwrap();
    let update = history(&indexer, update.next_cursor).await;
    assert_eq!(update.get_indexed_blocks()[0].endorsements.len(), 1);
    indexer.shutdown().await.unwrap();
}
