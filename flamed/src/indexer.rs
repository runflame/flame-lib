//! The core chain, as this node sees it: through an indexer.
//!
//! Every Flame header commits a `core_block_hash`: the core chain's tip when
//! the block was minted. Flame blocks are iblocks and come faster than core
//! blocks, so consecutive headers often name the same one. A header carries
//! its own time, so the node asks the indexer only for the tip.
//!
//! `MockIndexer` is the only implementation. There is no Bitcoin behind it:
//! its chain grows only when something calls [`MockIndexer::mine`], which on
//! a devnet is a timer of its own. It keeps its table in its own directory,
//! apart from `blocks.bin`, so a connector to a real Bitcoin node can
//! replace it without the node changing.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// One core block, as an indexer reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreBlock {
    /// Its height in the core chain; the first block the mock mines is 1.
    pub height: u64,
    /// The hash a Flame header commits as `core_block_hash`.
    pub hash: [u8; 32],
    /// Its time, in Unix seconds.
    pub time: u64,
}

/// What the node needs from a core-chain indexer.
pub trait CoreIndexer: Send {
    /// The core chain's tip, which the next Flame block anchors to.
    fn tip(&mut self) -> Result<CoreBlock, CoreError>;
}

/// This devnet's stand-in for the hash of core block `height`.
pub fn core_block_hash(height: u64) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"flame.devnet.core");
    digest.update(height.to_le_bytes());
    digest.finalize().into()
}

/// This machine's clock, in Unix seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// A core chain with no Bitcoin behind it.
///
/// Its table lives in `core-times.txt`, one line per core block from height
/// 1: the hash in hex, a space, the Unix time, a newline. Each line is
/// written and synced before `mine` returns it, so a block the node has
/// anchored to is one this indexer still has after a restart.
///
/// Clones share one chain: the node holds one to read the tip, and the
/// devnet's core miner another to extend it.
#[derive(Clone, Debug)]
pub struct MockIndexer {
    table: Arc<Mutex<Table>>,
}

#[derive(Debug)]
struct Table {
    file: File,
    blocks: Vec<CoreBlock>,
}

impl MockIndexer {
    /// The table's file name inside the indexer's directory.
    pub const FILE: &'static str = "core-times.txt";

    /// Opens the table in `dir`, creating both if this is a fresh start.
    ///
    /// A trailing line with no newline means the process died mid-append.
    /// No block was anchored to it, because `mine` had not returned yet, so
    /// the line is dropped.
    pub fn open(dir: &Path) -> Result<Self, CoreError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(Self::FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let complete = text.rfind('\n').map_or(0, |end| end + 1);

        let mut blocks = Vec::new();
        for (index, line) in text[..complete].lines().enumerate() {
            let height = index as u64 + 1;
            let block = parse_line(line)
                .filter(|(hash, _)| *hash == core_block_hash(height))
                .map(|(hash, time)| CoreBlock { height, hash, time })
                .ok_or_else(|| CoreError::Corrupt {
                    path: path.clone(),
                    line: index + 1,
                })?;
            blocks.push(block);
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        if complete < text.len() {
            file.set_len(complete as u64)?;
        }
        Ok(Self {
            table: Arc::new(Mutex::new(Table { file, blocks })),
        })
    }

    /// How many core blocks this chain has.
    pub fn len(&self) -> usize {
        self.lock().map_or(0, |table| table.blocks.len())
    }

    /// Whether it has any.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Extends the chain by one block, timed by this machine's clock but
    /// never before its parent.
    pub fn mine(&self) -> Result<CoreBlock, CoreError> {
        let mut table = self.lock()?;
        let (height, parent_time) = table
            .blocks
            .last()
            .map_or((1, 0), |tip| (tip.height + 1, tip.time));
        let block = CoreBlock {
            height,
            hash: core_block_hash(height),
            time: unix_now().max(parent_time),
        };
        writeln!(table.file, "{} {}", hex::encode(block.hash), block.time)?;
        table.file.sync_data()?;
        table.blocks.push(block);
        Ok(block)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Table>, CoreError> {
        self.table.lock().map_err(|_| CoreError::Poisoned)
    }
}

impl CoreIndexer for MockIndexer {
    /// The last block mined. A chain with none mines its first, so a fresh
    /// devnet can mint before its core miner has ticked.
    fn tip(&mut self) -> Result<CoreBlock, CoreError> {
        let tip = self.lock()?.blocks.last().copied();
        match tip {
            Some(tip) => Ok(tip),
            None => self.mine(),
        }
    }
}

fn parse_line(line: &str) -> Option<([u8; 32], u64)> {
    let (hash, time) = line.split_once(' ')?;
    let hash = hex::decode(hash).ok()?.try_into().ok()?;
    Some((hash, time.parse().ok()?))
}

/// An indexer could not answer.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A line of the mock's table that does not parse, or names the wrong
    /// hash for its height.
    #[error("{path} line {line} is not `<hex hash of its height> <unix time>`")]
    Corrupt {
        /// The table.
        path: PathBuf,
        /// Which line, from 1.
        line: usize,
    },
    /// A thread panicked while extending the mock's chain.
    #[error("the core indexer is poisoned: an earlier operation panicked")]
    Poisoned,
    /// The filesystem.
    #[error(transparent)]
    Io(#[from] io::Error),
}
