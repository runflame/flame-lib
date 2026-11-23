use super::block_processor::BlockProcessor;
use crate::block_processing::BtcBlockRecords;
use crate::block_processing::journal::{FlameTransition, RewardsChanges};
use crate::block_processing::{
    canonical_chain::{self as flame, FlameOperations},
    journal::{
        ApplyStage, BitcoinBlockJournalEntry, BitcoinJournal, BitcoinJournalEntry,
        BitcoinJournalOperation, JournalStatus, OperationId, RollbackStage,
    },
};
use crate::consensus::storage::consensus_operations::ConsensusOperations;
use crate::consensus::{BitcoinRollbackInput, BitcoinRollbackPlan, EndorsementIndex};
use crate::rewards::application::rewards_changes as rewards;
use crate::{
    block_processing::bitcoin::cursor_storage::CursorStorage,
    consensus::{ConsensusEngine, ConsensusStorage, ConsensusUndoManager},
    rewards::{RewardsEngine, RewardsStorage},
};
use btc_integration::watcher::BlockId;
use flame_chain_service::ChainAccess;
use flame_storage::{CanonicalStorage, ChainStorage};
use flamechain::{BlockHeader, BlockTip, CoreFlameHeight};
use std::collections::{BTreeMap, BTreeSet};

pub struct BitcoinBlockProcessor<E, C: ConsensusStorage, H, J, S, R, K, T: ChainStorage> {
    pub(crate) engine: E,
    consensus_storage: C,
    chain: H,
    journal: J,
    canonical_storage: S,
    rewards_storage: R,
    pub(crate) cursor_storage: K,
    rewards_engine: RewardsEngine<T, C>,
}

type ProcessingResult<P, V> = Result<V, <P as BlockProcessor>::Error>;

impl<E, C, H, J, S, R, K, T> BitcoinBlockProcessor<E, C, H, J, S, R, K, T>
where
    E: ConsensusEngine,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H: ChainAccess + Send + Sync + 'static,
    H::Error: Send + 'static,
    J: BitcoinJournal,
    S: CanonicalStorage + Send + Sync + 'static,
    S::Error: Send + 'static,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    K: CursorStorage,
    T: ChainStorage + Send + Sync + 'static,
    T::Error: Send + 'static,
{
    pub fn new(
        engine: E,
        consensus_storage: C,
        chain: H,
        journal: J,
        canonical_storage: S,
        rewards_storage: R,
        cursor_storage: K,
        rewards_engine: RewardsEngine<T, C>,
    ) -> Self {
        Self {
            engine,
            consensus_storage,
            chain,
            journal,
            canonical_storage,
            rewards_storage,
            cursor_storage,
            rewards_engine,
        }
    }

    pub async fn apply(&self, block: BtcBlockRecords) -> ProcessingResult<Self, BlockId> {
        let previous = self.drive().await?;
        let requested = block.id;
        if previous == requested {
            return Ok(previous);
        }
        if previous.height.checked_add(1) != Some(requested.height)
            || previous.hash == requested.hash
        {
            return Err(BlockProcessingError::InvalidSuccessor {
                previous,
                requested,
            });
        }
        let outcome = self
            .engine
            .get_minting_outcome(block.clone())
            .await
            .map_err(BlockProcessingError::Engine)?;
        if outcome.next_btc_cursor != requested {
            return Err(BlockProcessingError::OutcomeBlockMismatch {
                requested,
                actual: outcome.next_btc_cursor,
            });
        }
        let mut undo = ConsensusUndoManager {
            consensus_storage: &self.consensus_storage,
        }
        .prepare(&outcome.consensus)
        .await
        .map_err(BlockProcessingError::Consensus)?;
        undo.included_endorsements = block
            .endorsements
            .iter()
            .cloned()
            .map(|endorsement| crate::consensus::IncludedEndorsement {
                btc_block: requested,
                endorsement,
            })
            .collect();
        self.journal
            .begin_apply(previous, &block, &outcome, &undo)
            .await
            .map_err(BlockProcessingError::Journal)?;
        self.drive().await
    }

    pub async fn resume(&self) -> ProcessingResult<Self, BlockId> {
        self.drive().await
    }

    async fn rollback_required(
        &self,
        tip: BlockId,
        current: BlockId,
    ) -> ProcessingResult<Self, bool> {
        let entry = self
            .journal
            .get_entry(tip)
            .await
            .map_err(BlockProcessingError::Journal)?
            .ok_or(BlockProcessingError::MissingEntry(tip))?
            .operation
            .entry;
        if entry.status == JournalStatus::RolledBack && current == entry.previous_btc_tip {
            return Ok(false);
        }
        if current != tip {
            return Err(BlockProcessingError::RollbackTipMismatch {
                requested: tip,
                current,
            });
        }
        if entry.status != JournalStatus::Applied {
            return Err(BlockProcessingError::UnexpectedPendingStatus(entry.status));
        }
        Ok(true)
    }

    pub async fn rollback(&self, tip: BlockId) -> ProcessingResult<Self, BlockId> {
        let current = self.drive().await?;
        if !self.rollback_required(tip, current).await? {
            return Ok(current);
        }
        let entry = self
            .journal
            .get_entry(tip)
            .await
            .map_err(BlockProcessingError::Journal)?
            .ok_or(BlockProcessingError::MissingEntry(tip))?;
        let plan = self.prepare_rollback(&entry).await?;
        self.journal
            .begin_rollback(tip, &plan)
            .await
            .map_err(BlockProcessingError::Journal)?;
        self.drive().await
    }

    async fn drive(&self) -> ProcessingResult<Self, BlockId> {
        while let Some(snapshot) = self
            .journal
            .pending_entry()
            .await
            .map_err(BlockProcessingError::Journal)?
        {
            let id = snapshot.id;
            let entry = &snapshot.operation.entry;
            match entry.status {
                JournalStatus::Applying(stage) => {
                    self.apply_stage(id, entry, stage).await?;
                    self.journal
                        .complete_apply_stage(id, stage)
                        .await
                        .map_err(BlockProcessingError::Journal)?;
                }
                JournalStatus::RollingBack(stage) => {
                    self.rollback_stage(id, &snapshot.operation, stage).await?;
                    self.journal
                        .complete_rollback_stage(id, stage)
                        .await
                        .map_err(BlockProcessingError::Journal)?;
                }
                status => return Err(BlockProcessingError::UnexpectedPendingStatus(status)),
            }
        }
        self.cursor_storage
            .get_cursor()
            .await
            .map_err(BlockProcessingError::Cursor)
    }

    async fn apply_stage(
        &self,
        id: OperationId,
        entry: &BitcoinBlockJournalEntry,
        stage: ApplyStage,
    ) -> ProcessingResult<Self, ()> {
        let outcome = &entry.minting_outcome.consensus;
        let consensus = ConsensusOperations {
            storage: &self.consensus_storage,
        };
        let flame = FlameOperations {
            consensus_storage: &self.consensus_storage,
            chain: &self.chain,
            canonical_storage: &self.canonical_storage,
        };
        match stage {
            ApplyStage::RemoveEndorsements => consensus
                .remove_endorsements(&outcome.removed_endorsements)
                .await
                .map_err(BlockProcessingError::Consensus),
            ApplyStage::StoreEndorsements => consensus
                .store_endorsements(&outcome.accepted_endorsements)
                .await
                .map_err(BlockProcessingError::Consensus),
            ApplyStage::StoreAcquisitions => consensus
                .store_acquisitions(&outcome.accepted_acquisitions)
                .await
                .map_err(BlockProcessingError::Consensus),
            ApplyStage::StoreBlockWeights => consensus
                .store_block_weights(&outcome.weighted_blocks)
                .await
                .map_err(BlockProcessingError::Consensus),
            ApplyStage::StoreDoubleSigns => consensus
                .store_double_signs(&outcome.double_signs)
                .await
                .map_err(BlockProcessingError::Consensus),
            ApplyStage::StorePendingEndorsements => {
                consensus
                    .remove_pending_endorsements(&outcome.removed_pending_endorsements)
                    .await
                    .map_err(BlockProcessingError::Consensus)?;
                consensus
                    .store_pending_endorsements(&outcome.pending_endorsements)
                    .await
                    .map_err(BlockProcessingError::Consensus)
            }
            ApplyStage::PrepareFlameTransition => {
                if entry.flame_transition.is_none() {
                    let transition = flame
                        .prepare()
                        .await
                        .map_err(BlockProcessingError::PrepareFlame)?;
                    self.journal
                        .store_flame_transition(id, &transition)
                        .await
                        .map_err(BlockProcessingError::Journal)?;
                }
                Ok(())
            }
            ApplyStage::ApplyFlameTransition => {
                let transition = entry
                    .flame_transition
                    .as_ref()
                    .ok_or(BlockProcessingError::MissingFlameTransition)?;
                flame
                    .apply(transition)
                    .await
                    .map_err(BlockProcessingError::Flame)
            }
            ApplyStage::PrepareRewards => {
                if entry.rewards_changes.is_none() {
                    let transition = entry
                        .flame_transition
                        .as_ref()
                        .ok_or(BlockProcessingError::MissingFlameTransition)?;
                    let changes = rewards::prepare(
                        &self.rewards_engine,
                        &self.rewards_storage,
                        &transition.attached_core_headers,
                        entry.btc_tip,
                    )
                    .await
                    .map_err(BlockProcessingError::PrepareRewards)?;
                    self.journal
                        .store_rewards_changes(id, &changes)
                        .await
                        .map_err(BlockProcessingError::Journal)?;
                }
                Ok(())
            }
            ApplyStage::ApplyRewards => {
                let changes = entry
                    .rewards_changes
                    .as_ref()
                    .ok_or(BlockProcessingError::MissingRewardsChanges)?;
                rewards::apply(&self.rewards_storage, changes)
                    .await
                    .map_err(BlockProcessingError::Rewards)
            }
            ApplyStage::StoreCursor => self
                .cursor_storage
                .store_cursor(entry.btc_tip)
                .await
                .map_err(BlockProcessingError::Cursor),
        }
    }
}

impl<E, C, H, J, S, R, K, T> BlockProcessor for BitcoinBlockProcessor<E, C, H, J, S, R, K, T>
where
    E: ConsensusEngine,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H: ChainAccess + Send + Sync + 'static,
    H::Error: Send + 'static,
    J: BitcoinJournal,
    S: CanonicalStorage + Send + Sync + 'static,
    S::Error: Send + 'static,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    K: CursorStorage,
    T: ChainStorage + Send + Sync + 'static,
    T::Error: Send + 'static,
{
    type Error = BlockProcessingError<
        E::Error,
        C::Error,
        H::Error,
        J::Error,
        S::Error,
        R::Error,
        K::Error,
        T::Error,
    >;
    async fn apply(&self, block: BtcBlockRecords) -> Result<BlockId, Self::Error> {
        self.apply(block).await
    }
    async fn resume(&self) -> Result<BlockId, Self::Error> {
        self.resume().await
    }
    async fn rollback(&self, tip: BlockId) -> Result<BlockId, Self::Error> {
        self.rollback(tip).await
    }
}

#[derive(Debug)]
pub enum BlockProcessingError<E, C, H, J, S, R, K, T> {
    Engine(E),
    Consensus(C),
    Journal(J),
    Rewards(R),
    Cursor(K),
    PrepareFlame(flame::PrepareFlameError<C, S, H>),
    Flame(flame::ChainPathApplierError<S, H>),
    PrepareRewards(rewards::PrepareRewardsError<T, C, R>),
    MissingEntry(BlockId),
    MissingRollbackPlan,
    Chain(H),
    MissingBlock(flamechain::BlockTip),
    RewardHeightOverflow,
    MissingFlameTransition,
    MissingRewardsChanges,
    UnexpectedPendingStatus(JournalStatus),
    InvalidSuccessor {
        previous: BlockId,
        requested: BlockId,
    },
    OutcomeBlockMismatch {
        requested: BlockId,
        actual: BlockId,
    },
    RollbackTipMismatch {
        requested: BlockId,
        current: BlockId,
    },
}

#[cfg(test)]
mod tests;

impl<E, C, H, J, S, R, K, T> BitcoinBlockProcessor<E, C, H, J, S, R, K, T>
where
    E: ConsensusEngine,
    C: ConsensusStorage + Send + Sync + 'static,
    C::Error: Send + 'static,
    H: ChainAccess + Send + Sync + 'static,
    H::Error: Send + 'static,
    J: BitcoinJournal,
    S: CanonicalStorage + Send + Sync + 'static,
    S::Error: Send + 'static,
    R: RewardsStorage + Send + Sync + 'static,
    R::Error: Send + 'static,
    K: CursorStorage,
    T: ChainStorage + Send + Sync + 'static,
    T::Error: Send + 'static,
{
    pub(super) async fn prepare_rollback(
        &self,
        snapshot: &BitcoinJournalEntry,
    ) -> ProcessingResult<Self, BitcoinRollbackPlan> {
        let entry = &snapshot.operation.entry;
        let included = entry.consensus_undo.included_endorsements.clone();
        let mut minters = BTreeSet::new();
        for included in &included {
            minters.insert(included.endorsement.auth().minter().p2wsh());
        }
        for acquisition in &entry.minting_outcome.consensus.accepted_acquisitions {
            minters.insert(acquisition.acquisition.data().minter_p2wsh);
        }
        for acquisition in entry.consensus_undo.acquisitions.values().flatten() {
            minters.insert(acquisition.acquisition.data().minter_p2wsh);
        }
        minters.extend(
            entry
                .consensus_undo
                .endorsements
                .keys()
                .map(|(minter, _)| *minter),
        );
        minters.extend(
            entry
                .consensus_undo
                .double_signs
                .keys()
                .map(|(minter, _)| *minter),
        );
        let mut states = BTreeMap::new();
        let mut remaining = self
            .journal
            .get_applied_endorsements()
            .await
            .map_err(BlockProcessingError::Journal)?;
        for minter in minters {
            let state = self
                .consensus_storage
                .get_minter_endorsement_state(&minter)
                .await
                .map_err(BlockProcessingError::Consensus)?;
            remaining.extend(
                state
                    .endorsements
                    .iter()
                    .map(|entry| entry.original.clone()),
            );
            remaining.extend(state.pending_endorsements.iter().cloned());
            remaining.extend(
                state
                    .double_signs
                    .iter()
                    .flat_map(|sign| sign.endorsements.iter().cloned()),
            );
            states.insert(minter, state);
        }
        let mut unique = BTreeMap::new();
        for included in remaining {
            if included.btc_block != entry.btc_tip
                && states.contains_key(&included.endorsement.auth().minter().p2wsh())
            {
                unique.insert(
                    (
                        included.btc_block.height,
                        included.btc_block.hash,
                        EndorsementIndex::from(&included.endorsement),
                    ),
                    included,
                );
            }
        }
        self.engine
            .get_rollback_plan(BitcoinRollbackInput {
                included_endorsements: included,
                remaining_endorsements: unique.into_values().collect(),
                minters: states,
                acquisition_undo: entry.consensus_undo.acquisitions.clone(),
            })
            .await
            .map_err(BlockProcessingError::Engine)
    }

    pub(super) async fn rollback_stage(
        &self,
        id: OperationId,
        operation: &BitcoinJournalOperation,
        stage: RollbackStage,
    ) -> ProcessingResult<Self, ()> {
        let entry = &operation.entry;
        let plan = operation
            .rollback_plan
            .as_ref()
            .ok_or(BlockProcessingError::MissingRollbackPlan)?;
        match stage {
            RollbackStage::RemoveEndorsements => {
                for included in &plan.included_endorsements {
                    self.consensus_storage
                        .remove_endorsement_inclusion(included)
                        .await
                        .map_err(BlockProcessingError::Consensus)?;
                }
            }
            RollbackStage::RestoreAcquisitions => {
                for (&index, previous) in &plan.acquisition_undo {
                    self.consensus_storage
                        .restore_acquisition(index, previous.as_ref())
                        .await
                        .map_err(BlockProcessingError::Consensus)?;
                }
            }
            RollbackStage::StoreDoubleSigns => {
                for (&(minter, height), desired) in &plan.double_signs {
                    self.consensus_storage
                        .restore_double_sign(&minter, height, desired.as_ref())
                        .await
                        .map_err(BlockProcessingError::Consensus)?;
                }
            }
            RollbackStage::StoreEndorsements => {
                for (&(minter, height), desired) in &plan.endorsements {
                    self.consensus_storage
                        .restore_endorsement(&minter, height, desired.as_ref())
                        .await
                        .map_err(BlockProcessingError::Consensus)?;
                }
            }
            RollbackStage::StorePendingEndorsements => {
                for (&(tip, index), desired) in &plan.pending_endorsements {
                    self.consensus_storage
                        .restore_pending_endorsement(tip, index, desired.as_ref())
                        .await
                        .map_err(BlockProcessingError::Consensus)?;
                }
            }
            RollbackStage::StoreBlockWeights => {
                for (&tip, desired) in &plan.weighted_blocks {
                    self.consensus_storage
                        .store_cumulative_weight(tip, desired)
                        .await
                        .map_err(BlockProcessingError::Consensus)?;
                }
            }
            RollbackStage::PrepareFlameTransition => {
                if entry.flame_transition.is_none() {
                    let transition = self
                        .flame()
                        .prepare()
                        .await
                        .map_err(BlockProcessingError::PrepareFlame)?;
                    self.journal
                        .store_flame_transition(id, &transition)
                        .await
                        .map_err(BlockProcessingError::Journal)?;
                }
            }
            RollbackStage::ApplyFlameTransition => {
                let transition = entry
                    .flame_transition
                    .as_ref()
                    .ok_or(BlockProcessingError::MissingFlameTransition)?;
                self.flame()
                    .apply(transition)
                    .await
                    .map_err(BlockProcessingError::Flame)?;
            }
            RollbackStage::PrepareRewards => {
                if entry.rewards_changes.is_none() {
                    let transition = entry
                        .flame_transition
                        .as_ref()
                        .ok_or(BlockProcessingError::MissingFlameTransition)?;
                    let changes = self
                        .prepare_rollback_rewards(transition, plan, entry.previous_btc_tip)
                        .await?;
                    self.journal
                        .store_rewards_changes(id, &changes)
                        .await
                        .map_err(BlockProcessingError::Journal)?;
                }
            }
            RollbackStage::ApplyRewards => {
                let changes = entry
                    .rewards_changes
                    .as_ref()
                    .ok_or(BlockProcessingError::MissingRewardsChanges)?;
                rewards::replace(&self.rewards_storage, changes)
                    .await
                    .map_err(BlockProcessingError::Rewards)?;
            }
            RollbackStage::StoreCursor => {
                self.cursor_storage
                    .store_cursor(entry.previous_btc_tip)
                    .await
                    .map_err(BlockProcessingError::Cursor)?;
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

    async fn prepare_rollback_rewards(
        &self,
        transition: &FlameTransition,
        plan: &BitcoinRollbackPlan,
        btc_tip: BlockId,
    ) -> ProcessingResult<Self, RewardsChanges> {
        let mut targets = BTreeSet::new();
        let heights = plan
            .endorsements
            .keys()
            .map(|(_, height)| *height)
            .chain(plan.double_signs.keys().map(|(_, height)| *height))
            .chain(plan.pending_endorsements.keys().map(|(tip, _)| tip.height))
            .chain(
                plan.included_endorsements
                    .iter()
                    .map(|entry| entry.block_height()),
            );
        for height in heights {
            let Some(height) = height.as_u32().checked_sub(1) else {
                continue;
            };
            let target = (height / 100)
                .checked_mul(100)
                .and_then(|height| height.checked_add(200))
                .ok_or(BlockProcessingError::RewardHeightOverflow)?;
            targets.insert(CoreFlameHeight::from(target));
        }
        for header in &transition.attached_core_headers {
            if let Some(target) = Self::scheduled_reward_target(header)? {
                targets.insert(target);
            }
        }
        for &tip in &transition.path.detach {
            let block = self
                .chain
                .get_block(tip)
                .await
                .map_err(BlockProcessingError::Chain)?
                .ok_or(BlockProcessingError::MissingBlock(tip))?;
            if let Some(target) = Self::scheduled_reward_target(&block.header)? {
                targets.insert(target);
            }
        }
        let mut headers = Vec::new();
        if !targets.is_empty() {
            let mut tip = transition.next_tip;
            while tip.height.as_u64() > 0 {
                let block = self
                    .chain
                    .get_block(tip)
                    .await
                    .map_err(BlockProcessingError::Chain)?
                    .ok_or(BlockProcessingError::MissingBlock(tip))?;
                let header = &block.header;
                if Self::scheduled_reward_target(header)?
                    .is_some_and(|target| targets.contains(&target))
                {
                    headers.push(header.clone());
                }
                tip = BlockTip {
                    hash: header.parent,
                    height: (tip.height.as_u64() - 1).into(),
                };
            }
        }
        rewards::prepare_replacement(
            &self.rewards_engine,
            &self.rewards_storage,
            &headers,
            &targets,
            btc_tip,
        )
        .await
        .map_err(BlockProcessingError::PrepareRewards)
    }
    fn scheduled_reward_target(
        header: &BlockHeader,
    ) -> ProcessingResult<Self, Option<CoreFlameHeight>> {
        let Some(height) = header
            .core_block
            .as_ref()
            .map(|core| core.height.as_u32())
            .filter(|height| *height >= 150 && height % 100 == 50)
        else {
            return Ok(None);
        };
        height
            .checked_add(50)
            .map(|height| Some(height.into()))
            .ok_or(BlockProcessingError::RewardHeightOverflow)
    }
}
