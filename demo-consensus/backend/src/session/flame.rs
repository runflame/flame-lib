use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use flame_chain_service::{ChainAccess, InMemoryChain};
use flame_minting::consensus::{
    ConsensusStorage, WeightedBlockHeader, storage::InMemoryConsensusStorage,
};
use flame_storage::{
    CanonicalStorage, ChainStorage, chain::InMemoryChainStorage,
    state::canonical::InMemoryCanonicalStorage,
};
use flamechain::{Block, BlockHash, Blockchain, ChainParams, CoreBlockHeader};

use crate::types::{
    CoreBlockSnapshot, CreateFlameBlockRequest, FlameBlockHash, FlameBlockSnapshot,
};

use super::flame_tip;
use crate::error::DemoError;

struct BlockState {
    block: Arc<Block>,
    state: Blockchain,
}

pub(super) struct DemoFlameChain {
    chain: InMemoryChain,
    blocks: HashMap<BlockHash, BlockState>,
    block_storage: InMemoryChainStorage,
    consensus: InMemoryConsensusStorage,
    canonical: InMemoryCanonicalStorage,
    next_block_id: u64,
}

impl DemoFlameChain {
    pub async fn new(
        chain: InMemoryChain,
        block_storage: InMemoryChainStorage,
        consensus: InMemoryConsensusStorage,
        canonical: InMemoryCanonicalStorage,
        target_btc_height: u32,
    ) -> Result<Self> {
        let mut state = Blockchain::new(ChainParams::default())?;
        let mut root = state.build_block([0x11; 32], Vec::new())?;
        root.header.core_block = Some(CoreBlockHeader {
            height: 1.into(),
            target_btc_height,
        });
        let root = Arc::new(root);
        state.connect(&root)?;
        let mut flame = Self {
            chain,
            blocks: HashMap::new(),
            block_storage,
            consensus,
            canonical,
            next_block_id: 1,
        };
        flame.register_block(root, state.clone(), 0).await?;
        flame.canonical.commit_state(&state).await?;
        Ok(flame)
    }

    pub async fn create_block(
        &mut self,
        request: CreateFlameBlockRequest,
    ) -> Result<FlameBlockSnapshot> {
        let parent_hash = parse_flame_hash(&request.parent_hash)?;
        let parent = self
            .blocks
            .get(&parent_hash)
            .ok_or_else(|| DemoError::NotFound("unknown parent block".into()))?;
        let core_height = parent
            .block
            .header
            .core_block
            .as_ref()
            .map_or(0, |core| core.height.as_u32())
            .checked_add(1)
            .context("core height overflow")?;
        let next_block_id = self
            .next_block_id
            .checked_add(1)
            .context("block id overflow")?;
        let mut core_hash = [0; 32];
        core_hash[..8].copy_from_slice(&self.next_block_id.to_le_bytes());
        let mut state = parent.state.clone();
        let mut block = state.build_block(core_hash, Vec::new())?;
        block.header.core_block = Some(CoreBlockHeader {
            height: core_height.into(),
            target_btc_height: request.target_btc_height,
        });
        let weight = self
            .consensus
            .get_cumulative_weight(parent.block.header.block_tip())
            .await?
            .context("missing parent weight")?;
        let parent_block_weight = if weight.header.core_block.is_some() {
            u64::from(weight.effective_power.checked_ilog2().unwrap_or(0))
        } else {
            0
        };
        let parent_weight = weight
            .parent_weight
            .checked_add(parent_block_weight)
            .context("parent weight overflow")?;
        state.connect(&block)?;
        let block = Arc::new(block);
        let hash = block.header.id();
        self.register_block(block, state, parent_weight).await?;
        self.next_block_id = next_block_id;
        self.snapshot(hash).await
    }

    async fn register_block(
        &mut self,
        block: Arc<Block>,
        state: Blockchain,
        parent_weight: u64,
    ) -> Result<()> {
        self.block_storage.add_block(block.clone()).await?;
        self.consensus
            .store_cumulative_weight(
                block.header.block_tip(),
                &WeightedBlockHeader {
                    header: block.header.clone(),
                    parent_weight,
                    effective_power: 0,
                },
            )
            .await?;
        self.chain.import_block(block.clone()).await?;
        self.blocks
            .insert(block.header.id(), BlockState { block, state });
        Ok(())
    }

    pub async fn snapshot(&self, hash: BlockHash) -> Result<FlameBlockSnapshot> {
        let block = &self
            .blocks
            .get(&hash)
            .ok_or_else(|| DemoError::NotFound("unknown Flame block".into()))?
            .block;
        let tip = block.header.block_tip();
        let weight = self
            .consensus
            .get_cumulative_weight(tip)
            .await?
            .context("missing block weight")?;
        let mut canonical_tip = self.canonical.get_tip().await?;
        let mut is_canonical = false;
        while let Some(current) = canonical_tip {
            if current == tip {
                is_canonical = true;
                break;
            }
            let ancestor = self
                .blocks
                .get(&current.hash)
                .context("missing canonical block")?;
            canonical_tip = self
                .blocks
                .get(&ancestor.block.header.parent)
                .map(|parent| parent.block.header.block_tip());
        }
        let block_weight = if block.header.core_block.is_some() {
            weight.effective_power.checked_ilog2().unwrap_or(0)
        } else {
            0
        };
        Ok(FlameBlockSnapshot {
            tip: flame_tip(tip),
            parent_hash: self
                .blocks
                .get(&block.header.parent)
                .map(|parent| FlameBlockHash(hex::encode(parent.block.header.id().as_bytes()))),
            core: block
                .header
                .core_block
                .as_ref()
                .map(|core| CoreBlockSnapshot {
                    height: core.height.as_u32(),
                    target_btc_height: core.target_btc_height,
                }),
            is_canonical,
            parent_weight: weight.parent_weight,
            effective_power: weight.effective_power,
            block_weight,
            chain_weight: u128::from(weight.parent_weight) + u128::from(block_weight),
        })
    }

    pub async fn snapshots(&self) -> Result<Vec<FlameBlockSnapshot>> {
        let mut tips = self
            .blocks
            .values()
            .map(|entry| entry.block.header.block_tip())
            .collect::<Vec<_>>();
        tips.sort_by_key(|tip| (tip.height, tip.hash));
        let mut snapshots = Vec::with_capacity(tips.len());
        for tip in tips {
            snapshots.push(self.snapshot(tip.hash).await?);
        }
        Ok(snapshots)
    }
}

pub(super) fn parse_flame_hash(hash: &FlameBlockHash) -> Result<BlockHash> {
    let bytes: [u8; 32] = hex::decode(&hash.0)
        .map_err(|_| DemoError::InvalidRequest("invalid Flame block hash".into()))?
        .try_into()
        .map_err(|_| DemoError::InvalidRequest("Flame block hash must contain 32 bytes".into()))?;
    Ok(BlockHash::new(bytes))
}
