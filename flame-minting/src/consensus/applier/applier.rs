use std::collections::{BTreeMap, HashMap};
use std::future::Future;

use flame_chain_service::{ChainAccess, ChainPath};
use flame_storage::CanonicalStorage;
use flamechain::{BlockTip, CoreBlockTip};

use crate::consensus::{
    ConsensusStorage, DoubleSign, IncludedAcquisition, MintingOutcome, PendingVotes,
    WeightedBlockHeader, WeightedVote,
};

use super::{BlockAttacher, BlockDetacher, MintingJournal};

#[repr(u64)]
enum ApplyIntent {
    RemoveVotes = 1,
    StoreVotes = 2,
    StoreAcquisitions = 3,
    StoreBlockWeights = 4,
    StoreDoubleSigns = 5,
    StorePendingVotes = 6,
    ApplyChainPath = 7,
    StoreBtcCursor = 8,
}

pub struct MintingOutcomeApplier<
    C: ConsensusStorage,
    H: ChainAccess,
    J: MintingJournal,
    S: CanonicalStorage,
> {
    pub consensus_storage: C,
    pub chain: H,
    pub journal: J,
    pub canonical_storage: S,
}

impl<C: ConsensusStorage, H: ChainAccess, J: MintingJournal, S: CanonicalStorage>
    MintingOutcomeApplier<C, H, J, S>
{
    pub async fn apply(
        &mut self,
        outcome: &MintingOutcome,
    ) -> Result<(), MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>> {
        self.journal
            .write_pending_outcome(outcome)
            .await
            .map_err(MintingOutcomeApplierError::Journal)?;

        self.apply_outcome(outcome).await
    }

    pub async fn resume(
        &mut self,
    ) -> Result<(), MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>> {
        let outcome = self
            .journal
            .get_pending_outcome()
            .await
            .map_err(MintingOutcomeApplierError::Journal)?;
        if let Some(outcome) = outcome {
            self.apply_outcome(&outcome).await?;
        }

        Ok(())
    }

    async fn apply_outcome(
        &self,
        outcome: &MintingOutcome,
    ) -> Result<(), MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>> {
        self.with_intent(ApplyIntent::RemoveVotes as u64, || async {
            self.remove_votes(&outcome.removed_votes)
                .await
                .map_err(MintingOutcomeApplierError::ConsensusStorage)
        })
        .await?;
        self.with_intent(ApplyIntent::StoreVotes as u64, || async {
            self.store_votes(&outcome.accepted_votes)
                .await
                .map_err(MintingOutcomeApplierError::ConsensusStorage)
        })
        .await?;
        self.with_intent(ApplyIntent::StoreAcquisitions as u64, || async {
            self.store_acquisitions(&outcome.accepted_acquisitions)
                .await
                .map_err(MintingOutcomeApplierError::ConsensusStorage)
        })
        .await?;
        self.with_intent(ApplyIntent::StoreBlockWeights as u64, || async {
            self.store_block_weights(&outcome.weighted_blocks)
                .await
                .map_err(MintingOutcomeApplierError::ConsensusStorage)
        })
        .await?;
        self.with_intent(ApplyIntent::StoreDoubleSigns as u64, || async {
            self.store_double_signs(&outcome.double_signs)
                .await
                .map_err(MintingOutcomeApplierError::ConsensusStorage)
        })
        .await?;
        self.with_intent(ApplyIntent::StorePendingVotes as u64, || async {
            self.store_pending_votes(&outcome.pending_votes)
                .await
                .map_err(MintingOutcomeApplierError::ConsensusStorage)
        })
        .await?;
        self.with_intent(ApplyIntent::ApplyChainPath as u64, || async {
            let path = self.get_chain_path().await?;
            self.apply_chain_path(&path);
            Ok(())
        })
        .await?;

        self.with_intent(ApplyIntent::StoreBtcCursor as u64, || async {
            self.canonical_storage
                .store_btc_cursor(outcome.next_btc_cursor)
                .await
                .map_err(MintingOutcomeApplierError::CanonicalStorage)
        })
        .await?;
        self.journal
            .mark_outcome_applied(outcome.next_btc_cursor)
            .await
            .map_err(MintingOutcomeApplierError::Journal)?;

        Ok(())
    }

    async fn with_intent<F, Fut>(
        &self,
        intent: u64,
        function: F,
    ) -> Result<(), MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<
            Output = Result<(), MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>>,
        >,
    {
        let current = self
            .journal
            .get_intent()
            .await
            .map_err(MintingOutcomeApplierError::Journal)?;
        if let Some((current, ended)) = current {
            if current > intent || (current == intent && ended) {
                return Ok(());
            }
        }

        self.journal
            .write_intent(intent, false)
            .await
            .map_err(MintingOutcomeApplierError::Journal)?;
        function().await?;
        self.journal
            .write_intent(intent, true)
            .await
            .map_err(MintingOutcomeApplierError::Journal)
    }

    fn apply_chain_path(&self, path: &ChainPath) {
        let detacher = BlockDetacher;
        for &block_tip in &path.detach {
            detacher.detach_block(block_tip);
        }

        let attacher = BlockAttacher;
        for &block_tip in &path.attach {
            attacher.attach_block(block_tip);
        }
    }

    async fn remove_votes(&self, votes: &[WeightedVote]) -> Result<(), C::Error> {
        for vote in votes {
            self.consensus_storage.remove_vote(vote).await?;
        }

        Ok(())
    }

    async fn store_votes(&self, votes: &[WeightedVote]) -> Result<(), C::Error> {
        for vote in votes {
            self.consensus_storage
                .store_vote(vote.original.btc_block, vote)
                .await?;
        }

        Ok(())
    }

    async fn store_acquisitions(
        &self,
        acquisitions: &[IncludedAcquisition],
    ) -> Result<(), C::Error> {
        for acquisition in acquisitions {
            self.consensus_storage
                .store_acquisition(acquisition.btc_block, acquisition)
                .await?;
        }

        Ok(())
    }

    async fn store_block_weights(
        &self,
        weighted_blocks: &HashMap<BlockTip, WeightedBlockHeader>,
    ) -> Result<(), C::Error> {
        for (&tip, weighted_block) in weighted_blocks {
            self.consensus_storage
                .store_cumulative_weight(tip, weighted_block)
                .await?;
        }

        Ok(())
    }

    async fn store_double_signs(&self, double_signs: &[DoubleSign]) -> Result<(), C::Error> {
        for double_sign in double_signs {
            self.consensus_storage.add_double_sign(double_sign).await?;
        }

        Ok(())
    }

    async fn store_pending_votes(
        &self,
        pending_votes: &BTreeMap<CoreBlockTip, PendingVotes>,
    ) -> Result<(), C::Error> {
        for (&tip, votes) in pending_votes {
            for vote in votes {
                self.consensus_storage.store_pending_vote(tip, vote).await?;
            }
        }

        Ok(())
    }

    async fn get_chain_path(
        &self,
    ) -> Result<ChainPath, MintingOutcomeApplierError<C::Error, J::Error, S::Error, H::Error>> {
        let state_tip = self
            .canonical_storage
            .get_tip()
            .await
            .map_err(MintingOutcomeApplierError::CanonicalStorage)?
            .ok_or(MintingOutcomeApplierError::MissingStateTip)?;
        let consensus_tip = self
            .consensus_storage
            .get_block_tip_with_most_weight()
            .await
            .map_err(MintingOutcomeApplierError::ConsensusStorage)?
            .ok_or(MintingOutcomeApplierError::MissingConsensusTip)?;

        self.chain
            .get_chain_path(state_tip, consensus_tip)
            .await
            .map_err(MintingOutcomeApplierError::ChainAccess)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MintingOutcomeApplierError<C, J, S, H> {
    ConsensusStorage(C),
    Journal(J),
    CanonicalStorage(S),
    ChainAccess(H),
    MissingStateTip,
    MissingConsensusTip,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use btc_integration::{BtcBlockTip, MinterP2wsh};
    use flame_chain_service::{ChangesOutcome, ImportOutcome};
    use flamechain::{Block, BlockHash, CoreFlameHeight};

    use crate::consensus::{IncludedVote, MinterAcquisitions, MintingProtocolParams};

    type TestApplier = MintingOutcomeApplier<Storage, Chain, Journal, Storage>;
    type TestError =
        MintingOutcomeApplierError<&'static str, &'static str, &'static str, &'static str>;

    fn applier() -> TestApplier {
        MintingOutcomeApplier {
            consensus_storage: Storage,
            chain: Chain,
            journal: Journal::default(),
            canonical_storage: Storage,
        }
    }

    #[derive(Default)]
    struct JournalState {
        intent: Option<(u64, bool)>,
        fail_write: Option<(u64, bool)>,
    }

    #[derive(Default)]
    struct Journal(Mutex<JournalState>);

    impl MintingJournal for Journal {
        type Error = &'static str;

        async fn write_pending_outcome(&self, _: &MintingOutcome) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn get_pending_outcome(&self) -> Result<Option<MintingOutcome>, Self::Error> {
            unreachable!()
        }

        async fn get_intent(&self) -> Result<Option<(u64, bool)>, Self::Error> {
            Ok(self.0.lock().unwrap().intent)
        }

        async fn write_intent(&self, intent: u64, ended: bool) -> Result<(), Self::Error> {
            let mut state = self.0.lock().unwrap();
            if state.fail_write == Some((intent, ended)) {
                state.fail_write = None;
                return Err("intent write failed");
            }
            state.intent = Some((intent, ended));
            Ok(())
        }

        async fn mark_outcome_applied(&self, _: BtcBlockTip) -> Result<(), Self::Error> {
            unreachable!()
        }
    }

    struct Storage;

    impl CanonicalStorage for Storage {
        type State = ();
        type Error = &'static str;

        async fn get_tip(&self) -> Result<Option<BlockTip>, Self::Error> {
            unreachable!()
        }

        async fn store_btc_cursor(&self, _: BtcBlockTip) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn get_state(&self) -> Result<Option<(BlockHash, ())>, Self::Error> {
            unreachable!()
        }

        async fn commit_state(&self, _: BlockHash, _: &()) -> Result<(), Self::Error> {
            unreachable!()
        }
    }

    impl ConsensusStorage for Storage {
        type Error = &'static str;

        async fn get_block_tip_with_most_weight(&self) -> Result<Option<BlockTip>, Self::Error> {
            unreachable!()
        }

        async fn get_cumulative_weight(
            &self,
            _: BlockTip,
        ) -> Result<Option<WeightedBlockHeader>, Self::Error> {
            unreachable!()
        }

        async fn store_cumulative_weight(
            &self,
            _: BlockTip,
            _: &WeightedBlockHeader,
        ) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn store_acquisition(
            &self,
            _: BtcBlockTip,
            _: &IncludedAcquisition,
        ) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn get_acquisitions_by_minters(
            &self,
            _: u64,
        ) -> Result<HashMap<MinterP2wsh, MinterAcquisitions>, Self::Error> {
            unreachable!()
        }

        async fn get_active_minter_acquisitions_at_height(
            &self,
            _: &MinterP2wsh,
            _: u64,
            _: &MintingProtocolParams,
        ) -> Result<MinterAcquisitions, Self::Error> {
            unreachable!()
        }

        async fn store_vote(&self, _: BtcBlockTip, _: &WeightedVote) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn remove_vote(&self, _: &WeightedVote) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn store_pending_vote(
            &self,
            _: CoreBlockTip,
            _: &IncludedVote,
        ) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn get_minter_vote_for_height(
            &self,
            _: &MinterP2wsh,
            _: CoreFlameHeight,
        ) -> Result<Option<WeightedVote>, Self::Error> {
            unreachable!()
        }

        async fn add_double_sign(&self, _: &DoubleSign) -> Result<(), Self::Error> {
            unreachable!()
        }

        async fn get_double_sign(
            &self,
            _: &MinterP2wsh,
            _: CoreFlameHeight,
        ) -> Result<Option<DoubleSign>, Self::Error> {
            unreachable!()
        }

        async fn is_minter_double_signed(&self, _: &MinterP2wsh) -> Result<bool, Self::Error> {
            unreachable!()
        }
    }

    struct Chain;

    impl ChainAccess for Chain {
        type Error = &'static str;

        async fn get_chain_path(&self, _: BlockTip, _: BlockTip) -> Result<ChainPath, Self::Error> {
            unreachable!()
        }

        async fn import_block(&mut self, _: Block) -> Result<ImportOutcome, Self::Error> {
            unreachable!()
        }

        async fn select_tip(&mut self, _: BlockHash) -> Result<ChangesOutcome, Self::Error> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn skips_completed_steps_without_calling_the_factory() {
        let applier = applier();
        for current in [(4, false), (4, true), (3, true)] {
            applier.journal.0.lock().unwrap().intent = Some(current);
            applier
                .with_intent(3, || -> std::future::Ready<Result<(), TestError>> {
                    panic!("skipped step called")
                })
                .await
                .unwrap();
            assert_eq!(applier.journal.get_intent().await.unwrap(), Some(current));
        }
    }

    #[tokio::test]
    async fn starts_new_steps_and_retries_unfinished_steps() {
        let applier = applier();
        for current in [None, Some((2, true)), Some((2, false)), Some((3, false))] {
            applier.journal.0.lock().unwrap().intent = current;
            let mut called = false;
            applier
                .with_intent(3, || async {
                    assert_eq!(
                        applier.journal.get_intent().await.unwrap(),
                        Some((3, false))
                    );
                    called = true;
                    Ok(())
                })
                .await
                .unwrap();
            assert!(called);
            assert_eq!(applier.journal.get_intent().await.unwrap(), Some((3, true)));
        }
    }

    #[tokio::test]
    async fn failed_start_record_prevents_the_step() {
        let applier = applier();
        applier.journal.0.lock().unwrap().fail_write = Some((3, false));
        let result = applier
            .with_intent(3, || -> std::future::Ready<Result<(), TestError>> {
                panic!("unrecorded step called")
            })
            .await;
        assert_eq!(
            result,
            Err(MintingOutcomeApplierError::Journal("intent write failed"))
        );
        assert_eq!(applier.journal.get_intent().await.unwrap(), None);
    }

    #[tokio::test]
    async fn failed_step_remains_unfinished() {
        let applier = applier();
        let result = applier
            .with_intent(3, || async {
                Err(MintingOutcomeApplierError::ConsensusStorage("step failed"))
            })
            .await;
        assert_eq!(
            result,
            Err(MintingOutcomeApplierError::ConsensusStorage("step failed"))
        );
        assert_eq!(
            applier.journal.get_intent().await.unwrap(),
            Some((3, false))
        );
    }

    #[tokio::test]
    async fn replays_a_step_if_its_completion_record_failed() {
        let applier = applier();
        applier.journal.0.lock().unwrap().fail_write = Some((3, true));
        let mut writes = 0;
        let mut value = None;
        let result = applier
            .with_intent(3, || async {
                writes += 1;
                value = Some(42);
                Ok(())
            })
            .await;
        assert_eq!(
            result,
            Err(MintingOutcomeApplierError::Journal("intent write failed"))
        );
        assert_eq!(value, Some(42));
        assert_eq!(
            applier.journal.get_intent().await.unwrap(),
            Some((3, false))
        );
        applier
            .with_intent(3, || async {
                writes += 1;
                value = Some(42);
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(writes, 2);
        assert_eq!(value, Some(42));
    }
}
