use std::{collections::HashMap, sync::Mutex};

use super::types::IndexedBlock;
use crate::btc::rpc::BtcBlockTip;
use corepc_client::bitcoin::BlockHash;

const CAPACITY: usize = 10;

#[derive(Default)]
struct CacheState {
    window: Vec<BtcBlockTip>,
    blocks: HashMap<BlockHash, IndexedBlock>,
}

/// Stores only blocks in the latest observed chain window. It performs no I/O.
#[derive(Default)]
pub(super) struct RecentBlockCache {
    state: Mutex<CacheState>,
}

impl RecentBlockCache {
    pub const CAPACITY: usize = CAPACITY;

    pub fn get(&self, hash: BlockHash) -> Option<IndexedBlock> {
        self.state.lock().unwrap().blocks.get(&hash).cloned()
    }

    pub fn insert_if_current(&self, block: IndexedBlock) {
        let mut state = self.state.lock().unwrap();
        if state.window.contains(&block.btc_block_tip) {
            state.blocks.insert(block.btc_block_tip.hash, block);
        }
    }

    /// `window` is ordered from the observed tip towards its ancestors.
    pub fn replace_window(&self, mut window: Vec<BtcBlockTip>) {
        window.truncate(CAPACITY);
        let mut state = self.state.lock().unwrap();
        state
            .blocks
            .retain(|_, block| window.contains(&block.btc_block_tip));
        state.window = window;
    }

    pub fn clear(&self) {
        *self.state.lock().unwrap() = CacheState::default();
    }
}
