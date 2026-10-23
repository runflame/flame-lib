//! The devnet block producers.
//!
//! Flame's block producers are minters. There is no consensus here and no
//! competition for the next block: one node, one timer, one iblock per tick.
//! A second, slower timer stands in for Bitcoin and mines the mock core
//! chain the iblocks anchor to.

use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::indexer::MockIndexer;
use crate::node::{NodeError, SharedNode};

/// Mints one block per interval until told to stop.
pub struct Minter {
    node: SharedNode,
    interval: Duration,
}

impl Minter {
    /// A minter over one shared node.
    pub fn new(node: SharedNode, interval: Duration) -> Self {
        Self { node, interval }
    }

    /// Runs until `shutdown` is cancelled, or until a mint fails in a way
    /// the node cannot carry on from.
    ///
    /// Each tick mints on the blocking pool, because a connect re-runs every
    /// transaction's proof and can take hundreds of milliseconds — that is
    /// not work for a runtime thread.
    ///
    /// Cancellation is only ever seen between ticks. A mint already under
    /// way is awaited to the end, because a block archive cut off between
    /// the append and the connect is a node that refuses to start. A fatal
    /// error cancels `shutdown` itself before it is returned: a node that
    /// answers queries about a chain it has quietly stopped extending is
    /// worse than one that is gone.
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), Fatal> {
        let halted = self.mint_until(&shutdown).await;
        if halted.is_err() {
            shutdown.cancel();
        }
        halted
    }

    async fn mint_until(&self, shutdown: &CancellationToken) -> Result<(), Fatal> {
        let mut ticker = tokio::time::interval(self.interval);
        // A mint that overruns its interval must not be followed by a
        // burst of instant blocks: the default behaviour fires every
        // missed tick back to back, and connecting a full block runs every
        // transaction three times.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick of a tokio interval fires immediately; skip it, so
        // a node that has just started is not already a block ahead.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = ticker.tick() => {}
            }
            let node = self.node.clone();
            let minted = tokio::task::spawn_blocking(move || {
                // A poisoned lock means some earlier operation panicked
                // part-way through a mutation, so the indexes may no longer
                // describe the chain. Minting on top of that would write
                // the disagreement to disk.
                let mut node = node.lock().map_err(|_| Fatal::Poisoned)?;
                node.mint_block().map_err(Fatal::from).map(|_| ())
            })
            .await
            .map_err(|error| Fatal::Panicked(error.to_string()))?;

            match minted {
                Ok(()) => {}
                Err(Fatal::Recoverable(error)) => eprintln!("mint failed: {error}"),
                // Every remaining case means this process can no longer be
                // trusted to extend its own archive.
                Err(fatal) => return Err(fatal),
            }
        }
    }
}

/// Mines one mock core block per interval until told to stop.
pub struct CoreMiner {
    indexer: MockIndexer,
    interval: Duration,
}

impl CoreMiner {
    /// A miner extending the chain `indexer` shares with the node.
    pub fn new(indexer: MockIndexer, interval: Duration) -> Self {
        Self { indexer, interval }
    }

    /// Runs until `shutdown` is cancelled. A core block that could not be
    /// mined is only logged: the minter keeps anchoring to the old tip.
    pub async fn run(self, shutdown: CancellationToken) {
        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick fires at once: a node that starts mines a core
        // block, as Bitcoin will have moved on while it was down.
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => {}
            }
            // Mining syncs a file, which is not work for a runtime thread.
            let indexer = self.indexer.clone();
            match tokio::task::spawn_blocking(move || indexer.mine()).await {
                Ok(Ok(block)) => println!(
                    "core block {} {}",
                    block.height,
                    hex::encode(block.hash)
                ),
                Ok(Err(error)) => eprintln!("core mining failed: {error}"),
                Err(error) => eprintln!("the core miner panicked: {error}"),
            }
        }
    }
}

/// Why the minter stopped for good.
///
/// `Recoverable` is what a single tick can come back with, and the minter
/// only logs it; `run` returns every other variant.
#[derive(Debug, thiserror::Error)]
pub enum Fatal {
    /// An earlier operation panicked while holding the node.
    #[error("the node state is poisoned: an earlier operation panicked")]
    Poisoned,
    /// The mint task itself panicked.
    #[error("the mint task panicked: {0}")]
    Panicked(String),
    /// Disk and memory may no longer agree.
    #[error(transparent)]
    Archive(NodeError),
    /// A block that simply could not be built. The next tick tries again.
    #[error(transparent)]
    Recoverable(NodeError),
}

impl From<NodeError> for Fatal {
    fn from(error: NodeError) -> Self {
        match error {
            archive @ NodeError::Archive { .. } => Fatal::Archive(archive),
            other => Fatal::Recoverable(other),
        }
    }
}
