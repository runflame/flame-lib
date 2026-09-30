#[path = "common/test_context.rs"]
#[allow(dead_code)]
mod test_context;

use std::sync::Arc;

use bitcoind::anyhow::Context;
use btc_integration::{
    BitcoinConfig, BitcoinConnection, BitcoinRpcAuth, IdentityConfig, IdentityManager,
    MintingVoteData, SecretStorage, identity::storage::InMemorySecretStorage,
    protocol::validate_transaction_votes,
};
use corepc_client::bitcoin::{Address, Amount, Network};
use flamechain::BlockHash;
use flamevm::Predicate;

#[tokio::test(flavor = "multi_thread")]
async fn generated_identity_signs_votes_and_reconnects_with_shared_storage()
-> bitcoind::anyhow::Result<()> {
    let ctx = test_context::setup()?;
    let storage = Arc::new(InMemorySecretStorage::default());
    let manager = IdentityManager::new(
        storage.clone(),
        IdentityConfig {
            flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        },
    );
    let identity = manager.startup().await?.clone();
    let config = BitcoinConfig {
        node_rpc_url: ctx.node.rpc_url(),
        auth: BitcoinRpcAuth::CookieFile(ctx.cookie_file.clone()),
    };
    let connection =
        BitcoinConnection::votes(config.clone(), identity.clone(), storage.clone()).await?;
    let sender = connection.get_sender();
    assert_eq!(sender.minter(), &identity);

    let address = Address::p2wsh(identity.witness_script(), Network::Regtest);
    let funding = ctx.send_simple_transaction(address, Amount::from_sat(50_000))?;
    ctx.rpc.publish_transaction(&funding.tx).await?;
    ctx.generate_next_block()?;
    let hash = BlockHash::from([0x42; 32]);
    let txid = sender.send_vote(1234, hash).await?;
    let confirmation = ctx.generate_next_block()?;
    let transaction = ctx
        .rpc
        .transactions_with_prevouts_in_block(confirmation)
        .await?
        .into_iter()
        .find(|entry| entry.transaction.compute_txid() == txid)
        .context("vote was not confirmed")?;
    let votes = validate_transaction_votes(&transaction)?;
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].auth().minter(), &identity);
    assert_eq!(
        votes[0].output().data,
        MintingVoteData::V1 {
            flame_block_height: 1234,
            flame_block_hash: hash,
        }
    );

    let persisted = storage.get_secret_key().await?;
    let reconnected = BitcoinConnection::votes(config, identity.clone(), storage.clone()).await?;
    assert_eq!(reconnected.get_sender().minter(), &identity);
    assert_eq!(storage.get_secret_key().await?, persisted);
    let second_txid = reconnected
        .get_sender()
        .send_vote(1235, BlockHash::from([0x43; 32]))
        .await?;
    assert_ne!(second_txid, txid);
    let indexer = reconnected.create_indexer();
    indexer.startup().await?;
    assert!(indexer.subscribe().borrow().is_some());
    indexer.shutdown().await?;
    Ok(())
}
