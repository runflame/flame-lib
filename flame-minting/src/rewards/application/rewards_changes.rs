use crate::rewards::RewardsOutcome;
use crate::{
    consensus::ConsensusStorage,
    rewards::{RewardsEngine, RewardsEngineError, RewardsStorage, RewardsUndo, RewardsUndoManager},
};
use btc_integration::watcher::BlockId;
use flame_storage::ChainStorage;
use flamechain::BlockHeader;

pub(crate) async fn prepare<T: ChainStorage, C: ConsensusStorage, R: RewardsStorage>(
    engine: &RewardsEngine<T, C>,
    storage: &R,
    headers: &[BlockHeader],
    btc_tip: BlockId,
) -> Result<RewardsChanges, PrepareRewardsError<T::Error, C::Error, R::Error>> {
    let mut outcomes = Vec::new();
    for header in headers {
        if let Some(outcome) = engine
            .calculate(header, btc_tip)
            .await
            .map_err(PrepareRewardsError::Engine)?
        {
            outcomes.push(outcome);
        }
    }
    let mut undo = RewardsUndo::default();
    let manager = RewardsUndoManager {
        rewards_storage: storage,
    };
    for outcome in &outcomes {
        let previous = manager
            .prepare(outcome)
            .await
            .map_err(PrepareRewardsError::Storage)?;
        for (key, value) in previous.rewards {
            undo.rewards.entry(key).or_insert(value);
        }
    }
    Ok(RewardsChanges { outcomes, undo })
}

pub(crate) async fn apply<R: RewardsStorage>(
    storage: &R,
    changes: &RewardsChanges,
) -> Result<(), R::Error> {
    for outcome in &changes.outcomes {
        crate::rewards::application::rewards_applier::apply_outcome(storage, outcome).await?;
    }
    Ok(())
}

pub(crate) async fn prepare_replacement<T: ChainStorage, C: ConsensusStorage, R: RewardsStorage>(
    engine: &RewardsEngine<T, C>,
    storage: &R,
    headers: &[BlockHeader],
    targets: &std::collections::BTreeSet<flamechain::CoreFlameHeight>,
    btc_tip: BlockId,
) -> Result<RewardsChanges, PrepareRewardsError<T::Error, C::Error, R::Error>> {
    let mut outcomes: std::collections::BTreeMap<_, _> = targets
        .iter()
        .map(|&height| {
            (
                height,
                crate::rewards::RewardsOutcome {
                    target_block_height: height,
                    rewards: Vec::new(),
                },
            )
        })
        .collect();
    for header in headers {
        if let Some(outcome) = engine
            .calculate(header, btc_tip)
            .await
            .map_err(PrepareRewardsError::Engine)?
        {
            outcomes.insert(outcome.target_block_height, outcome);
        }
    }
    let mut undo = RewardsUndo::default();
    for outcome in outcomes.values() {
        let height = outcome.target_block_height;
        for previous in storage
            .get_pending_rewards(height)
            .await
            .map_err(PrepareRewardsError::Storage)?
        {
            undo.rewards.insert(
                (height, crate::rewards::RewardIndex::from(&previous)),
                Some(previous),
            );
        }
        for reward in &outcome.rewards {
            undo.rewards
                .entry((height, crate::rewards::RewardIndex::from(reward)))
                .or_insert(None);
        }
    }
    Ok(RewardsChanges {
        outcomes: outcomes.into_values().collect(),
        undo,
    })
}

pub(crate) async fn replace<R: RewardsStorage>(
    storage: &R,
    changes: &RewardsChanges,
) -> Result<(), R::Error> {
    let desired: std::collections::BTreeMap<_, _> = changes
        .outcomes
        .iter()
        .flat_map(|outcome| {
            outcome.rewards.iter().map(move |reward| {
                (
                    (
                        outcome.target_block_height,
                        crate::rewards::RewardIndex::from(reward),
                    ),
                    reward,
                )
            })
        })
        .collect();
    for &(height, index) in changes.undo.rewards.keys() {
        storage
            .restore_pending_reward(height, index, desired.get(&(height, index)).copied())
            .await?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum PrepareRewardsError<T, C, R> {
    Engine(RewardsEngineError<T, C>),
    Storage(R),
}

#[derive(Clone, Debug)]
pub struct RewardsChanges {
    pub outcomes: Vec<RewardsOutcome>,
    pub undo: RewardsUndo,
}
