use std::sync::Mutex as StdMutex;

use btc_integration::{HistoryUpdate, IndexedBlock};
use corepc_client::bitcoin::{BlockHash as BtcBlockHash, hashes::Hash};
use flamechain::{BlockHeader, Blockchain, ChainParams, CoreBlockHeader};

use super::*;
use crate::consensus::MintingOutcome;
use crate::consensus::engine::tests::Storage;
use crate::rewards::PendingReward;
use crate::rewards::engine::tests::TestChainStorage;
use crate::rewards::engine::tests::protocol_params;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    ConsensusApplied,
    CursorStored,
}

#[derive(Default)]
struct State {
    events: StdMutex<Vec<Event>>,
    headers: Vec<BlockHeader>,
    cancel_during_calculate: bool,
    cancel_during_apply: bool,
    cancellation: CancellationToken,
}

impl State {
    fn record(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

struct Indexer;
struct Engine(Arc<State>);
struct Applier(Arc<State>);
struct RewardStorage;
struct Cursor(Arc<State>);

fn btc_tip(height: u64) -> BtcBlockTip {
    BtcBlockTip {
        hash: BtcBlockHash::from_byte_array([height as u8; 32]),
        height,
    }
}

fn header(core_height: Option<u32>) -> BlockHeader {
    let mut header = Blockchain::new(ChainParams::default())
        .unwrap()
        .build_block([0; 32], vec![])
        .unwrap()
        .header;
    header.height = 350;
    header.core_block = core_height.map(|height| CoreBlockHeader {
        height: height.into(),
        target_btc_height: height,
    });
    header
}

impl ConsensusIndexer for Indexer {
    async fn startup(&self) -> Result<(), StartupError> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), ShutdownError> {
        Ok(())
    }

    fn subscribe(&self) -> watch::Receiver<Option<BtcBlockTip>> {
        watch::channel(Some(btc_tip(1))).1
    }

    async fn get_history(
        &self,
        cursor: BtcBlockTip,
        _: NonZeroUsize,
    ) -> Result<HistoryUpdate, HistoryError> {
        assert_eq!(cursor, btc_tip(0));
        Ok(HistoryUpdate {
            target_tip: btc_tip(1),
            next_cursor: btc_tip(1),
            change: HistoryChange::Extension {
                new_blocks: vec![IndexedBlock {
                    btc_block_tip: btc_tip(1),
                    acquisitions: vec![],
                    votes: vec![],
                }],
            },
        })
    }
}

impl ConsensusEngine for Engine {
    type Error = &'static str;

    async fn get_minting_outcome(
        &self,
        block: IndexedBlock,
    ) -> Result<MintingOutcome, Self::Error> {
        if self.0.cancel_during_calculate {
            self.0.cancellation.cancel();
            return std::future::pending().await;
        }
        Ok(MintingOutcome {
            accepted_votes: vec![],
            removed_votes: vec![],
            weighted_blocks: Default::default(),
            accepted_acquisitions: vec![],
            double_signs: vec![],
            pending_votes: Default::default(),
            next_btc_cursor: block.btc_block_tip,
        })
    }
}

impl ConsensusApplier for Applier {
    type Error = &'static str;

    async fn apply(&mut self, _: &MintingOutcome) -> Result<Vec<BlockHeader>, Self::Error> {
        self.0.record(Event::ConsensusApplied);
        if self.0.cancel_during_apply {
            self.0.cancellation.cancel();
        }
        Ok(self.0.headers.clone())
    }
}

impl RewardsStorage for RewardStorage {
    type Error = &'static str;

    async fn store_pending_reward(
        &self,
        _: flamechain::CoreFlameHeight,
        _: &PendingReward,
    ) -> Result<(), Self::Error> {
        panic!("no rewards should be stored before calculation is implemented")
    }
}

impl CursorStorage for Cursor {
    type Error = &'static str;

    async fn get_cursor(&self) -> Result<BtcBlockTip, Self::Error> {
        Ok(btc_tip(0))
    }

    async fn store_cursor(&self, cursor: BtcBlockTip) -> Result<(), Self::Error> {
        assert_eq!(cursor, btc_tip(1));
        self.0.record(Event::CursorStored);
        self.0.cancellation.cancel();
        Ok(())
    }
}

async fn run(
    state: &Arc<State>,
) -> LoopResult<&'static str, &'static str, &'static str, &'static str, &'static str, &'static str>
{
    let consensus = ConsensusLoop::new(
        Engine(state.clone()),
        Applier(state.clone()),
        Cursor(state.clone()),
        RewardsEngine {
            consensus_storage: Arc::new(Storage::default()),
            protocol_params: protocol_params(),
            chain_storage: Arc::new(TestChainStorage::default()),
        },
        RewardsApplier {
            rewards_storage: RewardStorage,
        },
    );
    consensus
        .processor
        .run(
            &Indexer,
            Indexer.subscribe(),
            btc_tip(0),
            state.cancellation.clone(),
        )
        .await
}

#[tokio::test]
async fn skips_non_distribution_blocks() {
    let state = Arc::new(State {
        headers: [None, Some(49), Some(50), Some(99), Some(100), Some(151)]
            .into_iter()
            .map(header)
            .collect(),
        ..State::default()
    });
    run(&state).await.unwrap();
    assert_eq!(
        *state.events.lock().unwrap(),
        vec![Event::ConsensusApplied, Event::CursorStored,]
    );
}

#[tokio::test]
async fn unchanged_chain_does_not_trigger_rewards() {
    let state = Arc::new(State::default());
    run(&state).await.unwrap();
    assert_eq!(
        *state.events.lock().unwrap(),
        vec![Event::ConsensusApplied, Event::CursorStored]
    );
}

#[tokio::test]
async fn cancellation_interrupts_consensus_calculation_before_applying() {
    let state = Arc::new(State {
        cancel_during_calculate: true,
        ..State::default()
    });
    run(&state).await.unwrap();
    assert!(state.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_during_apply_stores_cursor() {
    let state = Arc::new(State {
        headers: vec![header(Some(149))],
        cancel_during_apply: true,
        ..State::default()
    });
    run(&state).await.unwrap();
    assert_eq!(
        *state.events.lock().unwrap(),
        vec![Event::ConsensusApplied, Event::CursorStored,]
    );
}

#[tokio::test]
async fn missing_rewards_history_prevents_cursor_advancement() {
    for height in [150, 250] {
        let state = Arc::new(State {
            headers: vec![
                header(Some(height - 1)),
                header(Some(height)),
                header(Some(height + 1)),
            ],
            ..State::default()
        });
        assert!(matches!(
            run(&state).await,
            Err(ConsensusLoopError::RewardsEngine(
                RewardsEngineError::MissingBlock(_)
            ))
        ));
        assert_eq!(*state.events.lock().unwrap(), vec![Event::ConsensusApplied]);
    }
}
