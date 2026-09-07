mod common;

use std::{num::NonZeroUsize, time::Duration};

use bitcoind::anyhow::Context;
use btc_integration::protocol::HistoryChange;
use common::{create_connection, setup};
use corepc_client::bitcoin::Amount;
use flamechain::BlockHash;

#[tokio::test(flavor = "multi_thread")]
async fn indexer_binds_acquisitions_and_votes_to_their_bitcoin_block()
-> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let connection = create_connection(&ctx).await?;
    let sender = connection.get_sender();
    let indexer = connection.create_indexer();

    let mut events = indexer.subscribe();
    indexer.startup().await?;
    let cursor = events.borrow_and_update().expect("running indexer");

    let acquisition_txid = sender.send_acquisition(Amount::from_sat(20_000)).await?;
    let vote_txid = sender.send_vote(777, BlockHash::from([0x77; 32])).await?;
    let block_hash = ctx.generate_next_block()?;
    let block_tip = ctx.rpc.best_block_tip().await?;
    assert_eq!(block_tip.hash, block_hash);

    tokio::time::timeout(Duration::from_secs(5), events.changed())
        .await
        .context("timed out waiting for the Bitcoin tip")??;
    let update = indexer
        .get_history(cursor, NonZeroUsize::new(10).unwrap())
        .await?;
    let HistoryChange::Extension { new_blocks } = update.change else {
        bitcoind::anyhow::bail!("expected an extension");
    };
    assert_eq!(update.next_cursor, block_tip);
    assert_eq!(new_blocks.len(), 1);
    let block = &new_blocks[0];

    assert_eq!(block.btc_block_tip, block_tip);
    assert_eq!(block.acquisitions.len(), 1);
    assert_eq!(block.acquisitions[0].txid(), acquisition_txid);
    assert_eq!(block.votes.len(), 1);
    assert_eq!(block.votes[0].txid(), vote_txid);

    indexer.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn history_returns_real_bitcoin_rollback_and_replacement_branch()
-> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let connection = create_connection(&ctx).await?;
    let ancestor = ctx.rpc.best_block_tip().await?;
    let first_hash = ctx.generate_next_block()?;
    let first = ctx.rpc.best_block_tip().await?;
    ctx.generate_next_block()?;
    let old_tip = ctx.rpc.best_block_tip().await?;
    let indexer = connection.create_indexer();
    indexer.startup().await?;

    ctx.invalidate_block(first_hash)?;
    let rollback = indexer
        .get_history(old_tip, NonZeroUsize::new(1).unwrap())
        .await?;
    assert_eq!(rollback.next_cursor, ancestor);
    assert!(
        matches!(rollback.change, HistoryChange::Reorg { removed_block_tips, new_blocks }
        if removed_block_tips == vec![old_tip, first] && new_blocks.is_empty())
    );

    // A different coinbase destination guarantees a new block rather than regenerating
    // the exact invalidated block when both branches are mined within the same second.
    let replacement_address = ctx.node.client.new_address()?;
    ctx.node
        .client
        .generate_to_address(1, &replacement_address)?;
    let replacement = ctx.rpc.best_block_tip().await?;
    assert_ne!(replacement.hash, first_hash);
    ctx.generate_next_block()?;
    let target = ctx.rpc.best_block_tip().await?;
    let update = indexer
        .get_history(old_tip, NonZeroUsize::new(1).unwrap())
        .await?;
    assert_eq!(update.target_tip, target);
    assert_eq!(update.next_cursor, replacement);
    assert!(
        matches!(update.change, HistoryChange::Reorg { removed_block_tips, new_blocks }
        if removed_block_tips == vec![old_tip, first]
            && new_blocks.len() == 1 && new_blocks[0].btc_block_tip == replacement)
    );
    let next = indexer
        .get_history(update.next_cursor, NonZeroUsize::new(1).unwrap())
        .await?;
    assert_eq!(next.next_cursor, target);
    assert!(
        matches!(next.change, HistoryChange::Extension { new_blocks }
        if new_blocks.len() == 1 && new_blocks[0].btc_block_tip == target)
    );
    indexer.shutdown().await?;
    Ok(())
}
