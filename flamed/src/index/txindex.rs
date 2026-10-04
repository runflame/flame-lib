//! Transactions by id.

use std::collections::BTreeMap;

use flamechain::BlockHash;
use flamed_rpc::{BlockSummary, TransactionSummary, TransactionsResult};
use flamevm::TxID;

/// Where a transaction landed, and what it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxRecord {
    /// The height of the block that carried it.
    pub height: u64,
    /// That block's hash.
    pub block: BlockHash,
    /// Its effect log, as a `CellEnvelope`.
    pub log: Vec<u8>,
    /// Public execution summary.
    pub summary: TransactionSummary,
}

/// Confirmed transactions, by id.
#[derive(Debug, Default)]
pub struct TxIndex {
    // Keyed on the raw bytes: `TxID` is `Eq` but not `Ord`, the same reason
    // upstream's own mempool keeps a `BTreeSet<[u8; 32]>`.
    txs: BTreeMap<[u8; 32], TxRecord>,
    by_height: BTreeMap<u64, Vec<TxID>>,
    blocks: BTreeMap<u64, BlockSummary>,
}

impl TxIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one confirmed transaction.
    pub fn insert(&mut self, txid: TxID, record: TxRecord) {
        self.by_height.entry(record.height).or_default().push(txid);
        self.txs.insert(txid.0, record);
    }

    /// What the node knows about one transaction.
    pub fn get(&self, txid: &TxID) -> Option<&TxRecord> {
        self.txs.get(&txid.0)
    }

    /// Every transaction confirmed at one height, in block order.
    pub fn at_height(&self, height: u64) -> &[TxID] {
        self.by_height
            .get(&height)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// How many transactions have confirmed.
    pub fn len(&self) -> usize {
        self.txs.len()
    }

    /// Whether any have.
    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    /// Adds a header after the block has been accepted.
    pub fn insert_block(&mut self, block: BlockSummary) {
        self.blocks.insert(block.header.height, block);
    }

    /// Header at an exact height.
    pub fn block(&self, height: u64) -> Option<&BlockSummary> {
        self.blocks.get(&height)
    }

    /// A page of executions, newest first.
    /// Returns `None` if the cursor does not name a confirmed execution.
    pub fn transactions(&self, before: Option<TxID>, limit: usize) -> Option<TransactionsResult> {
        let height = match before {
            Some(id) => self.get(&id)?.height,
            None => u64::MAX,
        };

        let mut ids = self
            .by_height
            .range(..=height)
            .rev()
            .flat_map(|(_, ids)| ids.iter().rev())
            .skip_while(|id| before.is_some_and(|cursor| **id != cursor));

        if before.is_some() {
            ids.next();
        }

        let transactions = ids
            .by_ref()
            .take(limit)
            .map(|id| self.txs[&id.0].summary.clone())
            .collect();

        Some(TransactionsResult {
            transactions,
            has_more: ids.next().is_some(),
        })
    }
}
