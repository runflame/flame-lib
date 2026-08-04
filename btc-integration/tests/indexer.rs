use std::time::Duration;

use bitcoind::anyhow::Context;
use btc_integration::prelude::*;
use btc_integration::rpc::RpcApi;
use btc_integration::test::setup;
use corepc_client::bitcoin::Amount;

#[tokio::test(flavor = "multi_thread")]
async fn indexer_bootstraps_and_follows_new_bitcoin_blocks() -> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let (sender, indexer) = create_mint_proof_components(BtcIntegrationConfig {
        btc_rpc_api: ctx.rpc_url.clone(),
        btc_rpc_auth: BitcoinRpcAuth::CookieFile(ctx.cookie_file.clone()),
        flame_network_id: 7,
    })?;

    let bootstrap_flame_hash = [0x11; 32];
    let bootstrap_amount = Amount::from_sat(10_000);
    sender
        .send_mint_proof(bootstrap_amount, &[], bootstrap_flame_hash, true)
        .await?;
    let bootstrap_block_hash = ctx.generate_next_block()?;
    let bootstrap_tip = ctx.rpc.block_tip().await?;
    assert_eq!(bootstrap_tip.hash, bootstrap_block_hash);

    indexer.startup().await?;

    let bootstrap_proof = MintingProof {
        minting_proof_data: MintingProofData {
            network_id: 7,
            flame_block_hash: bootstrap_flame_hash,
            want_participate_in_consensus: true,
        },
        burned_amount: bootstrap_amount,
        bitcoin_block_tip: bootstrap_tip,
    };
    assert_eq!(
        indexer.get_proofs(bootstrap_flame_hash).await,
        vec![bootstrap_proof]
    );

    let mut notifications = indexer.subscribe();
    let live_flame_hash = [0x22; 32];
    let live_amount = Amount::from_sat(20_000);
    sender
        .send_mint_proof(live_amount, &[], live_flame_hash, false)
        .await?;
    let live_block_hash = ctx.generate_next_block()?;
    let live_tip = ctx.rpc.block_tip().await?;
    assert_eq!(live_tip.hash, live_block_hash);

    let notification = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .context("timed out waiting for the indexed mint proof")??;
    let live_proof = MintingProof {
        minting_proof_data: MintingProofData {
            network_id: 7,
            flame_block_hash: live_flame_hash,
            want_participate_in_consensus: false,
        },
        burned_amount: live_amount,
        bitcoin_block_tip: live_tip,
    };

    assert_eq!(
        notification
            .get(&live_flame_hash)
            .context("notification did not contain the live mint proof")?,
        &vec![live_proof.clone()]
    );
    assert_eq!(indexer.get_proofs(live_flame_hash).await, vec![live_proof]);

    indexer.shutdown().await?;
    drop(sender);
    drop(indexer);
    drop(ctx);

    Ok(())
}
