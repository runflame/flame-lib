//! The watch-only wallet of a minter in a real Bitcoin Core.

mod common;

use bitcoin::secp256k1::SecretKey;
use bitcoin::{Address, Amount, Network};
use bitcoind::client::{Auth, v31::Client};
use btc_integration::identity::impls::LocalKey;
use btc_integration::wallet::impls::RpcWatchOnlyWallet;
use btc_integration::{BitcoinConfig, BitcoinRpcAuth, IdentityConfig, MinterIdentity};
use flamevm::Predicate;
use serde_json::Value;

fn identity() -> MinterIdentity {
    let key = LocalKey::new(SecretKey::from_slice(&[0x41; 32]).unwrap());
    let nobody = Predicate::opaque(Predicate::unspendable_key());
    IdentityConfig {
        flame_predicate: nobody.clone(),
        access_predicate: nobody,
        validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    }
    .identity(&key)
}

/// Opens the wallet of the minter, the way a minter does at its start.
async fn open(
    ctx: &common::TestContext,
    identity: &MinterIdentity,
) -> bitcoind::anyhow::Result<RpcWatchOnlyWallet> {
    let config = BitcoinConfig {
        node_rpc_url: format!("{}/", ctx.node.rpc_url()),
        ..ctx.bitcoin_config()
    };
    Ok(RpcWatchOnlyWallet::open(&config, identity.witness_script()).await?)
}

fn wallet(
    ctx: &common::TestContext,
    identity: &MinterIdentity,
) -> bitcoind::anyhow::Result<(String, Client)> {
    let name = format!("flame-minter-{}", identity.witness_script().wscript_hash());
    let client = Client::new_with_auth(
        &ctx.node.rpc_url_with_wallet(&name),
        Auth::CookieFile(ctx.cookie_file.clone()),
    )?;
    Ok((name, client))
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_wallet_tracks_new_funding_and_survives_reconnect_and_reload()
-> bitcoind::anyhow::Result<()> {
    let ctx = common::setup()?;
    let identity = identity();
    let address = Address::p2wsh(identity.witness_script(), Network::Regtest);
    open(&ctx, &identity).await?;
    let txid = ctx.pay(address, Amount::from_sat(20_000)).await?;
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

    open(&ctx, &identity).await?;
    assert_eq!(wallet.list_descriptors()?, descriptors);
    let _: Value = wallet.call("unloadwallet", &[])?;
    open(&ctx, &identity).await?;
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
async fn managed_wallet_is_loaded_again_after_bitcoin_core_restarts() -> bitcoind::anyhow::Result<()>
{
    // A data directory that outlives the first node.
    let datadir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("managed-wallet-restart");
    let _ = std::fs::remove_dir_all(&datadir);
    let mut conf = bitcoind::Conf::default();
    conf.staticdir = Some(datadir);
    let mut node = bitcoind::BitcoinD::from_downloaded_with_conf(&conf)?;
    let identity = identity();
    let config = BitcoinConfig {
        node_rpc_url: node.rpc_url(),
        auth: BitcoinRpcAuth::CookieFile(node.params.cookie_file.clone()),
    };
    RpcWatchOnlyWallet::open(&config, identity.witness_script()).await?;
    let name = format!("flame-minter-{}", identity.witness_script().wscript_hash());
    assert!(node.client.list_wallets()?.0.contains(&name));

    node.stop()?;
    let node = bitcoind::BitcoinD::from_downloaded_with_conf(&conf)?;

    // Nobody connected for the minter yet: Bitcoin Core loaded the wallet
    // itself, so a minter that was running through the restart goes on.
    assert!(node.client.list_wallets()?.0.contains(&name));
    Ok(())
}
