//! `blocks.bin`: every block this node has accepted, in order.
//!
//! One record per block — a `u64` little-endian length, then the block's
//! canonical bytes — and nothing else: no magic number, no index, no
//! checksum. The block bytes are self-describing and `Block::from_bytes_bounded`
//! re-encodes what it decodes and compares, so a corrupt record cannot pass
//! for a valid one. What the length prefix buys is the ability to find the
//! next record without decoding this one.
//!
//! A torn trailing record is a refusal to start rather than a silent
//! discard. It means the process died mid-append; an operator who truncates
//! the file to the reported offset gets a node that starts.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use flamechain::{Block, ChainParams};
use flamevm::CellError;

/// The append-only block archive.
#[derive(Debug)]
pub struct BlockStore {
    file: File,
    offsets: Vec<u64>,
}

impl BlockStore {
    /// Opens the archive, creating it if this is a fresh data directory.
    pub fn open(path: &Path) -> io::Result<BlockStore> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(BlockStore {
            file,
            offsets: Vec::new(),
        })
    }

    /// Appends one block and waits for the disk to say so.
    pub fn append(&mut self, block: &Block) -> Result<(), StoreError> {
        let bytes = block.to_bytes()?;
        let offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&(bytes.len() as u64).to_le_bytes())?;
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;
        self.offsets.push(offset);
        Ok(())
    }

    /// Every archived block, in order, rebuilding the offset table.
    ///
    /// `params` comes from `genesis.json`, never from `Default`:
    /// `from_bytes_bounded` enforces the limits it is handed, and a node that
    /// replayed its own archive under someone else's limits would be
    /// deciding validity by whichever build it happens to be.
    pub fn replay(&mut self, params: ChainParams) -> Result<Vec<Block>, StoreError> {
        let length = self.file.seek(SeekFrom::End(0))?;
        self.offsets.clear();

        let mut blocks = Vec::new();
        let mut offset = 0u64;
        while offset < length {
            let mut prefix = [0u8; 8];
            self.read_exact_at(&mut prefix, offset)?;
            let size = u64::from_le_bytes(prefix);
            // A corrupt prefix is a truncated file, not an overflow panic.
            let end = offset
                .checked_add(8)
                .and_then(|start| start.checked_add(size))
                .ok_or(StoreError::Truncated { offset })?;
            if end > length {
                return Err(StoreError::Truncated { offset });
            }

            let mut bytes =
                vec![0u8; usize::try_from(size).map_err(|_| StoreError::Truncated { offset })?];
            self.read_exact_at(&mut bytes, offset + 8)?;
            blocks.push(
                Block::from_bytes_bounded(&bytes, params)
                    .map_err(|source| StoreError::Decode { offset, source })?,
            );

            self.offsets.push(offset);
            offset = end;
        }

        // Leave the cursor where the next append belongs.
        self.file.seek(SeekFrom::End(0))?;
        Ok(blocks)
    }

    /// How many blocks are archived.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether the archive is empty.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Reads from an absolute offset, never trusting the cursor.
    fn read_exact_at(&mut self, buffer: &mut [u8], offset: u64) -> Result<(), StoreError> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                StoreError::Truncated { offset }
            } else {
                StoreError::Io(error)
            }
        })
    }
}

/// The archive could not be read or extended.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A record runs past the end of the file.
    #[error("the block archive is truncated at byte {offset}")]
    Truncated {
        /// Where the incomplete record starts.
        offset: u64,
    },
    /// A record's bytes are not a block.
    #[error("the block archived at byte {offset} does not decode: {source}")]
    Decode {
        /// Where that record starts.
        offset: u64,
        /// Why.
        source: CellError,
    },
    /// A block would not encode.
    #[error(transparent)]
    Cell(#[from] CellError),
    /// The file itself.
    #[error(transparent)]
    Io(#[from] io::Error),
}
