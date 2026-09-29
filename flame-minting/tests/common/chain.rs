use std::{
    convert::Infallible,
    sync::{Arc, RwLock},
};

use bitcoind::anyhow::{Context, Result, ensure};
use flame_chain_service::{ChainAccess, ChainPath, ChangesOutcome, CoreBlockSource, ImportOutcome};
use flame_minting::core_block_notifier::CoreBlockNotifier;
use flamechain::{Block, BlockHash, BlockTip};

#[derive(Clone, Default)]
pub struct TestChain {
    blocks: Arc<RwLock<Vec<Arc<Block>>>>,
}

impl TestChain {
    async fn parent_tip(&self, tip: BlockTip) -> Result<BlockTip> {
        let height = tip
            .height
            .as_u64()
            .checked_sub(1)
            .context("path has no common ancestor")?;
        let block = self.get_block(tip).await?.context("missing path block")?;
        Ok(BlockTip {
            hash: block.header.parent,
            height: height.into(),
        })
    }
}

impl ChainAccess for TestChain {
    type Error = bitcoind::anyhow::Error;

    async fn set_as_child(&self, parent: BlockTip, child: BlockTip) -> Result<()> {
        let block = self
            .get_block(child)
            .await?
            .context("missing child block")?;
        ensure!(block.header.parent == parent.hash, "unexpected parent");
        ensure!(
            child.height.as_u64() == parent.height.as_u64() + 1,
            "unexpected height"
        );
        Ok(())
    }

    async fn get_block(&self, tip: BlockTip) -> Result<Option<Arc<Block>>> {
        Ok(self
            .blocks
            .read()
            .unwrap()
            .iter()
            .find(|block| block.header.block_tip() == tip)
            .cloned())
    }

    async fn get_chain_path(&self, from: BlockTip, to: BlockTip) -> Result<ChainPath> {
        let mut path = ChainPath::default();
        let mut from = from;
        let mut to = to;
        while from != to {
            if from.height >= to.height {
                path.detach.push(from);
                from = self.parent_tip(from).await?;
            } else {
                path.attach.push(to);
                to = self.parent_tip(to).await?;
            }
        }
        path.attach.reverse();
        Ok(path)
    }

    async fn import_block(&mut self, block: Arc<Block>) -> Result<ImportOutcome> {
        let hash = block.header.id();
        let mut blocks = self.blocks.write().unwrap();
        if blocks.iter().any(|stored| stored.header.id() == hash) {
            return Ok(ImportOutcome::AlreadyKnown(hash));
        }
        blocks.push(block);
        Ok(ImportOutcome::Imported(hash))
    }

    async fn select_tip(&mut self, _: BlockHash) -> Result<ChangesOutcome> {
        unreachable!("consensus applies the chain path through set_as_child")
    }
}

impl CoreBlockSource for TestChain {
    type Error = Infallible;

    async fn get_core_block(&self, target_height: u64) -> Result<Option<Arc<Block>>, Self::Error> {
        Ok(self
            .blocks
            .read()
            .unwrap()
            .iter()
            .find(|block| {
                block
                    .header
                    .core_block
                    .as_ref()
                    .is_some_and(|core| u64::from(core.target_btc_height) == target_height)
            })
            .cloned())
    }

    async fn wait_core_block(&self, _: u64) -> Result<Option<Arc<Block>>, Self::Error> {
        unreachable!("minter reads available core blocks")
    }
}

pub struct UnusedNotifier;

impl CoreBlockNotifier for UnusedNotifier {
    type Error = Infallible;

    async fn notify_core_block_needed(&mut self, _: u64) -> Result<(), Self::Error> {
        unreachable!("the test supplies the core block")
    }
}
