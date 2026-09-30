use std::{
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use demo_consensus_backend::{DemoSession, types::*};
use tokio::time::sleep;

async fn mine(session: &mut DemoSession) -> Result<BitcoinBlockTip> {
    let blocks = session
        .create_bitcoin_blocks(CreateBitcoinBlocksRequest {
            count: NonZeroU32::new(1).unwrap(),
        })
        .await?;
    let tip = blocks.into_iter().next().context("missing mined block")?;
    ensure!(
        session.consensus_cursor().await? == tip,
        "mining returned before consensus caught up"
    );
    ensure!(
        session.bitcoin_tip().await? == tip,
        "unexpected Bitcoin tip"
    );
    Ok(tip)
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_seeds_alice_and_a_confirmed_vote_for_core_genesis() -> Result<()> {
    let mut session = DemoSession::start().await?;
    let result: Result<()> = async {
        let snapshot = session.snapshot().await?;
        ensure!(snapshot.minters.len() == 1, "unexpected initial minters");
        let alice = &snapshot.minters[0];
        ensure!(
            alice.id == MinterId(0) && alice.name == "Alice",
            "missing Alice"
        );
        ensure!(!alice.automatic_voting, "Alice is not manually controlled");
        ensure!(
            snapshot.flame.blocks.len() == 1,
            "unexpected initial blocks"
        );
        let genesis = &snapshot.flame.blocks[0];
        let core = genesis.core.as_ref().context("genesis is not core")?;
        ensure!(
            core.height == 1 && genesis.parent_hash.is_none(),
            "wrong core genesis"
        );
        ensure!(
            genesis.is_canonical
                && snapshot.flame.canonical_tip.as_ref() == Some(&genesis.tip)
                && snapshot.consensus.heaviest_tip.as_ref() == Some(&genesis.tip),
            "genesis is not the selected tip"
        );
        ensure!(
            snapshot.acquisitions.len() == 1,
            "missing initial acquisition"
        );
        let acquisition = &snapshot.acquisitions[0];
        ensure!(
            acquisition.minter_id == alice.id && acquisition.amount_sats == 20_000,
            "wrong initial acquisition"
        );
        let TransactionStatus::Confirmed {
            block: acquisition_block,
        } = &acquisition.transaction_status
        else {
            anyhow::bail!("initial acquisition is unconfirmed");
        };
        ensure!(
            u64::from(core.target_btc_height)
                == acquisition_block.height
                    + u64::from(snapshot.consensus.parameters.acquisition_maturity),
            "genesis target does not account for acquisition maturity"
        );
        ensure!(snapshot.votes.len() == 1, "expected one initial vote");
        let vote = &snapshot.votes[0];
        ensure!(
            vote.minter_id == alice.id
                && vote.block_hash == genesis.tip.hash
                && vote.core_height == core.height,
            "initial vote targets the wrong block or minter"
        );
        ensure!(
            matches!(
                vote.processing_status,
                VoteProcessingStatus::Accepted {
                    effective_minting_power: 200
                }
            ),
            "initial vote was not accepted at full power"
        );
        let TransactionStatus::Confirmed { block: vote_block } = &vote.transaction_status else {
            anyhow::bail!("initial vote is unconfirmed");
        };
        ensure!(
            vote_block.height == u64::from(core.target_btc_height),
            "initial vote was delayed"
        );
        ensure!(
            genesis.parent_weight == 0 && genesis.block_weight == 7 && genesis.chain_weight == 7,
            "incorrect initial weight"
        );
        ensure!(
            snapshot.bitcoin.tip == snapshot.consensus.btc_cursor
                && snapshot.bitcoin.tip == *vote_block
                && snapshot.bitcoin.mempool.is_empty(),
            "startup returned before initialization completed"
        );
        let child = session
            .create_flame_block(CreateFlameBlockRequest {
                parent_hash: genesis.tip.hash.clone(),
                target_btc_height: core.target_btc_height + 1,
            })
            .await?;
        ensure!(
            child.core.unwrap().height == 2 && child.parent_weight == 7,
            "child did not inherit core genesis"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_votes_reorganize_competing_branches_and_double_sign_removes_power() -> Result<()> {
    let mut session = DemoSession::start().await?;
    let result: Result<()> = async {
        let root = session.canonical_tip().await?;
        let first_minter = session.minters().await?.remove(0);
        ensure!(
            !first_minter.automatic_voting,
            "manual session enabled automatic voting"
        );
        let second_minter = session
            .create_minter(CreateMinterRequest { name: "Bob".into() })
            .await?;
        ensure!(
            first_minter.p2wsh_address != second_minter.p2wsh_address,
            "minters share an identity"
        );
        mine(&mut session).await?;
        let sent = session
            .send_acquisition(SendAcquisitionRequest {
                minter_id: second_minter.id,
                amount_sats: NonZeroU64::new(80_000).unwrap(),
            })
            .await?;
        ensure!(
            session.bitcoin_mempool().await?.contains(&sent.txid),
            "acquisition missing from mempool"
        );
        let acquisition_tip = mine(&mut session).await?;
        let request = CreateFlameBlockRequest {
            parent_hash: root.hash.clone(),
            target_btc_height: u32::try_from(acquisition_tip.height + 1)?,
        };
        let first = session.create_flame_block(request.clone()).await?;
        let second = session.create_flame_block(request).await?;
        ensure!(
            first.tip.hash != second.tip.hash,
            "competing blocks have the same hash"
        );
        ensure!(
            first.parent_hash == second.parent_hash,
            "competing blocks have different parents"
        );
        ensure!(
            session.bitcoin_mempool().await?.is_empty(),
            "manual mode sent an automatic vote"
        );
        let first_vote = session
            .send_vote(SendVoteRequest {
                minter_id: first_minter.id,
                core_height: first.core.as_ref().unwrap().height,
                block_hash: first.tip.hash.clone(),
            })
            .await?;
        ensure!(
            session.bitcoin_mempool().await?.contains(&first_vote.txid),
            "vote missing from mempool"
        );
        ensure!(
            session.flame_block(&first.tip.hash).await?.effective_power == 0,
            "unconfirmed vote changed power"
        );
        mine(&mut session).await?;
        ensure!(
            session.canonical_tip().await? == first.tip,
            "first vote did not select its block"
        );
        ensure!(
            session.flame_block(&first.tip.hash).await?.effective_power == 200,
            "incorrect first vote power"
        );
        session
            .send_vote(SendVoteRequest {
                minter_id: second_minter.id,
                core_height: second.core.as_ref().unwrap().height,
                block_hash: second.tip.hash.clone(),
            })
            .await?;
        mine(&mut session).await?;
        ensure!(
            session.canonical_tip().await? == second.tip,
            "stronger vote did not reorganize the chain"
        );
        ensure!(
            session.flame_block(&second.tip.hash).await?.effective_power == 400,
            "late vote power was not reduced"
        );
        ensure!(
            !session.flame_block(&first.tip.hash).await?.is_canonical,
            "old branch is still canonical"
        );
        ensure!(
            session.flame_block(&root.hash).await?.is_canonical,
            "canonical ancestor was omitted"
        );
        session
            .send_vote(SendVoteRequest {
                minter_id: second_minter.id,
                core_height: first.core.as_ref().unwrap().height,
                block_hash: first.tip.hash.clone(),
            })
            .await?;
        mine(&mut session).await?;
        ensure!(
            session.flame_block(&second.tip.hash).await?.effective_power == 0,
            "double sign did not remove power"
        );
        ensure!(
            session.canonical_tip().await? == first.tip,
            "double sign did not restore the stronger branch"
        );
        ensure!(
            session
                .minters()
                .await?
                .iter()
                .any(|minter| minter.id == second_minter.id && minter.is_double_signed),
            "double sign missing from minter snapshot"
        );
        let child = session
            .create_flame_block(CreateFlameBlockRequest {
                parent_hash: first.tip.hash.clone(),
                target_btc_height: u32::try_from(session.bitcoin_tip().await?.height + 1)?,
            })
            .await?;
        ensure!(
            child.parent_weight == 14,
            "child did not inherit parent weight"
        );
        ensure!(
            child.core.unwrap().height == 3,
            "child core height is incorrect"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown
}

#[tokio::test(flavor = "multi_thread")]
async fn session_requires_manual_votes_after_bootstrap() -> Result<()> {
    let mut session = DemoSession::start().await?;
    let result: Result<()> = async {
        let root = session.canonical_tip().await?;
        let block = session
            .create_flame_block(CreateFlameBlockRequest {
                parent_hash: root.hash,
                target_btc_height: u32::try_from(session.bitcoin_tip().await?.height + 2)?,
            })
            .await?;
        mine(&mut session).await?;
        sleep(Duration::from_millis(300)).await;
        mine(&mut session).await?;
        let snapshot = session.snapshot().await?;
        ensure!(
            snapshot.votes.len() == 1,
            "session sent a vote without a command"
        );
        ensure!(
            snapshot.bitcoin.mempool.is_empty(),
            "unexpected pending vote"
        );
        ensure!(
            session.flame_block(&block.tip.hash).await?.block_weight == 0,
            "unvoted block gained weight"
        );
        ensure!(
            snapshot
                .minters
                .iter()
                .all(|minter| !minter.automatic_voting),
            "automatic voting enabled"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_commands_leave_the_session_usable() -> Result<()> {
    let mut session = DemoSession::start().await?;
    let result: Result<()> = async {
        ensure!(
            session
                .create_minter(CreateMinterRequest { name: "  ".into() })
                .await
                .is_err(),
            "accepted empty name"
        );
        ensure!(
            session.minters().await?.len() == 1,
            "invalid command registered a minter"
        );
        ensure!(
            session
                .send_acquisition(SendAcquisitionRequest {
                    minter_id: MinterId(99),
                    amount_sats: NonZeroU64::new(20_000).unwrap(),
                })
                .await
                .is_err(),
            "accepted unknown minter"
        );
        for hash in ["invalid".to_owned(), "ff".repeat(32)] {
            ensure!(
                session
                    .create_flame_block(CreateFlameBlockRequest {
                        parent_hash: FlameBlockHash(hash),
                        target_btc_height: 105,
                    })
                    .await
                    .is_err(),
                "accepted invalid or unknown parent"
            );
        }
        ensure!(
            session
                .send_vote(SendVoteRequest {
                    minter_id: MinterId(0),
                    core_height: 1,
                    block_hash: FlameBlockHash("00".into()),
                })
                .await
                .is_err(),
            "accepted a short hash"
        );
        ensure!(
            session.bitcoin_mempool().await?.is_empty(),
            "invalid command broadcast a transaction"
        );
        let before = session.bitcoin_tip().await?;
        let blocks = session
            .create_bitcoin_blocks(CreateBitcoinBlocksRequest {
                count: NonZeroU32::new(2).unwrap(),
            })
            .await?;
        ensure!(
            blocks.len() == 2 && blocks[1].height == before.height + 2,
            "batch mining failed"
        );
        ensure!(
            session.consensus_cursor().await? == blocks[1],
            "batch mining did not finish processing"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown
}
