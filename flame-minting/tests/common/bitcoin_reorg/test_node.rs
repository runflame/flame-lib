use super::*;

type Processor = BitcoinBlockProcessor<
    MintingEngineFactory<InMemoryConsensusStorage, InMemoryChainStorage, InMemoryCanonicalStorage>,
    InMemoryConsensusStorage,
    InMemoryChain,
    InMemoryBitcoinJournal,
    InMemoryCanonicalStorage,
    InMemoryRewardsStorage,
    InMemoryCursorStorage,
    InMemoryChainStorage,
>;

pub struct TestNode {
    pub(super) processor: Processor,
    pub(super) canonical: InMemoryCanonicalStorage,
    pub(super) rewards: InMemoryRewardsStorage,
    pub(super) cursor: InMemoryCursorStorage,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub btc_tip: BlockId,
    pub flame_tip: BlockTip,
    state: StateCommitment,
}

impl TestNode {
    pub async fn apply(&self, block: &BtcBlockRecords) {
        self.processor.apply(block.clone()).await.unwrap();
    }

    pub async fn rollback(&self, block: &BtcBlockRecords) {
        self.processor.rollback(block.id).await.unwrap();
    }

    pub async fn snapshot(&self) -> Snapshot {
        let (_, state) = self.canonical.get_state().await.unwrap().unwrap();
        Snapshot {
            btc_tip: self.cursor.get_cursor().await.unwrap(),
            flame_tip: BlockTip {
                hash: state.tip(),
                height: state.height().into(),
            },
            state: state.state_commitment(),
        }
    }

    pub async fn rewards(&self, height: u32) -> Vec<(RewardIndex, u64)> {
        self.rewards
            .get_pending_rewards(height.into())
            .await
            .unwrap()
            .iter()
            .map(|reward| (RewardIndex::from(reward), reward.amount))
            .collect()
    }
}
