mod common;

use bitcoind::anyhow::Context;
use btc_integration::protocol::{MintingVoteData, validate_transaction_votes};
use common::{create_connection, setup};
use corepc_client::bitcoin::Amount;
use flamechain::BlockHash;

#[tokio::test(flavor = "multi_thread")]
async fn minting_sender_publishes_an_authenticated_vote_on_regtest() -> bitcoind::anyhow::Result<()>
{
    let ctx = setup()?;
    let connection = create_connection(&ctx).await?;
    let sender = connection.get_sender();
    let flame_block_hash = BlockHash::from([0x42; 32]);

    let vote_txid = sender.send_vote(1234, flame_block_hash).await?;
    let confirmation_block = ctx.generate_next_block()?;
    let confirmed = ctx
        .rpc
        .transactions_with_prevouts_in_block(confirmation_block)
        .await?
        .into_iter()
        .find(|entry| entry.transaction.compute_txid() == vote_txid)
        .context("signed Minter vote was not included in the next block")?;
    let votes = validate_transaction_votes(&confirmed)?;

    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].txid(), vote_txid);
    let vote_output_index = votes[0].output().output_index as usize;
    assert_eq!(
        confirmed.transaction.output[vote_output_index].value,
        Amount::ZERO
    );
    assert_eq!(
        votes[0].output().data,
        MintingVoteData::V1 {
            flame_block_height: 1234,
            flame_block_hash,
        }
    );

    Ok(())
}
