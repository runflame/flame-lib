//! Follows the header source and keeps the overlay chain on its best branch.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use corepc_client::bitcoin::BlockHash;
use tokio::sync::watch;

use crate::{
    headers::{HeaderError, HeaderSource},
    lag::{Lag, Timeline, View},
    state::{State, StateError},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tip {
    pub hash: BlockHash,
    pub height: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Headers(#[from] HeaderError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("block {0} is missing from the header source")]
    MissingParent(BlockHash),
    #[error("start height {start} is above the header source tip {tip}")]
    StartAboveTip { start: u64, tip: u64 },
    #[error("a reorg replaced the start block at {0}; restart with a fresh log")]
    StartReplaced(u64),
}

/// Blocks served below the tip on a fresh log. The indexer reads up to 10 blocks back.
pub const DEFAULT_HISTORY: u64 = 20;

pub struct Overlay {
    headers: Arc<dyn HeaderSource>,
    state: Mutex<State>,
    start_height: Option<u64>,
    tip: watch::Sender<Option<Tip>>,
    syncing: tokio::sync::Mutex<()>,
    timeline: Mutex<Timeline>,
    lags: Mutex<HashMap<String, Lag>>,
}

impl Overlay {
    /// `start_height` is the first block served on a fresh log, by default [`DEFAULT_HISTORY`]
    /// below the tip; it is ignored once started.
    pub fn new(headers: Arc<dyn HeaderSource>, state: State, start_height: Option<u64>) -> Self {
        let tip = state.tip().map(|block| Tip {
            hash: block.hash(),
            height: block.header.height,
        });
        Self {
            headers,
            state: Mutex::new(state),
            start_height,
            tip: watch::Sender::new(tip),
            syncing: tokio::sync::Mutex::new(()),
            timeline: Mutex::new(Timeline::new(tip)),
            lags: Mutex::new(HashMap::new()),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Notified on every tip change, whether or not a lagging client sees it yet.
    pub fn subscribe(&self) -> watch::Receiver<Option<Tip>> {
        self.tip.subscribe()
    }

    pub fn lag(&self, client: &str) -> Option<Lag> {
        self.lags.lock().unwrap().get(client).copied()
    }

    /// Sets the lag of `client`, or removes it with `None`.
    pub fn set_lag(&self, client: &str, lag: Option<Lag>) {
        let mut lags = self.lags.lock().unwrap();
        match lag {
            Some(lag) => lags.insert(client.into(), lag),
            None => lags.remove(client),
        };
    }

    /// The chain as `client` sees it now; without a name or a lag, the live chain.
    pub fn view(&self, client: Option<&str>) -> View {
        let client = client.unwrap_or_default();
        let lag = self.lag(client);
        self.timeline
            .lock()
            .unwrap()
            .view(client, lag.as_ref(), Instant::now())
    }

    /// Moves to the header source's tip. Returns whether the tip changed.
    pub async fn sync(&self) -> Result<bool, SyncError> {
        let _syncing = self.syncing.lock().await;
        let remote = self.headers.tip().await?;
        let (started, tip) = {
            let state = self.state();
            (state.start_height(), state.tip().map(|block| block.header))
        };
        if tip == Some(remote) {
            return Ok(false);
        }
        // Blockbook can briefly step back during a reorg; Core never moves to a shorter chain.
        if tip.is_some_and(|tip| remote.height < tip.height) {
            return Ok(false);
        }
        let start = started
            .or(self.start_height)
            .unwrap_or(remote.height.saturating_sub(DEFAULT_HISTORY));
        if remote.height < start {
            return Err(SyncError::StartAboveTip {
                start,
                tip: remote.height,
            });
        }

        // Walk back to the canonical chain, or to the start block on a fresh log.
        let mut branch = vec![remote];
        loop {
            let current = *branch.last().unwrap();
            if current.height == start {
                // A canonical start block would have ended the walk one step earlier.
                if started.is_some() {
                    return Err(SyncError::StartReplaced(start));
                }
                break;
            }
            let parent = self
                .headers
                .header(current.header.prev_blockhash)
                .await?
                .filter(|parent| parent.height + 1 == current.height)
                .ok_or(SyncError::MissingParent(current.header.prev_blockhash))?;
            if self.state().is_canonical(&parent) {
                break;
            }
            branch.push(parent);
        }
        let mut state = self.state();
        if started.is_none() {
            state.start(start)?;
        }
        let lowest = branch.last().map_or(remote.height, |header| header.height);
        while state.tip().is_some_and(|tip| tip.header.height >= lowest) {
            state.disconnect_tip()?;
        }
        let new_blocks = branch
            .iter()
            .map(|header| header.hash())
            .filter(|hash| state.block(hash).is_none())
            .collect();
        for header in branch.iter().rev() {
            state.connect(*header)?;
        }
        let tip = Tip {
            hash: remote.hash(),
            height: remote.height,
        };
        self.timeline
            .lock()
            .unwrap()
            .record(Instant::now(), tip, new_blocks);
        self.tip.send_replace(Some(tip));
        Ok(true)
    }
}
