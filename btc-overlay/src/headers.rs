//! Where block headers come from: Blockbook in service, memory in tests.

use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;
use corepc_client::bitcoin::{BlockHash, TxMerkleNode, block, hashes::Hash};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub height: u64,
    pub header: block::Header,
}

impl Header {
    pub fn hash(&self) -> BlockHash {
        self.header.block_hash()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("header source request failed: {0}")]
    Request(String),
    #[error("header source returned bad data: {0}")]
    Malformed(String),
}

#[async_trait]
pub trait HeaderSource: Send + Sync {
    async fn tip(&self) -> Result<Header, HeaderError>;

    async fn header(&self, hash: BlockHash) -> Result<Option<Header>, HeaderError>;
}

/// A header chain kept in memory, with reorgs on demand.
#[derive(Default)]
pub struct MemoryHeaders {
    chain: Mutex<(Vec<BlockHash>, HashMap<BlockHash, Header>)>,
}

impl MemoryHeaders {
    /// A chain of `length` blocks from a made-up genesis.
    pub fn new(length: u64) -> Self {
        let headers = Self::default();
        for _ in 0..length {
            headers.mine(0);
        }
        headers
    }

    /// Adds a block on top of the block `depth` below the tip, dropping the blocks above it.
    pub fn mine(&self, depth: usize) -> Header {
        let mut chain = self.chain.lock().unwrap();
        let (hashes, headers) = &mut *chain;
        hashes.truncate(hashes.len() - depth.min(hashes.len()));
        let parent = hashes.last().map(|hash| headers[hash]);
        let header = Header {
            height: parent.map_or(0, |parent| parent.height + 1),
            header: block::Header {
                version: block::Version::TWO,
                prev_blockhash: parent.map_or(BlockHash::all_zeros(), |p| p.hash()),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 1_700_000_000 + hashes.len() as u32 * 600,
                bits: corepc_client::bitcoin::CompactTarget::from_consensus(0x207f_ffff),
                // Branches at the same height differ by nonce.
                nonce: headers.len() as u32,
            },
        };
        hashes.push(header.hash());
        headers.insert(header.hash(), header);
        header
    }
}

#[async_trait]
impl HeaderSource for MemoryHeaders {
    async fn tip(&self) -> Result<Header, HeaderError> {
        let chain = self.chain.lock().unwrap();
        let hash = chain
            .0
            .last()
            .ok_or_else(|| HeaderError::Request("empty chain".into()))?;
        Ok(chain.1[hash])
    }

    async fn header(&self, hash: BlockHash) -> Result<Option<Header>, HeaderError> {
        Ok(self.chain.lock().unwrap().1.get(&hash).copied())
    }
}
