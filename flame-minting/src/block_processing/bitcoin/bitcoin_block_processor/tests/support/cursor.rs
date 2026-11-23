use super::*;

impl CursorStorage for Cursor {
    type Error = &'static str;
    async fn get_cursor(&self) -> Result<BlockId, Self::Error> {
        self.inner
            .get_cursor()
            .await
            .map_err(|_| "cursor read failed")
    }
    async fn store_cursor(&self, tip: BlockId) -> Result<(), Self::Error> {
        let entry = self
            .journal
            .pending_entry()
            .await
            .unwrap()
            .unwrap()
            .operation
            .entry;
        assert!(matches!(
            entry.status,
            JournalStatus::Applying(ApplyStage::StoreCursor)
                | JournalStatus::RollingBack(RollbackStage::StoreCursor)
        ));
        let failure = self.fault.lock().unwrap().take();
        if failure == Some(false) {
            return Err("cursor interrupted");
        }
        self.inner
            .store_cursor(tip)
            .await
            .map_err(|_| "cursor write failed")?;
        if failure == Some(true) {
            return Err("cursor interrupted");
        }
        Ok(())
    }
}

pub(in super::super) struct Cursor {
    pub(in super::super) inner: InMemoryCursorStorage,
    pub(in super::super) journal: InMemoryBitcoinJournal,
    pub(in super::super) fault: Mutex<Option<bool>>,
}
