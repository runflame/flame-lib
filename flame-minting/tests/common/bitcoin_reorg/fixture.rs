use super::*;

pub struct Fixture {
    initial: Blockchain,
    root: Arc<Block>,
    pub old_flame: Vec<Arc<Block>>,
    pub new_flame: Vec<Arc<Block>>,
}

impl Fixture {
    pub fn new(branch_length: u32) -> Self {
        let mut initial = Blockchain::new(ChainParams::default()).unwrap();
        let root = Arc::new(initial.build_block([0; 32], vec![]).unwrap());
        initial.connect(&root).unwrap();
        Self {
            old_flame: flame_branch(initial.clone(), 1, branch_length),
            new_flame: flame_branch(initial.clone(), 2, branch_length),
            initial,
            root,
        }
    }

    pub async fn node(&self) -> TestNode {
        let consensus = InMemoryConsensusStorage::new();
        let canonical = InMemoryCanonicalStorage::new();
        let rewards = InMemoryRewardsStorage::new();
        let cursor = InMemoryCursorStorage::new(btc_tip(0, 0));
        let mut chain = InMemoryChain::default();
        let blocks = InMemoryChainStorage::new();
        let params = Arc::new(MintingProtocolParams {
            acquisition_maturity: 1,
            default_acquisition_duration: 100.try_into().unwrap(),
            min_acquisition_duration: 1.try_into().unwrap(),
            max_endorsement_delay: 10,
        });
        let node = TestNode {
            processor: BitcoinBlockProcessor::new(
                MintingEngineFactory {
                    protocol_params: params.clone(),
                    consensus_storage: Arc::new(consensus.clone()),
                    chain_storage: Arc::new(blocks.clone()),
                    canonical_storage: Arc::new(canonical.clone()),
                },
                consensus.clone(),
                chain.clone(),
                InMemoryConsensusJournal::new(btc_tip(0, 0)).bitcoin_journal(),
                canonical.clone(),
                rewards.clone(),
                cursor.clone(),
                RewardsEngine {
                    protocol_params: params,
                    consensus_storage: Arc::new(consensus.clone()),
                    chain_storage: Arc::new(blocks.clone()),
                },
            ),
            canonical,
            rewards,
            cursor,
        };
        node.canonical.commit_state(&self.initial).await.unwrap();
        register_block(&self.root, &blocks, &mut chain, &consensus).await;
        node.apply(&BtcBlockRecords {
            id: btc_tip(1, 0),
            burns: vec![acquisition(1, 1_000_000), acquisition(2, 100_000_000)],
            endorsements: vec![],
        })
        .await;
        for block in self.old_flame.iter().chain(&self.new_flame) {
            register_block(block, &blocks, &mut chain, &consensus).await;
        }
        node
    }
}
