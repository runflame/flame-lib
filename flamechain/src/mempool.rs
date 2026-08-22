//! Small bounded, FIFO mempool. Policy is local; block validity is not.

use std::collections::BTreeSet;

use flamevm::{CellID, TxEntry, TxID, TxLog, VMError};

use crate::BlockHash;
use crate::block::{BlockTx, Blockchain, CellLeaf};
use crate::utreexo::{self, Catchup, Forest, Proof, UtreexoError, WorkForest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MempoolPolicy {
    pub max_transactions: usize,
    pub max_witness_bytes: usize,
    pub max_transaction_gas: u64,
    pub max_proofs_per_transaction: usize,
    pub max_proof_depth: usize,
    pub minimum_fee: u64,
}

impl Default for MempoolPolicy {
    fn default() -> Self {
        Self {
            max_transactions: 10_000,
            max_witness_bytes: 64 * 1024 * 1024,
            max_transaction_gas: 10_000_000,
            max_proofs_per_transaction: 100_000,
            max_proof_depth: 63,
            minimum_fee: 0,
        }
    }
}

pub struct MempoolEntry {
    block_tx: BlockTx,
    txid: TxID,
    fee: u64,
    witness_bytes: usize,
}

impl MempoolEntry {
    pub fn transaction(&self) -> &BlockTx {
        &self.block_tx
    }

    pub fn txid(&self) -> TxID {
        self.txid
    }

    pub fn fee(&self) -> u64 {
        self.fee
    }

    pub fn witness_bytes(&self) -> usize {
        self.witness_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RebaseReport {
    pub kept: usize,
    pub dropped: usize,
}

pub struct Mempool {
    policy: MempoolPolicy,
    version: u32,
    base_tip: BlockHash,
    base_cells: Forest,
    work: WorkForest,
    entries: Vec<MempoolEntry>,
    witness_ids: BTreeSet<[u8; 32]>,
    txids: BTreeSet<[u8; 32]>,
    created_cells: BTreeSet<CellID>,
    witness_bytes: usize,
}

impl Mempool {
    pub fn new(chain: &Blockchain, policy: MempoolPolicy) -> Self {
        let base_cells = chain.cell_forest().clone();
        let work = base_cells.work_forest();
        Self {
            policy,
            version: chain.version(),
            base_tip: chain.tip(),
            base_cells,
            work,
            entries: Vec::new(),
            witness_ids: BTreeSet::new(),
            txids: BTreeSet::new(),
            created_cells: BTreeSet::new(),
            witness_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &MempoolEntry> {
        self.entries.iter()
    }

    /// Verifies and appends one candidate. Entries remain FIFO because sorting
    /// without package semantics could put a child before its transient parent.
    pub fn admit(&mut self, block_tx: BlockTx) -> Result<&MempoolEntry, MempoolError> {
        self.admit_inner(block_tx, None)
    }

    fn admit_inner(
        &mut self,
        mut block_tx: BlockTx,
        catchup: Option<&Catchup>,
    ) -> Result<&MempoolEntry, MempoolError> {
        if self.entries.len() >= self.policy.max_transactions {
            return Err(MempoolError::Full);
        }
        let witness_bytes = self.check_envelope(&block_tx)?;
        let witness_id = block_tx.witness_hash().0;
        if self.witness_ids.contains(&witness_id) {
            return Err(MempoolError::Duplicate);
        }

        let log = block_tx.tx.verify(block_tx.limits)?;
        let txid = log.txid();
        if self.txids.contains(&(txid.0).0) {
            return Err(MempoolError::Duplicate);
        }
        let fee = log
            .iter()
            .try_fold(0u64, |sum, entry| match entry {
                TxEntry::Fee(value) => sum.checked_add(*value),
                _ => Some(sum),
            })
            .ok_or(MempoolError::InvalidEnvelope)?;
        if fee < self.policy.minimum_fee {
            return Err(MempoolError::FeeTooLow);
        }

        if let Some(catchup) = catchup {
            let inputs: Vec<_> = log
                .iter()
                .filter_map(|entry| match entry {
                    TxEntry::Input(id) => Some(*id),
                    _ => None,
                })
                .collect();
            if inputs.len() != block_tx.proofs.len() {
                return Err(MempoolError::ProofCount);
            }
            let hasher = utreexo::utreexo_hasher::<CellLeaf>();
            for (id, proof) in inputs.into_iter().zip(&mut block_tx.proofs) {
                *proof = catchup.update_proof(&CellLeaf(id), proof.clone(), &hasher)?;
            }
        }

        Self::apply_log(
            &mut self.work,
            &log,
            &block_tx.proofs,
            &mut self.created_cells,
        )?;
        self.witness_bytes = self
            .witness_bytes
            .checked_add(witness_bytes)
            .ok_or(MempoolError::Full)?;
        self.witness_ids.insert(witness_id);
        self.txids.insert((txid.0).0);
        self.entries.push(MempoolEntry {
            block_tx,
            txid,
            fee,
            witness_bytes,
        });
        Ok(self.entries.last().unwrap())
    }

    fn check_envelope(&self, block_tx: &BlockTx) -> Result<usize, MempoolError> {
        if block_tx.tx.header().version != self.version
            || block_tx.limits.gas > self.policy.max_transaction_gas
            || block_tx.proofs.len() > self.policy.max_proofs_per_transaction
            || block_tx.proofs.iter().any(|proof| {
                proof
                    .as_path()
                    .is_some_and(|path| path.neighbors.len() > self.policy.max_proof_depth)
            })
        {
            return Err(MempoolError::InvalidEnvelope);
        }
        let bytes = block_tx
            .witness_size()
            .ok_or(MempoolError::InvalidEnvelope)?;
        if bytes
            > self
                .policy
                .max_witness_bytes
                .saturating_sub(self.witness_bytes)
        {
            return Err(MempoolError::Full);
        }
        Ok(bytes)
    }

    fn apply_log(
        work: &mut WorkForest,
        log: &TxLog,
        proofs: &[Proof],
        created_cells: &mut BTreeSet<CellID>,
    ) -> Result<(), MempoolError> {
        let expected = log
            .iter()
            .filter(|entry| matches!(entry, TxEntry::Input(_)))
            .count();
        if expected != proofs.len() {
            return Err(MempoolError::ProofCount);
        }
        let hasher = utreexo::utreexo_hasher::<CellLeaf>();
        let mut proofs = proofs.iter();
        let mut new_cells = BTreeSet::new();
        work.batch(|work| {
            for entry in log.iter() {
                match entry {
                    TxEntry::Input(id) => {
                        work.delete(&CellLeaf(*id), proofs.next().unwrap(), &hasher)?;
                    }
                    TxEntry::Output(cell) => {
                        let id = cell.id();
                        if created_cells.contains(&id) || !new_cells.insert(id) {
                            return Err(MempoolError::DuplicateCell);
                        }
                        work.insert(&CellLeaf(id), &hasher);
                    }
                    _ => {}
                }
            }
            Ok::<_, MempoolError>(())
        })?;
        created_cells.extend(new_cells);
        Ok(())
    }

    /// Replays bounded entries on a new tip. A one-transition Catchup updates
    /// surviving proofs; invalid or conflicting entries are simply dropped.
    pub fn rebase(&mut self, chain: &Blockchain, catchup: Option<&Catchup>) -> RebaseReport {
        if self.base_tip == chain.tip() {
            return RebaseReport {
                kept: self.entries.len(),
                dropped: 0,
            };
        }
        let old = std::mem::take(&mut self.entries);
        self.version = chain.version();
        self.base_tip = chain.tip();
        self.base_cells = chain.cell_forest().clone();
        self.reset_work();

        let total = old.len();
        for entry in old {
            let _ = self.admit_inner(entry.block_tx, catchup);
        }
        RebaseReport {
            kept: self.entries.len(),
            dropped: total - self.entries.len(),
        }
    }

    /// Moves all entries out in their dependency-safe FIFO order.
    pub fn take_all(&mut self) -> Vec<BlockTx> {
        let entries = std::mem::take(&mut self.entries);
        self.reset_work();
        entries.into_iter().map(|entry| entry.block_tx).collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.reset_work();
    }

    fn reset_work(&mut self) {
        self.work = self.base_cells.work_forest();
        self.witness_ids.clear();
        self.txids.clear();
        self.created_cells.clear();
        self.witness_bytes = 0;
    }
}

#[derive(thiserror::Error, Debug)]
pub enum MempoolError {
    #[error("mempool policy limit reached")]
    Full,
    #[error("duplicate transaction")]
    Duplicate,
    #[error("transaction fee is below local policy")]
    FeeTooLow,
    #[error("invalid transaction envelope")]
    InvalidEnvelope,
    #[error("missing or trailing Utreexo proof")]
    ProofCount,
    #[error("duplicate cell id")]
    DuplicateCell,
    #[error(transparent)]
    Vm(#[from] VMError),
    #[error(transparent)]
    Utreexo(#[from] UtreexoError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use flamevm::{Anchor, Cell, Predicate, TxHeader};

    #[test]
    fn transient_dependency_is_consumed_once() {
        let base = Forest::new();
        let mut work = base.work_forest();
        let mut created = BTreeSet::new();
        let cell = Cell::new(
            Predicate::opaque(Predicate::unspendable_key()),
            Anchor([7; 32]),
            vec![],
        )
        .expect("empty payload is portable");
        let id = cell.id();
        let output = TxLog::from(vec![
            TxEntry::Header(TxHeader {
                version: 1,
                locktime: 0,
            }),
            TxEntry::Output(cell),
        ]);
        Mempool::apply_log(&mut work, &output, &[], &mut created).unwrap();

        let input = TxLog::from(vec![TxEntry::Input(id)]);
        Mempool::apply_log(&mut work, &input, &[Proof::Transient], &mut created).unwrap();
        assert!(matches!(
            Mempool::apply_log(&mut work, &input, &[Proof::Transient], &mut created),
            Err(MempoolError::Utreexo(UtreexoError::InvalidProof))
        ));
    }

    #[test]
    fn empty_rebase_tracks_new_tip() {
        let mut chain = Blockchain::new(Default::default()).unwrap();
        let mut pool = Mempool::new(&chain, Default::default());
        let block = chain.build_block([1; 32], vec![]).unwrap();
        let applied = chain.connect(&block).unwrap();
        assert_eq!(
            pool.rebase(&chain, Some(&applied.catchup)),
            RebaseReport {
                kept: 0,
                dropped: 0
            }
        );
    }
}
