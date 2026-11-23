use super::*;

impl Journal {
    pub(in super::super) fn check(&self, point: Point, after: bool) -> Result<(), &'static str> {
        let mut fault = self.fault.lock().unwrap();
        if *fault == Some((point, after)) {
            *fault = None;
            return Err("journal interrupted");
        }
        Ok(())
    }
}

impl BitcoinJournal for Journal {
    type Error = &'static str;

    async fn begin_apply(
        &self,
        previous: BlockId,
        block: &BtcBlockRecords,
        outcome: &MintingOutcome,
        undo: &ConsensusUndo,
    ) -> Result<OperationId, Self::Error> {
        self.check(Point::Begin, false)?;
        let id = self
            .inner
            .begin_apply(previous, block, outcome, undo)
            .await
            .map_err(|_| "journal operation failed")?;
        self.check(Point::Begin, true)?;
        Ok(id)
    }

    async fn store_flame_transition(
        &self,
        id: OperationId,
        transition: &FlameTransition,
    ) -> Result<(), Self::Error> {
        self.check(Point::FlamePlan, false)?;
        self.inner
            .store_flame_transition(id, transition)
            .await
            .map_err(|_| "journal operation failed")?;
        self.check(Point::FlamePlan, true)
    }

    async fn store_rewards_changes(
        &self,
        id: OperationId,
        changes: &RewardsChanges,
    ) -> Result<(), Self::Error> {
        self.check(Point::RewardsPlan, false)?;
        self.inner
            .store_rewards_changes(id, changes)
            .await
            .map_err(|_| "journal operation failed")?;
        self.check(Point::RewardsPlan, true)
    }

    async fn complete_apply_stage(
        &self,
        id: OperationId,
        stage: ApplyStage,
    ) -> Result<(), Self::Error> {
        self.check(Point::Apply(stage), false)?;
        self.inner
            .complete_apply_stage(id, stage)
            .await
            .map_err(|_| "journal operation failed")?;
        self.check(Point::Apply(stage), true)
    }

    async fn begin_rollback(
        &self,
        tip: BlockId,
        plan: &BitcoinRollbackPlan,
    ) -> Result<OperationId, Self::Error> {
        self.check(Point::BeginRollback, false)?;
        let id = self
            .inner
            .begin_rollback(tip, plan)
            .await
            .map_err(|_| "journal operation failed")?;
        self.check(Point::BeginRollback, true)?;
        Ok(id)
    }

    async fn complete_rollback_stage(
        &self,
        id: OperationId,
        stage: RollbackStage,
    ) -> Result<(), Self::Error> {
        self.check(Point::Restore(stage), false)?;
        self.inner
            .complete_rollback_stage(id, stage)
            .await
            .map_err(|_| "journal operation failed")?;
        self.check(Point::Restore(stage), true)
    }

    async fn get_applied_endorsements(&self) -> Result<Vec<IncludedEndorsement>, Self::Error> {
        self.inner
            .get_applied_endorsements()
            .await
            .map_err(|_| "history failed")
    }

    async fn get_entry(&self, tip: BlockId) -> Result<Option<BitcoinJournalEntry>, Self::Error> {
        self.inner.get_entry(tip).await.map_err(|_| "entry failed")
    }

    async fn pending_entry(&self) -> Result<Option<BitcoinJournalEntry>, Self::Error> {
        self.inner
            .pending_entry()
            .await
            .map_err(|_| "pending failed")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Point {
    Begin,
    BeginRollback,
    FlamePlan,
    RewardsPlan,
    Apply(ApplyStage),
    Restore(RollbackStage),
}

pub(in super::super) struct Journal {
    pub(in super::super) inner: InMemoryBitcoinJournal,
    pub(in super::super) fault: Mutex<Option<(Point, bool)>>,
}
