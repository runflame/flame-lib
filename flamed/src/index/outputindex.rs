//! Contracts by id, by spend, and by predicate.

use std::collections::BTreeMap;

use flamevm::{ContractID, TxID};

/// One contract, as the node archived it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputRecord {
    /// The height that created it.
    pub height: u64,
    /// The transaction that created it.
    pub txid: TxID,
    /// The compressed point of the predicate that locks it.
    pub predicate: [u8; 32],
    /// The contract itself, as a `CellEnvelope`.
    pub contract: Vec<u8>,
    /// The entry right after the contract's `Output` in its log, if that
    /// entry is `Data`, byte for byte: the output's note, which this node
    /// never parses.
    pub note: Option<Vec<u8>>,
}

/// What became of a contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpendRecord {
    /// The height that spent it.
    pub height: u64,
    /// The transaction that spent it.
    pub txid: TxID,
}

/// Every contract the node has ever seen, spent or not.
#[derive(Debug, Default)]
pub struct OutputIndex {
    outputs: BTreeMap<ContractID, OutputRecord>,
    spent: BTreeMap<ContractID, SpendRecord>,
    by_predicate: BTreeMap<[u8; 32], Vec<ContractID>>,
}

impl OutputIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a created contract.
    ///
    /// The by-predicate list grows only on a first sighting: a contract id
    /// commits its predicate, so a repeat is the same contract twice.
    pub fn insert(&mut self, id: ContractID, record: OutputRecord) {
        let predicate = record.predicate;
        if self.outputs.insert(id, record).is_none() {
            self.by_predicate.entry(predicate).or_default().push(id);
        }
    }

    /// Records a spend.
    pub fn spend(&mut self, id: ContractID, record: SpendRecord) {
        self.spent.insert(id, record);
    }

    /// One contract.
    pub fn get(&self, id: &ContractID) -> Option<&OutputRecord> {
        self.outputs.get(id)
    }

    /// What spent one contract, if anything has.
    pub fn spend_of(&self, id: &ContractID) -> Option<&SpendRecord> {
        self.spent.get(id)
    }

    /// Every contract created under one predicate, in creation order.
    pub fn under(&self, predicate: &[u8; 32]) -> &[ContractID] {
        self.by_predicate
            .get(predicate)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// How many contracts have ever existed.
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    /// Whether any have.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}
