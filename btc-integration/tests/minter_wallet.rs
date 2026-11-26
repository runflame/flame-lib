//! The wallet of a minter on a real bitcoind in regtest.

mod common;

use std::sync::Arc;

use bitcoin::{Address, Amount, Network};
use bitcoind::anyhow::Context;
use btc_integration::identity::impls::LocalKey;
use btc_integration::wallet::impls::RpcWatchOnlyWallet;
use btc_integration::{
    BtcBlockRecords, BtcWallet, IdentityConfig, MinterWallet, MintingEndorsementData,
};
use flamechain::BlockHash;
use flamevm::Predicate;

#[tokio::test(flavor = "multi_thread")]
async fn a_new_key_endorses_and_a_second_wallet_with_the_same_key_goes_on()
-> bitcoind::anyhow::Result<()> {
    let ctx = common::setup()?;
    let key = Arc::new(LocalKey::generate());
    let minter = IdentityConfig {
        flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
        access_predicate: Predicate::opaque(Predicate::unspendable_key()),
        validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    }
    .identity(&key);
    let script = minter.witness_script();
    let config = ctx.bitcoin_config();
    let watch_only = RpcWatchOnlyWallet::open(&config, script).await?;
    let wallet = MinterWallet::new(watch_only, key.clone(), minter.clone())?;

    ctx.pay(
        Address::p2wsh(script, Network::Regtest),
        Amount::from_sat(50_000),
    )
    .await?;
    let endorsement = MintingEndorsementData::V1 {
        flame_block_height: 1234,
        flame_block_hash: BlockHash::from([0x42; 32]),
    };
    let txid = wallet.send(endorsement.to_output()).await?;
    let confirmation = ctx.generate_next_block()?;
    let (block, prevouts) = ctx.block(confirmation).await?;
    let endorsements = BtcBlockRecords::from_block(confirmation, &block, &prevouts)?.endorsements;
    endorsements
        .iter()
        .find(|endorsement| endorsement.txid() == txid)
        .context("endorsement was not confirmed")?;
    assert_eq!(endorsements.len(), 1);
    assert_eq!(endorsements[0].auth().minter(), &minter);
    assert_eq!(endorsements[0].output().data, endorsement);

    // The minter starts again: the same key, the same wallet of Bitcoin Core.
    let watch_only = RpcWatchOnlyWallet::open(&config, script).await?;
    let again = MinterWallet::new(watch_only, key, minter.clone())?;
    let next = MintingEndorsementData::V1 {
        flame_block_height: 1235,
        flame_block_hash: BlockHash::from([0x43; 32]),
    };
    assert_ne!(again.send(next.to_output()).await?, txid);
    Ok(())
}
