//! The devnet block producer.
//!
//! Flame's block producers are minters. There is no consensus here and no
//! competition for the next block: one node, one timer, one block per tick.

use std::process;
use std::time::Duration;

use tokio::time::MissedTickBehavior;

use crate::node::{NodeError, SharedNode};

/// Mints one block per interval, forever.
pub struct Minter {
    node: SharedNode,
    interval: Duration,
}

impl Minter {
    /// A minter over one shared node.
    pub fn new(node: SharedNode, interval: Duration) -> Self {
        Self { node, interval }
    }

    /// Runs until the process ends.
    ///
    /// Each tick mints on the blocking pool, because a connect re-runs every
    /// transaction's proof and can take hundreds of milliseconds — that is
    /// not work for a runtime thread.
    pub async fn run(self) {
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
            ticker.tick().await;
            let node = self.node.clone();
            let minted = tokio::task::spawn_blocking(move || {
                // A poisoned lock means some earlier operation panicked
                // part-way through a mutation, so the indexes may no longer
                // describe the chain. Minting on top of that would write
                // the disagreement to disk.
                let mut node = node.lock().map_err(|_| Fatal::Poisoned)?;
                node.mint_block().map_err(Fatal::from).map(|_| ())
            })
            .await;

            match minted {
                Ok(Ok(())) => {}
                Ok(Err(Fatal::Recoverable(error))) => eprintln!("mint failed: {error}"),
                // Every remaining case means this process can no longer be
                // trusted to extend its own archive, and a node that
                // answers queries about a chain it has quietly stopped
                // extending is worse than one that is gone.
                Ok(Err(fatal)) => {
                    eprintln!("fatal: {fatal}");
                    process::exit(1);
                }
                Err(error) => {
                    eprintln!("fatal: the mint task panicked: {error}");
                    process::exit(1);
                }
            }
        }
    }
}

/// What a tick can come back with.
#[derive(Debug, thiserror::Error)]
enum Fatal {
    /// An earlier operation panicked while holding the node.
    #[error("the node state is poisoned: an earlier operation panicked")]
    Poisoned,
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
