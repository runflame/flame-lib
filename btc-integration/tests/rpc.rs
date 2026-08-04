use bitcoind::anyhow::Context;
use btc_integration::mint_proofs::MintingProofData;
use btc_integration::prelude::*;
use btc_integration::rpc::RpcApi;
use btc_integration::test::setup;
use corepc_client::bitcoin::Amount;
use ed25519_dalek::SigningKey;
use flamevm::Predicate;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn core31_rpc_api_works_with_regtest_blocks_and_transactions() -> bitcoind::anyhow::Result<()>
{
    let ctx = setup()?;
    let rpc = Arc::clone(&ctx.rpc);

    let (block_hash, transactions_at_height) = rpc.transactions_at_height(1).await?;
    let transactions_by_hash = rpc.transactions_in_block(block_hash).await?;
    assert_eq!(transactions_at_height, transactions_by_hash);
    assert_eq!(transactions_at_height.len(), 1);

    let (previous_tip_hash, _) = rpc.transactions_at_height(101).await?;
    let previous_tip = BtcBlockTip {
        hash: previous_tip_hash,
        height: 101,
    };

    let waiter = ctx.wait_for_next_block(previous_tip);
    let next_hash = ctx.generate_next_block()?;

    let next_tip = waiter.await.expect("wait for next block");

    assert_eq!(next_tip.height, 102);
    assert_eq!(next_tip.hash, next_hash);

    let recipient = ctx.node.client.new_address()?;
    let amount = Amount::from_btc(1.0).expect("valid BTC amount");
    let signed_transaction = ctx.send_simple_transaction(recipient, amount)?;

    assert!(signed_transaction.complete);

    let expected_txid = signed_transaction.tx.compute_txid();
    let published_txid = rpc.publish_transaction(&signed_transaction.tx).await?;
    assert_eq!(published_txid, expected_txid);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mint_proof_sender_works_with_core31_rpc_api() -> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let rpc = Arc::clone(&ctx.rpc);
    let mint_proof_sender = MintProofSenderV31::new(Arc::clone(&rpc), 1);
    let sent_amount = Amount::from_btc(1.0).expect("valid BTC amount");
    let flame_block_hash = [0xab; 32];
    let flame_address = Predicate::opaque(Predicate::unspendable_key());
    let validator_pubkey = SigningKey::from_bytes(&[7; 32]).verifying_key();

    let txid = mint_proof_sender
        .send_mint_proof(
            sent_amount,
            &[],
            flame_block_hash,
            flame_address.clone(),
            Some(validator_pubkey),
        )
        .await?;
    let confirmation_block = ctx.generate_next_block()?;
    let confirmed_transactions = ctx.rpc.transactions_in_block(confirmation_block).await?;
    let confirmed_transaction = confirmed_transactions
        .iter()
        .find(|transaction| transaction.compute_txid() == txid)
        .context("mint-proof transaction was not included in the next block")?;
    let confirmed_mint_output = confirmed_transaction
        .output
        .iter()
        .find(|output| MintingProofData::from_tx_out(output).is_some())
        .context("confirmed transaction does not contain a mint-proof output")?;

    assert_eq!(confirmed_mint_output.value, sent_amount);
    assert_eq!(
        MintingProofData::from_tx_out(confirmed_mint_output),
        Some(MintingProofData {
            network_id: 1,
            flame_block_hash,
            flame_reward_address: flame_address,
            validator_pubkey: Some(validator_pubkey),
        })
    );

    Ok(())
}
