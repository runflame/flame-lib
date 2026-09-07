mod common;

use std::convert::Infallible;

use async_trait::async_trait;
use btc_integration::{
    BitcoinConfig, BitcoinConnection, BitcoinConnectionError, BitcoinRpcAuth, MinterIdentity,
    SecretStorage, TestAcquisitionConfig, TestSigner, btc::rpc::Core31RpcApi,
};
use corepc_client::bitcoin::{
    Address, Amount, Network, Script,
    secp256k1::{Secp256k1, SecretKey},
};
use corepc_client::client_sync::v31::Client;
use flamevm::Predicate;
use serde_json::{Value, json};

struct UnreadStorage;

#[async_trait]
impl SecretStorage for UnreadStorage {
    type Error = Infallible;

    async fn get_secret_key(&self) -> Result<SecretKey, Self::Error> {
        panic!("connection setup must not read the secret key");
    }
}

#[tokio::test]
async fn unsupported_authorization_is_rejected_before_connecting() {
    let identity = MinterIdentity::new(
        btc_integration::protocol::minter_witness_script::build_with_authorization(
            &Predicate::opaque(Predicate::unspendable_key()),
            Script::from_bytes(&[0x51]),
        ),
    )
    .unwrap();
    let result = BitcoinConnection::regtest(
        BitcoinConfig {
            node_rpc_url: "not a URL".into(),
            auth: BitcoinRpcAuth::None,
        },
        identity,
        UnreadStorage,
        TestAcquisitionConfig {
            wallet_rpc_url: "not a URL".into(),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(BitcoinConnectionError::UnsupportedAuthorization)
    ));
}

fn identity() -> MinterIdentity {
    let public_key = SecretKey::from_slice(&[0x41; 32])
        .unwrap()
        .public_key(&Secp256k1::new());
    MinterIdentity::single_key(
        &Predicate::opaque(Predicate::unspendable_key()),
        &public_key,
    )
}

async fn connect(
    ctx: &common::TestContext,
    identity: MinterIdentity,
) -> bitcoind::anyhow::Result<BitcoinConnection<TestSigner<Core31RpcApi, UnreadStorage>>> {
    Ok(BitcoinConnection::regtest(
        BitcoinConfig {
            node_rpc_url: format!("{}/", ctx.node.rpc_url()),
            auth: BitcoinRpcAuth::CookieFile(ctx.cookie_file.clone()),
        },
        identity,
        UnreadStorage,
        TestAcquisitionConfig {
            wallet_rpc_url: ctx.rpc_url.clone(),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        },
    )
    .await?)
}

fn wallet(
    ctx: &common::TestContext,
    identity: &MinterIdentity,
) -> bitcoind::anyhow::Result<(String, Client)> {
    let name = format!("flame-minter-{}", identity.witness_script().wscript_hash());
    let client = Client::new_with_auth(
        &ctx.node.rpc_url_with_wallet(&name),
        BitcoinRpcAuth::CookieFile(ctx.cookie_file.clone()),
    )?;
    Ok((name, client))
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_wallet_tracks_new_funding_and_survives_reconnect_and_reload()
-> bitcoind::anyhow::Result<()> {
    let ctx = common::setup()?;
    let identity = identity();
    let address = Address::p2wsh(identity.witness_script(), Network::Regtest);
    let first = connect(&ctx, identity.clone()).await?;
    let funded = ctx.send_simple_transaction(address.clone(), Amount::from_sat(20_000))?;
    let txid = ctx.rpc.publish_transaction(&funded.tx).await?;
    ctx.generate_next_block()?;
    let (name, wallet) = wallet(&ctx, &identity)?;
    let info: Value = wallet.call("getwalletinfo", &[])?;
    assert_eq!(info["private_keys_enabled"], false);
    assert_eq!(info["descriptors"], true);
    let unspent: Value = wallet.call("listunspent", &[])?;
    assert!(
        unspent
            .as_array()
            .unwrap()
            .iter()
            .any(|utxo| utxo["txid"] == txid.to_string())
    );
    let descriptors = wallet.list_descriptors()?;
    assert_eq!(descriptors.descriptors.len(), 1);
    assert!(descriptors.descriptors[0].timestamp > 1);

    let repeated = connect(&ctx, identity.clone()).await?;
    assert_eq!(first.get_sender().minter(), repeated.get_sender().minter());
    assert_eq!(wallet.list_descriptors()?, descriptors);
    let _: Value = wallet.call("unloadwallet", &[])?;
    let reloaded = connect(&ctx, identity.clone()).await?;
    assert_eq!(reloaded.get_sender().minter(), &identity);
    assert_eq!(wallet.list_descriptors()?, descriptors);
    let reloaded_unspent: Value = wallet.call("listunspent", &[])?;
    assert_eq!(reloaded_unspent, unspent);
    assert_eq!(
        ctx.node
            .client
            .list_wallets()?
            .0
            .iter()
            .filter(|wallet| *wallet == &name)
            .count(),
        1
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn wallet_setup_and_reconnect_do_not_scan_old_payments() -> bitcoind::anyhow::Result<()> {
    let ctx = common::setup()?;
    let identity = identity();
    let address = Address::p2wsh(identity.witness_script(), Network::Regtest);
    let funded = ctx.send_simple_transaction(address.clone(), Amount::from_sat(20_000))?;
    ctx.rpc.publish_transaction(&funded.tx).await?;
    ctx.generate_next_block()?;
    // Leave the payment outside Core's two-hour rescan safety window.
    let later = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        + 24 * 60 * 60;
    let _: Value = ctx.node.client.call("setmocktime", &[json!(later)])?;
    // Advance the median block time too: Core resolves "now" from the chain.
    for _ in 0..11 {
        ctx.generate_next_block()?;
    }
    connect(&ctx, identity.clone()).await?;
    let (_, wallet) = wallet(&ctx, &identity)?;
    let descriptors = wallet.list_descriptors()?;
    assert_eq!(descriptors.descriptors.len(), 1);
    assert!(descriptors.descriptors[0].timestamp >= later);
    let unspent: Value = wallet.call("listunspent", &[])?;
    assert!(unspent.as_array().unwrap().is_empty());
    connect(&ctx, identity.clone()).await?;
    let unspent: Value = wallet.call("listunspent", &[])?;
    assert!(unspent.as_array().unwrap().is_empty());
    assert_eq!(wallet.list_descriptors()?, descriptors);
    let _: Value = wallet.call("unloadwallet", &[])?;
    connect(&ctx, identity).await?;
    let unspent: Value = wallet.call("listunspent", &[])?;
    assert!(unspent.as_array().unwrap().is_empty());
    assert_eq!(wallet.list_descriptors()?, descriptors);
    Ok(())
}
