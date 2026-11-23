use crate::block_processing::{
    FlameProcessor,
    canonical_chain::{self as flame, FlameOperations},
    journal::{
        FlameJournal, FlameJournalOperation, OperationId,
        flame::{FlameApplyStage, FlameJournalStatus},
    },
};
use crate::consensus::storage::consensus_operations::ConsensusOperations;
use crate::rewards::application::rewards_changes as rewards;
use crate::{
    block_processing::bitcoin::cursor_storage::CursorStorage,
    consensus::{ConsensusEngine, ConsensusOutcome, ConsensusStorage, ConsensusUndoManager},
    rewards::{RewardsEngine, RewardsStorage},
};
use btc_integration::watcher::BlockId;
use flame_chain_service::ChainAccess;
use flame_storage::{CanonicalStorage, ChainStorage};
use flamechain::{BlockTip, CoreBlockTip};

pub struct FlameBlockProcessor<E, C: ConsensusStorage, H, S, R, K, T: ChainStorage, J> {
    engine: E,
    consensus_storage: C,
    chain: H,
    canonical_storage: S,
    rewards_storage: R,
    cursor_storage: K,
    rewards_engine: RewardsEngine<T, C>,
    journal: J,
}

type ProcessingResult<P, V = ()> = Result<V, <P as FlameProcessor>::Error>;

impl<E, C, H, S, R, K, T, J> FlameBlockProcessor<E, C, H, S, R, K, T, J>
where
    E: ConsensusEngine,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H: ChainAccess + Send + Sync + 'static,
    H::Error: Send + 'static,
    S: CanonicalStorage + Send + Sync + 'static,
    S::Error: Send + 'static,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    K: CursorStorage,
    T: ChainStorage + Send + Sync + 'static,
    T::Error: Send + 'static,
    J: FlameJournal,
{
    pub fn new(
        engine: E,
        consensus_storage: C,
        chain: H,
        canonical_storage: S,
        rewards_storage: R,
        cursor_storage: K,
        rewards_engine: RewardsEngine<T, C>,
        journal: J,
    ) -> Self {
        Self {
            engine,
            consensus_storage,
            chain,
            canonical_storage,
            rewards_storage,
            cursor_storage,
            rewards_engine,
            journal,
        }
    }

    async fn on_flame_block_arrival(&self, tip: BlockTip) -> ProcessingResult<Self> {
        self.drive().await?;
        self.process_arrival(tip).await
    }

    async fn process_arrival(&self, tip: BlockTip) -> ProcessingResult<Self> {
        let block = self
            .chain
            .get_block(tip)
            .await
            .map_err(FlameBlockProcessingError::Chain)?
            .ok_or(FlameBlockProcessingError::MissingBlock(tip))?;
        let actual = block.header.block_tip();
        if actual != tip {
            return Err(FlameBlockProcessingError::BlockTipMismatch {
                requested: tip,
                actual,
            });
        }
        let Some(core) = &block.header.core_block else {
            return Ok(());
        };
        let pending = self
            .consensus_storage
            .get_pending_endorsements_for_block(CoreBlockTip {
                hash: tip.hash,
                height: core.height,
            })
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;
        if pending.is_empty() {
            return Ok(());
        }
        let outcome = self
            .engine
            .get_endorsements_outcome(pending)
            .await
            .map_err(FlameBlockProcessingError::Engine)?;
        let undo = ConsensusUndoManager {
            consensus_storage: &self.consensus_storage,
        }
        .prepare(&outcome)
        .await
        .map_err(FlameBlockProcessingError::Consensus)?;
        let btc_tip = self
            .cursor_storage
            .get_cursor()
            .await
            .map_err(FlameBlockProcessingError::Cursor)?;
        self.journal
            .begin_apply(tip, btc_tip, &outcome, &undo)
            .await
            .map_err(FlameBlockProcessingError::Journal)?;
        self.drive().await
    }

    async fn apply_consensus(&self, outcome: &ConsensusOutcome) -> ProcessingResult<Self> {
        let consensus = ConsensusOperations {
            storage: &self.consensus_storage,
        };
        consensus
            .remove_endorsements(&outcome.removed_endorsements)
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;
        consensus
            .store_endorsements(&outcome.accepted_endorsements)
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;
        consensus
            .store_acquisitions(&outcome.accepted_acquisitions)
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;
        consensus
            .store_block_weights(&outcome.weighted_blocks)
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;
        consensus
            .store_double_signs(&outcome.double_signs)
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;
        consensus
            .store_pending_endorsements(&outcome.pending_endorsements)
            .await
            .map_err(FlameBlockProcessingError::Consensus)?;

        Ok(())
    }

    async fn resume(&self) -> ProcessingResult<Self> {
        self.drive().await
    }

    async fn drive(&self) -> ProcessingResult<Self> {
        while let Some(snapshot) = self
            .journal
            .pending_entry()
            .await
            .map_err(FlameBlockProcessingError::Journal)?
        {
            let id = snapshot.id;
            let entry = snapshot.operation;
            let cursor = self
                .cursor_storage
                .get_cursor()
                .await
                .map_err(FlameBlockProcessingError::Cursor)?;
            if cursor != entry.btc_tip {
                return Err(FlameBlockProcessingError::CursorMismatch {
                    expected: entry.btc_tip,
                    actual: cursor,
                });
            }
            match entry.status {
                FlameJournalStatus::Applying(stage) => {
                    self.apply_stage(id, &entry, stage).await?;
                    self.journal
                        .complete_apply_stage(id, stage)
                        .await
                        .map_err(FlameBlockProcessingError::Journal)?;
                }
                status => return Err(FlameBlockProcessingError::UnexpectedStatus(status)),
            }
        }
        Ok(())
    }

    fn flame(&self) -> FlameOperations<'_, C, H, S> {
        FlameOperations {
            consensus_storage: &self.consensus_storage,
            chain: &self.chain,
            canonical_storage: &self.canonical_storage,
        }
    }

    async fn apply_stage(
        &self,
        id: OperationId,
        entry: &FlameJournalOperation,
        stage: FlameApplyStage,
    ) -> ProcessingResult<Self> {
        match stage {
            FlameApplyStage::Consensus => self.apply_consensus(&entry.outcome).await,
            FlameApplyStage::PrepareTransition => {
                if entry.transition.is_none() {
                    let transition = self
                        .flame()
                        .prepare()
                        .await
                        .map_err(FlameBlockProcessingError::PrepareFlame)?;
                    self.journal
                        .store_transition(id, &transition)
                        .await
                        .map_err(FlameBlockProcessingError::Journal)?;
                }
                Ok(())
            }
            FlameApplyStage::ApplyTransition => {
                let transition = entry
                    .transition
                    .as_ref()
                    .ok_or(FlameBlockProcessingError::MissingTransition)?;
                self.flame()
                    .apply(transition)
                    .await
                    .map_err(FlameBlockProcessingError::Flame)
            }
            FlameApplyStage::PrepareRewards => {
                if entry.rewards.is_none() {
                    let transition = entry
                        .transition
                        .as_ref()
                        .ok_or(FlameBlockProcessingError::MissingTransition)?;
                    let changes = rewards::prepare(
                        &self.rewards_engine,
                        &self.rewards_storage,
                        &transition.attached_core_headers,
                        entry.btc_tip,
                    )
                    .await
                    .map_err(FlameBlockProcessingError::PrepareRewards)?;
                    self.journal
                        .store_rewards(id, &changes)
                        .await
                        .map_err(FlameBlockProcessingError::Journal)?;
                }
                Ok(())
            }
            FlameApplyStage::ApplyRewards => {
                let changes = entry
                    .rewards
                    .as_ref()
                    .ok_or(FlameBlockProcessingError::MissingRewards)?;
                rewards::apply(&self.rewards_storage, changes)
                    .await
                    .map_err(FlameBlockProcessingError::Rewards)
            }
            FlameApplyStage::RemovePending => ConsensusOperations {
                storage: &self.consensus_storage,
            }
            .remove_pending_endorsements(&entry.outcome.removed_pending_endorsements)
            .await
            .map_err(FlameBlockProcessingError::Consensus),
        }
    }
}

impl<E, C, H, S, R, K, T, J> FlameProcessor for FlameBlockProcessor<E, C, H, S, R, K, T, J>
where
    E: ConsensusEngine,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H: ChainAccess + Send + Sync + 'static,
    H::Error: Send + 'static,
    S: CanonicalStorage + Send + Sync + 'static,
    S::Error: Send + 'static,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    K: CursorStorage,
    T: ChainStorage + Send + Sync + 'static,
    T::Error: Send + 'static,
    J: FlameJournal,
{
    type Error = FlameBlockProcessingError<
        E::Error,
        C::Error,
        H::Error,
        S::Error,
        R::Error,
        K::Error,
        T::Error,
        J::Error,
    >;

    async fn process(&self, tip: BlockTip) -> Result<(), Self::Error> {
        self.on_flame_block_arrival(tip).await
    }
    async fn resume(&self) -> Result<(), Self::Error> {
        self.resume().await
    }
}

#[derive(Debug)]
pub enum FlameBlockProcessingError<E, C, H, S, R, K, T, J> {
    Engine(E),
    Journal(J),
    CursorMismatch {
        expected: BlockId,
        actual: BlockId,
    },
    UnexpectedStatus(FlameJournalStatus),
    MissingTransition,
    MissingRewards,
    Consensus(C),
    Chain(H),
    Cursor(K),
    PrepareFlame(flame::PrepareFlameError<C, S, H>),
    Flame(flame::ChainPathApplierError<S, H>),
    PrepareRewards(rewards::PrepareRewardsError<T, C, R>),
    Rewards(R),
    MissingBlock(BlockTip),
    BlockTipMismatch {
        requested: BlockTip,
        actual: BlockTip,
    },
}

#[cfg(test)]
mod tests;
