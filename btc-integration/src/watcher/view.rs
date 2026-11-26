use thiserror::Error;

use crate::watcher::types::{BlockId, BtcEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BtcView {
    genesis_id: BlockId,
    best: BlockId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("the event does not start at the best block of the view")]
pub struct OutOfSync;

impl BtcView {
    pub fn new(genesis_id: BlockId) -> Self {
        BtcView {
            genesis_id,
            best: genesis_id,
        }
    }

    pub fn restore(genesis_id: BlockId, best: BlockId) -> Self {
        assert!(
            best.height > genesis_id.height || best == genesis_id,
            "the best block {best} is not above the genesis block {genesis_id}"
        );
        BtcView { genesis_id, best }
    }

    pub fn genesis_id(&self) -> BlockId {
        self.genesis_id
    }

    pub fn best(&self) -> BlockId {
        self.best
    }

    pub fn height(&self) -> u64 {
        self.best.height
    }

    /// The view checks heights only. It holds no blocks, so it cannot check
    /// that the new blocks link to each other: the watcher has done that.
    pub fn apply(&mut self, event: &BtcEvent) -> Result<(), OutOfSync> {
        let fork = match event {
            BtcEvent::NewBlocks { .. } => self.best,
            BtcEvent::Reorg { fork, removed, .. } => {
                // The removed blocks are the top of our chain: the first one
                // is the best block, and `fork` stands below it by their number.
                let from_best = removed.first() == Some(&self.best);
                let reaches_fork =
                    fork.height as usize + removed.len() == self.best.height as usize;
                let keeps_genesis =
                    fork.height > self.genesis_id.height || *fork == self.genesis_id;
                if !(from_best && reaches_fork && keeps_genesis) {
                    return Err(OutOfSync);
                }
                *fork
            }
        };
        let mut best = fork;
        for block in event.blocks() {
            if block.height != best.height + 1 {
                return Err(OutOfSync);
            }
            best = *block;
        }
        self.best = best;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::BlockHash;
    use bitcoin::hashes::Hash;

    use super::*;

    fn block(height: u64, tag: u8) -> BlockId {
        BlockId {
            height,
            hash: BlockHash::from_byte_array([tag; 32]),
        }
    }

    /// Blocks 100 ← 101 ← 102 ← 103 with tags 10, 11, 12, 13.
    fn view() -> BtcView {
        let mut view = BtcView::new(block(100, 10));
        let event = BtcEvent::NewBlocks {
            blocks: vec![block(101, 11), block(102, 12), block(103, 13)],
        };
        assert_eq!(view.apply(&event), Ok(()));
        view
    }

    #[test]
    fn a_reorg_moves_the_best_block() {
        // To the end of the new branch.
        let mut view = view();
        let event = BtcEvent::Reorg {
            fork: block(101, 11),
            removed: vec![block(103, 13), block(102, 12)],
            blocks: vec![block(102, 22)],
        };
        assert_eq!(view.apply(&event), Ok(()));
        assert_eq!(view.best(), block(102, 22));

        // A rollback is a reorg without new blocks: to the fork.
        let event = BtcEvent::Reorg {
            fork: block(101, 11),
            removed: vec![block(102, 22)],
            blocks: vec![],
        };
        assert_eq!(view.apply(&event), Ok(()));
        assert_eq!(view.best(), block(101, 11));
    }

    #[test]
    fn the_genesis_block_cannot_be_removed() {
        let mut view = view();
        let before = view;
        // The event undoes everything down to a block at height 100, but that
        // is another block.
        let event = BtcEvent::Reorg {
            fork: block(100, 77),
            removed: vec![block(103, 13), block(102, 12), block(101, 11)],
            blocks: vec![block(101, 78)],
        };
        assert_eq!(view.apply(&event), Err(OutOfSync));
        assert_eq!(view, before);
    }

    #[test]
    fn an_event_that_does_not_fit_the_view_is_rejected() {
        let mut view = view();
        let before = view;
        // The event removes a block that is not our best block.
        let event = BtcEvent::Reorg {
            fork: block(102, 12),
            removed: vec![block(103, 99)],
            blocks: vec![],
        };
        assert_eq!(view.apply(&event), Err(OutOfSync));
        // A block is missing between the removed one and the fork.
        let event = BtcEvent::Reorg {
            fork: block(101, 11),
            removed: vec![block(103, 13)],
            blocks: vec![],
        };
        assert_eq!(view.apply(&event), Err(OutOfSync));
        // The new blocks do not start right above our best block.
        let event = BtcEvent::NewBlocks {
            blocks: vec![block(105, 15)],
        };
        assert_eq!(view.apply(&event), Err(OutOfSync));
        // The new blocks skip a height.
        let event = BtcEvent::NewBlocks {
            blocks: vec![block(104, 14), block(106, 16)],
        };
        assert_eq!(view.apply(&event), Err(OutOfSync));
        assert_eq!(view, before);
    }
}
