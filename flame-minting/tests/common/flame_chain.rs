use std::sync::Arc;

use bitcoind::anyhow::{Context, Result};
use flame_chain_service::{ChainAccess, InMemoryChain};
use flame_minting::consensus::{
    ConsensusStorage, WeightedBlockHeader, storage::InMemoryConsensusStorage,
};
use flame_storage::{
    CanonicalStorage, ChainStorage, chain::InMemoryChainStorage,
    state::canonical::InMemoryCanonicalStorage,
};
use flamechain::{Block, Blockchain, ChainParams, CoreBlockHeader};

#[derive(Clone)]
pub struct FlameChain {
    chain: InMemoryChain,
    chain_storage: InMemoryChainStorage,
    consensus_storage: InMemoryConsensusStorage,
    state: Blockchain,
    core_height: u32,
}

impl FlameChain {
    pub async fn new(
        chain: InMemoryChain,
        chain_storage: InMemoryChainStorage,
        consensus_storage: InMemoryConsensusStorage,
        canonical_storage: &InMemoryCanonicalStorage,
    ) -> Result<Self> {
        let mut state = Blockchain::new(ChainParams::default())?;
        let root = Arc::new(state.build_block([0x11; 32], Vec::new())?);
        state.connect(&root)?;
        let mut fixture = Self {
            chain,
            chain_storage,
            consensus_storage,
            state,
            core_height: 0,
        };
        fixture.register_block(root, 0).await?;
        canonical_storage.commit_state(&fixture.state).await?;
        Ok(fixture)
    }

    pub fn state(&self) -> &Blockchain {
        &self.state
    }

    pub async fn create_core_block(&mut self, target_btc_height: u32) -> Result<Arc<Block>> {
        self.create_core_block_with_hash(target_btc_height, [0; 32])
            .await
    }

    pub async fn create_competing_core_blocks(
        &mut self,
        target_btc_height: u32,
    ) -> Result<[Arc<Block>; 2]> {
        let mut alternative = self.clone();
        let first = self.create_core_block(target_btc_height).await?;
        let second = alternative
            .create_core_block_with_hash(target_btc_height, [1; 32])
            .await?;
        Ok([first, second])
    }

    async fn create_core_block_with_hash(
        &mut self,
        target_btc_height: u32,
        core_block_hash: [u8; 32],
    ) -> Result<Arc<Block>> {
        let core_height = self
            .core_height
            .checked_add(1)
            .context("core height overflow")?;
        let mut next_state = self.state.clone();
        let mut block = next_state.build_block(core_block_hash, Vec::new())?;
        block.header.core_block = Some(CoreBlockHeader {
            height: core_height.into(),
            target_btc_height,
        });
        let parent = self
            .consensus_storage
            .get_cumulative_weight(flamechain::BlockTip {
                hash: block.header.parent,
                height: (block.header.height - 1).into(),
            })
            .await?
            .context("missing parent weight")?;
        let parent_block_weight = if parent.header.core_block.is_some() {
            u64::from(parent.effective_power.checked_ilog2().unwrap_or(0))
        } else {
            0
        };
        let parent_weight = parent
            .parent_weight
            .checked_add(parent_block_weight)
            .context("parent weight overflow")?;
        next_state.connect(&block)?;
        let block = Arc::new(block);
        self.register_block(block.clone(), parent_weight).await?;
        self.state = next_state;
        self.core_height = core_height;
        Ok(block)
    }

    async fn register_block(&mut self, block: Arc<Block>, parent_weight: u64) -> Result<()> {
        self.chain_storage.add_block(block.clone()).await?;
        self.consensus_storage
            .store_cumulative_weight(
                block.header.block_tip(),
                &WeightedBlockHeader {
                    header: block.header.clone(),
                    parent_weight,
                    effective_power: 0,
                },
            )
            .await?;
        self.chain.import_block(block).await?;
        Ok(())
    }
}
