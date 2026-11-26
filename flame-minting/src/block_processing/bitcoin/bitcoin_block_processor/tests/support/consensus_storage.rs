use super::*;

impl std::ops::Deref for Consensus {
    type Target = ConsensusData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Consensus {
    pub(in super::super) fn before_read(&self) -> Result<(), &'static str> {
        if self.reject_reads.load(Ordering::SeqCst) {
            Err("undo read rejected")
        } else {
            Ok(())
        }
    }

    pub(in super::super) async fn before_write(&self, stage: ApplyStage) {
        let entry = self
            .journal
            .pending_entry()
            .await
            .unwrap()
            .unwrap()
            .operation
            .entry;
        assert_eq!(entry.minting_outcome, self.outcome);
        assert_eq!(entry.consensus_undo, self.undo);
        assert_eq!(entry.status, JournalStatus::Applying(stage));
        self.writes.lock().unwrap().push(stage);
    }

    pub(in super::super) fn after_write(&self) -> Result<(), &'static str> {
        let mut remaining = self.fail_after_write.lock().unwrap();
        if let Some(count) = remaining.as_mut() {
            *count -= 1;
            if *count == 0 {
                *remaining = None;
                return Err("interrupted after storage write");
            }
        }
        Ok(())
    }
}

impl ConsensusStorage for Consensus {
    type Error = &'static str;

    async fn get_minter_endorsement_state(
        &self,
        minter: &MinterP2wsh,
    ) -> Result<MinterEndorsementState, Self::Error> {
        self.before_read()?;
        self.inner
            .get_minter_endorsement_state(minter)
            .await
            .map_err(|_| "storage read failed")
    }

    async fn remove_endorsement_inclusion(
        &self,
        included: &IncludedEndorsement,
    ) -> Result<(), Self::Error> {
        self.inner
            .remove_endorsement_inclusion(included)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }

    async fn get_acquisition(
        &self,
        index: AcquisitionIndex,
    ) -> Result<Option<IncludedAcquisition>, Self::Error> {
        self.before_read()?;
        self.inner
            .get_acquisition(index)
            .await
            .map_err(|_| "storage read failed")
    }

    async fn get_minter_endorsement_for_height(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<WeightedEndorsement>, Self::Error> {
        self.before_read()?;
        self.inner
            .get_minter_endorsement_for_height(minter, height)
            .await
            .map_err(|_| "storage read failed")
    }

    async fn get_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
    ) -> Result<Option<IncludedEndorsement>, Self::Error> {
        self.before_read()?;
        self.inner
            .get_pending_endorsement(tip, index)
            .await
            .map_err(|_| "storage read failed")
    }

    async fn get_cumulative_weight(
        &self,
        tip: BlockTip,
    ) -> Result<Option<WeightedBlockHeader>, Self::Error> {
        self.before_read()?;
        self.inner
            .get_cumulative_weight(tip)
            .await
            .map_err(|_| "storage read failed")
    }

    async fn get_double_sign(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
    ) -> Result<Option<DoubleSign>, Self::Error> {
        self.before_read()?;
        self.inner
            .get_double_sign(minter, height)
            .await
            .map_err(|_| "storage read failed")
    }

    async fn remove_endorsement(
        &self,
        endorsement: &WeightedEndorsement,
    ) -> Result<(), Self::Error> {
        self.before_write(ApplyStage::RemoveEndorsements).await;
        self.inner
            .remove_endorsement(endorsement)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }

    async fn store_endorsement(
        &self,
        tip: BlockId,
        endorsement: &WeightedEndorsement,
    ) -> Result<(), Self::Error> {
        self.before_write(ApplyStage::StoreEndorsements).await;
        self.inner
            .store_endorsement(tip, endorsement)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }

    async fn store_acquisition(
        &self,
        tip: BlockId,
        acquisition: &IncludedAcquisition,
    ) -> Result<(), Self::Error> {
        self.before_write(ApplyStage::StoreAcquisitions).await;
        self.inner
            .store_acquisition(tip, acquisition)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }

    async fn store_cumulative_weight(
        &self,
        tip: BlockTip,
        weighted: &WeightedBlockHeader,
    ) -> Result<(), Self::Error> {
        if self
            .journal
            .pending_entry()
            .await
            .unwrap()
            .is_some_and(|entry| matches!(entry.operation.entry.status, JournalStatus::Applying(_)))
        {
            self.before_write(ApplyStage::StoreBlockWeights).await;
        }
        self.inner
            .store_cumulative_weight(tip, weighted)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }

    async fn add_double_sign(&self, sign: &DoubleSign) -> Result<(), Self::Error> {
        self.before_write(ApplyStage::StoreDoubleSigns).await;
        self.inner
            .add_double_sign(sign)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }

    async fn store_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        endorsement: &IncludedEndorsement,
    ) -> Result<(), Self::Error> {
        self.before_write(ApplyStage::StorePendingEndorsements)
            .await;
        self.inner
            .store_pending_endorsement(tip, endorsement)
            .await
            .map_err(|_| "storage write failed")?;
        self.after_write()
    }
    async fn get_block_tip_with_most_weight(&self) -> Result<Option<BlockTip>, Self::Error> {
        self.selections.fetch_add(1, Ordering::SeqCst);
        if self.reject_selection.load(Ordering::SeqCst) {
            return Err("selection rejected");
        }
        let rollback = self
            .journal
            .pending_entry()
            .await
            .unwrap()
            .is_some_and(|entry| {
                matches!(entry.operation.entry.status, JournalStatus::RollingBack(_))
            });
        Ok(Some(if rollback {
            self.previous_winner
        } else {
            self.winner
        }))
    }
    async fn restore_acquisition(
        &self,
        index: AcquisitionIndex,
        previous: Option<&IncludedAcquisition>,
    ) -> Result<(), Self::Error> {
        self.inner
            .restore_acquisition(index, previous)
            .await
            .map_err(|_| "restore failed")?;
        self.after_write()
    }
    async fn restore_endorsement(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
        previous: Option<&WeightedEndorsement>,
    ) -> Result<(), Self::Error> {
        self.inner
            .restore_endorsement(minter, height, previous)
            .await
            .map_err(|_| "restore failed")?;
        self.after_write()
    }
    async fn restore_pending_endorsement(
        &self,
        tip: CoreBlockTip,
        index: EndorsementIndex,
        previous: Option<&IncludedEndorsement>,
    ) -> Result<(), Self::Error> {
        if matches!(
            self.journal
                .pending_entry()
                .await
                .unwrap()
                .map(|entry| entry.operation.entry.status),
            Some(JournalStatus::Applying(
                ApplyStage::StorePendingEndorsements
            ))
        ) {
            self.before_write(ApplyStage::StorePendingEndorsements)
                .await;
        }
        self.inner
            .restore_pending_endorsement(tip, index, previous)
            .await
            .map_err(|_| "restore failed")?;
        self.after_write()
    }
    async fn restore_cumulative_weight(
        &self,
        tip: BlockTip,
        previous: Option<&WeightedBlockHeader>,
    ) -> Result<(), Self::Error> {
        self.inner
            .restore_cumulative_weight(tip, previous)
            .await
            .map_err(|_| "restore failed")?;
        self.after_write()
    }
    async fn restore_double_sign(
        &self,
        minter: &MinterP2wsh,
        height: CoreFlameHeight,
        previous: Option<&DoubleSign>,
    ) -> Result<(), Self::Error> {
        self.inner
            .restore_double_sign(minter, height, previous)
            .await
            .map_err(|_| "restore failed")?;
        self.after_write()
    }
}

#[derive(Clone)]
pub(in super::super) struct Consensus(pub(in super::super) Arc<ConsensusData>);

pub(in super::super) struct ConsensusData {
    pub(in super::super) winner: BlockTip,
    pub(in super::super) previous_winner: BlockTip,
    pub(in super::super) selections: AtomicUsize,
    pub(in super::super) reject_selection: AtomicBool,
    pub(in super::super) inner: InMemoryConsensusStorage,
    pub(in super::super) journal: InMemoryBitcoinJournal,
    pub(in super::super) outcome: MintingOutcome,
    pub(in super::super) undo: ConsensusUndo,
    pub(in super::super) reject_reads: AtomicBool,
    pub(in super::super) writes: Mutex<Vec<ApplyStage>>,
    pub(in super::super) fail_after_write: Mutex<Option<usize>>,
}
