use super::*;

impl ConsensusEngine for Engine {
    type Error = &'static str;

    async fn get_rollback_plan(
        &self,
        input: BitcoinRollbackInput,
    ) -> Result<BitcoinRollbackPlan, Self::Error> {
        self.rollback_calls.fetch_add(1, Ordering::SeqCst);
        let mut plan = self.rollback.clone();
        plan.included_endorsements = input.included_endorsements;
        plan.acquisition_undo = input.acquisition_undo;
        Ok(plan)
    }

    async fn get_endorsements_outcome(
        &self,
        _: Vec<IncludedEndorsement>,
    ) -> Result<ConsensusOutcome, Self::Error> {
        panic!("unexpected endorsement processing")
    }

    async fn get_minting_outcome(
        &self,
        block: BtcBlockRecords,
    ) -> Result<MintingOutcome, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        assert_eq!(block.id, self.outcome.next_btc_cursor);
        Ok(self.outcome.clone())
    }
}

pub(in super::super) struct Engine {
    pub(in super::super) rollback: BitcoinRollbackPlan,
    pub(in super::super) rollback_calls: AtomicUsize,
    pub(in super::super) outcome: MintingOutcome,
    pub(in super::super) calls: AtomicUsize,
}
