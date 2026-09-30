mod common;

use bitcoind::anyhow::Context;
use btc_integration::btc::rpc::BtcBlockTip;
use btc_integration::protocol::Acquisition;
use common::{create_connection, setup};
use corepc_client::bitcoin::Amount;
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

    let confirmation_block = ctx.generate_next_block()?;
    let confirmed = rpc
        .transactions_with_prevouts_in_block(confirmation_block)
        .await?
        .into_iter()
        .find(|entry| entry.transaction.compute_txid() == expected_txid)
        .context("published transaction was not included in the next block")?;
    assert_eq!(confirmed.prevouts.len(), confirmed.transaction.input.len());
    assert!(confirmed.prevouts.iter().all(Option::is_some));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn acquisition_sender_works_with_core31_rpc_api() -> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let connection = create_connection(&ctx).await?;
    let sender = connection.get_sender();
    let sent_amount = Amount::from_sat(25_000);

    assert!(matches!(
        sender.send_acquisition(Amount::ZERO).await,
        Err(btc_integration::MintingSendError::ZeroAcquisitionAmount)
    ));

    let txid = sender.send_acquisition(sent_amount).await?;
    let confirmation_block = ctx.generate_next_block()?;
    let confirmed_transactions = ctx.rpc.transactions_in_block(confirmation_block).await?;
    let confirmed_transaction = confirmed_transactions
        .iter()
        .find(|transaction| transaction.compute_txid() == txid)
        .context("Acquisition transaction was not included in the next block")?;
    let acquisitions = Acquisition::from_tx(confirmed_transaction);

    assert_eq!(acquisitions.len(), 1);
    assert_eq!(acquisitions[0].txid(), txid);
    assert_eq!(acquisitions[0].amount(), sent_amount);
    assert_eq!(acquisitions[0].data().minter_p2wsh, sender.minter().p2wsh());
    assert_eq!(
        acquisitions[0].data().access_predicate.to_point(),
        flamevm::Predicate::unspendable_key(),
    );
    assert_eq!(
        acquisitions[0].data().validator_pubkey,
        ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    );

    Ok(())
}
