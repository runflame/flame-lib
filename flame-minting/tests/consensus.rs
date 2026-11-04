mod common;

use bitcoind::anyhow::{Context, Result};
use btc_integration::MintingEndorsementData;
use corepc_client::bitcoin::Amount;
use flame_minting::consensus::ConsensusStorage;
use flame_storage::CanonicalStorage;
use flamechain::{BlockTip, CoreBlockHeader, CoreBlockTip};
use std::time::Duration;
use tokio::time::{sleep, timeout};

use common::TestContext;

#[tokio::test(flavor = "multi_thread")]
async fn public_handle_and_indexer_share_state_across_arrivals_restart_and_bitcoin_reorg()
-> Result<()> {
    TestContext::run(|mut ctx| async move {
        ctx.send_acquisition(Amount::from_sat(20_000)).await?;
        let acquisition_tip = ctx.bitcoin.mine_block().await?;
        ctx.start().await?;
        ctx.wait_for_processing(acquisition_tip).await?;
        let target_btc_height = u32::try_from(acquisition_tip.height + 1)?;
        let mut state = ctx.flame_chain.state().clone();
        let mut candidate = state.build_block([0; 32], vec![])?;
        candidate.header.core_block = Some(CoreBlockHeader {
            height: 1.into(),
            target_btc_height,
        });
        ctx.orchestrator
            .btc_sender
            .send_endorsement(1, candidate.header.id())
            .await?;
        let confirmation = ctx.bitcoin.mine_block().await?;
        ctx.wait_for_processing(confirmation).await?;
        let core_tip = CoreBlockTip {
            hash: candidate.header.id(),
            height: 1.into(),
        };
        let consensus = ctx.orchestrator.get_consensus_storage();
        assert_eq!(
            consensus
                .get_pending_endorsements_for_block(core_tip)
                .await?
                .len(),
            1
        );
        assert!(
            consensus
                .get_endorsements_for_block(core_tip)
                .await?
                .is_empty()
        );

        let block = ctx.flame_chain.create_core_block(target_btc_height).await?;
        assert_eq!(block.header, candidate.header);
        let handle = ctx.orchestrator.consensus_handle();
        handle
            .on_flame_block_arrival(block.header.block_tip())
            .await?;
        // The canonical tip moves at `ApplyTransition`, but pending endorsements are only
        // removed in the final `RemovePending` stage, so wait for both.
        timeout(Duration::from_secs(10), async {
            while ctx.orchestrator.get_canonical_storage().get_tip().await?
                != Some(block.header.block_tip())
                || !consensus
                    .get_pending_endorsements_for_block(core_tip)
                    .await?
                    .is_empty()
            {
                sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, bitcoind::anyhow::Error>(())
        })
        .await
        .context("timed out applying the Flame arrival through the public handle")??;
        let counted = consensus.get_endorsements_for_block(core_tip).await?;
        assert_eq!(counted.len(), 1);
        assert_eq!(counted[0].original.btc_block, confirmation);
        assert_eq!(counted[0].effective_minting_power, 200);
        handle
            .on_flame_block_arrival(block.header.block_tip())
            .await?;
        ctx.orchestrator
            .shutdown()
            .await
            .map_err(|error| bitcoind::anyhow::anyhow!("shutdown: {error:?}"))?;
        assert_eq!(
            handle
                .on_flame_block_arrival(block.header.block_tip())
                .await,
            Err(flame_minting::ConsensusSendError::Stopped)
        );
        let _: () = ctx
            .bitcoin
            .node
            .client
            .call("invalidateblock", &[confirmation.hash.to_string().into()])?;
        let address = ctx.bitcoin.node.client.new_address()?;
        ctx.bitcoin.node.client.generate_to_address(1, &address)?;
        let replacement = ctx.bitcoin.rpc.best_block_tip().await?;
        assert_ne!(replacement.hash, confirmation.hash);
        let target = ctx.bitcoin.mine_block().await?;
        ctx.start().await?;
        ctx.wait_for_processing(target).await?;
        handle
            .on_flame_block_arrival(block.header.block_tip())
            .await?;
        let replaced = consensus.get_endorsements_for_block(core_tip).await?;
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].original.btc_block, replacement);
        assert_eq!(
            replaced[0].original.endorsement,
            counted[0].original.endorsement
        );
        assert_eq!(
            replaced[0].effective_minting_power,
            counted[0].effective_minting_power
        );
        assert_eq!(
            ctx.orchestrator.get_canonical_storage().get_tip().await?,
            Some(block.header.block_tip())
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn stronger_late_endorsement_reorganizes_to_competing_core_block() -> Result<()> {
    TestContext::run(|mut ctx| async move {
        let second_minter = ctx.create_minter().await?;
        ctx.bitcoin.mine_block().await?;
        ctx.send_acquisition(Amount::from_sat(20_000)).await?;
        second_minter
            .send_acquisition(Amount::from_sat(80_000))
            .await?;
        let acquisition_tip = ctx.bitcoin.mine_block().await?;
        let target_btc_height = u32::try_from(acquisition_tip.height + 1)?;
        let [first, second] = ctx
            .flame_chain
            .create_competing_core_blocks(target_btc_height)
            .await?;
        let canonical = ctx.orchestrator.get_canonical_storage();

        ctx.start().await?;
        ctx.bitcoin
            .wait_for_endorsement(&MintingEndorsementData::V1 {
                flame_block_height: 1,
                flame_block_hash: first.header.id(),
            })
            .await?;
        let first_confirmation = ctx.bitcoin.mine_block().await?;
        ctx.wait_for_processing(first_confirmation).await?;
        assert_eq!(canonical.get_tip().await?, Some(first.header.block_tip()));

        second_minter
            .send_endorsement(1, second.header.id())
            .await?;
        let second_confirmation = ctx.bitcoin.mine_block().await?;
        ctx.wait_for_processing(second_confirmation).await?;
        assert_eq!(canonical.get_tip().await?, Some(second.header.block_tip()));
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn acquisition_and_automatic_endorsement_select_and_apply_core_block() -> Result<()> {
    TestContext::run(|mut ctx| async move {
        let acquisition_txid = ctx.send_acquisition(Amount::from_sat(20_000)).await?;
        let starting_tip = ctx.bitcoin.mine_block().await?;
        let root_tip = BlockTip {
            hash: ctx.flame_chain.state().tip(),
            height: ctx.flame_chain.state().height().into(),
        };
        let consensus = ctx.orchestrator.get_consensus_storage();
        let canonical = ctx.orchestrator.get_canonical_storage();

        ctx.start().await?;
        ctx.wait_for_processing(starting_tip).await?;
        let acquisitions = consensus
            .get_acquisitions_by_minters(starting_tip.height)
            .await?;
        assert_eq!(acquisitions.len(), 1);
        let minter = &acquisitions[&ctx.identity.p2wsh()];
        assert!(!minter.is_double_signed);
        assert_eq!(minter.acquisitions.len(), 1);
        assert_eq!(minter.acquisitions[0].acquisition.txid(), acquisition_txid);
        assert_eq!(minter.acquisitions[0].btc_block, starting_tip);
        assert_eq!(canonical.get_tip().await?, Some(root_tip));

        let target_btc_height = u32::try_from(starting_tip.height + 2)?;
        let block = ctx.flame_chain.create_core_block(target_btc_height).await?;
        let core_tip = CoreBlockTip {
            hash: block.header.id(),
            height: 1.into(),
        };

        let trigger_tip = ctx.bitcoin.mine_block().await?;
        assert_eq!(trigger_tip.height, starting_tip.height + 1);
        let endorsement_txid = ctx
            .bitcoin
            .wait_for_endorsement(&MintingEndorsementData::V1 {
                flame_block_height: 1,
                flame_block_hash: core_tip.hash,
            })
            .await?;
        ctx.wait_for_processing(trigger_tip).await?;
        assert!(
            consensus
                .get_endorsements_for_block(core_tip)
                .await?
                .is_empty()
        );
        let initial_weight = consensus
            .get_cumulative_weight(block.header.block_tip())
            .await?
            .context("missing initial core block weight")?;
        assert_eq!(initial_weight.parent_weight, 0);
        assert_eq!(initial_weight.effective_power, 0);

        let confirmation = ctx.bitcoin.mine_block().await?;
        assert_eq!(confirmation.height, u64::from(target_btc_height));
        let endorsements = ctx.bitcoin.endorsements_in_block(confirmation).await?;
        assert_eq!(endorsements.len(), 1);
        let endorsement = &endorsements[0];
        assert_eq!(endorsement.txid(), endorsement_txid);
        assert_eq!(endorsement.block_tip(), core_tip);
        assert_eq!(endorsement.auth().minter(), &ctx.identity);

        ctx.wait_for_processing(confirmation).await?;
        let accepted = consensus.get_endorsements_for_block(core_tip).await?;
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].original.endorsement, *endorsement);
        assert_eq!(accepted[0].original.btc_block, confirmation);
        assert_eq!(accepted[0].effective_minting_power, 200);
        let weight = consensus
            .get_cumulative_weight(block.header.block_tip())
            .await?
            .context("missing core block weight")?;
        assert_eq!(weight.header, block.header);
        assert_eq!(weight.parent_weight, 0);
        assert_eq!(weight.effective_power, 200);
        assert_eq!(
            consensus.get_block_tip_with_most_weight().await?,
            Some(block.header.block_tip())
        );
        assert_eq!(canonical.get_tip().await?, Some(block.header.block_tip()));
        let (_, applied) = canonical
            .get_state()
            .await?
            .context("missing canonical state")?;
        assert_eq!(applied.tip(), ctx.flame_chain.state().tip());
        assert_eq!(applied.height(), ctx.flame_chain.state().height());
        assert_eq!(
            applied.state_commitment(),
            ctx.flame_chain.state().state_commitment()
        );
        Ok(())
    })
    .await
}
