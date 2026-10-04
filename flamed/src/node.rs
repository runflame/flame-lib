//! One node: a chain, a mempool, and the three things the chain forgets.
//!
//! A `Blockchain` keeps roots and an accumulator. It does not keep a proof
//! for anyone, it does not remember which transaction created which
//! contract, and it never knew which predicate locked it. A wallet needs all
//! three, so this node keeps them: `UtxoSet` for the proofs, `TxIndex` and
//! `OutputIndex` for the history. All three are rebuilt by replaying
//! `blocks.bin`, which is the only durable state there is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::{Arc, Mutex};

use flamechain::utreexo::{Proof, UtreexoError};
use flamechain::{
    utreexo_hasher, Block, BlockHash, BlockTx, Blockchain, ChainError, ChainParams, ContractLeaf,
    ExecutionKind, Mempool, MempoolError, MempoolPolicy, RebaseReport,
};
use flamed_rpc::{
    ActorId, BlockHeader, BlockId, BlockResult, BlockSummary, ContractEnvelope, ContractId,
    ExecutionData, NoteEnvelope, PredicatePoint, ScanEntry, SpentAt, StateCommitment,
    TransactionResult, TransactionSummary, TransactionsResult, TxId, MAX_PAGE_SIZE,
};
use flamevm::{CellError, ContractID, TxEntry, TxID, VMError};
use sha2::{Digest, Sha256};

use crate::cells::{contract_bytes, log_bytes};
use crate::config::{GenesisFile, NodeConfig};
use crate::genesis::{self, GenesisError};
use crate::index::{OutputIndex, OutputRecord, SpendRecord, TxIndex, TxRecord};
use crate::inspect;
use crate::store::{BlockStore, StoreError};
use crate::utxos::UtxoSet;

/// The txid the indexes record for a genesis allocation, which no
/// transaction created.
pub const GENESIS_TXID: TxID = TxID([0; 32]);

/// One node, shared by the RPC handlers and the minter.
pub type SharedNode = Arc<Mutex<Node>>;

/// This devnet's stand-in for a core-chain identity.
///
/// `check_header` does not constrain `core_block_hash` and the header
/// carries no timestamp, so a devnet needs some rule; height is the only
/// thing a second node could independently agree on.
pub fn core_block_hash(height: u64) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"flame.devnet.core");
    digest.update(height.to_le_bytes());
    digest.finalize().into()
}

/// An archival node.
pub struct Node {
    params: ChainParams,
    chain: Blockchain,
    mempool: Mempool,
    utxos: UtxoSet,
    txindex: TxIndex,
    outputs: OutputIndex,
    store: BlockStore,
}

// Every call into the node runs on the blocking pool behind a
// `std::sync::Mutex`, which needs this. It holds today — nothing upstream
// uses `Rc` — and this is the tripwire if that changes.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Node>();
};

/// The chain's current tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipInfo {
    /// The tip block's hash.
    pub hash: BlockHash,
    /// Its height.
    pub height: u64,
    /// The contract accumulator root at that tip.
    pub contract_root: [u8; 32],
}

/// What the node knows about one contract.
#[derive(Clone, Debug)]
pub enum ProofStatus {
    /// Unspent, with a proof valid at the tip.
    Unspent(Proof),
    /// Spent, here.
    Spent {
        /// The height that spent it.
        height: u64,
        /// The transaction that spent it.
        txid: TxID,
    },
    /// Never seen.
    Unknown,
}

/// Where a transaction is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxStatus {
    /// Neither in the mempool nor in the chain.
    Unknown,
    /// Waiting in this node's mempool.
    Mempool,
    /// In a block.
    Confirmed {
        /// The height of that block.
        height: u64,
        /// Its hash.
        block: BlockHash,
    },
}

impl Node {
    /// Opens a node: rebuilds the genesis, replays the archive, and starts
    /// with an empty mempool.
    pub fn open(genesis: &GenesisFile, cfg: &NodeConfig) -> Result<Node, NodeError> {
        fs::create_dir_all(&cfg.data_dir)?;
        let params = genesis.chain.params();

        // Every allocation is checked against its own bytes before it is
        // allowed to be money.
        let allocations = genesis::contracts(genesis)?;
        let ids: Vec<ContractID> = allocations.iter().map(|(id, _)| *id).collect();

        let (mut chain, catchup) = build_chain(params, &ids)?;
        let derived = chain.tip().into_bytes();
        if derived != genesis.genesis_hash.0 {
            return Err(NodeError::GenesisHashMismatch {
                recorded: hex::encode(genesis.genesis_hash.0),
                derived: hex::encode(derived),
            });
        }

        let mut utxos = UtxoSet::new();
        let mut outputs = OutputIndex::new();
        let mut txindex = TxIndex::new();

        txindex.insert_block(block_summary(chain.header(), 0, 0, 0, 0));

        for ((id, contract), record) in allocations.iter().zip(&genesis.contracts) {
            utxos.insert_transient(*id);
            outputs.insert(
                *id,
                OutputRecord {
                    height: 0,
                    txid: GENESIS_TXID,
                    predicate: contract.predicate.to_point().to_bytes(),
                    contract: record.bytes.0.clone(),
                    note: None,
                },
            );
        }
        if let Some(catchup) = catchup {
            utxos.apply_catchup(&catchup)?;
        }

        let mut store = BlockStore::open(&cfg.blocks_path())?;
        // An archived block was validated when it was appended, so one that
        // will not decode or connect now means the archive is wrong, not
        // that this block is skippable.
        for block in store.replay(params)? {
            let height = block.header.height;
            connect_block(
                &mut chain,
                &mut utxos,
                &mut txindex,
                &mut outputs,
                None,
                &block,
            )
            .map_err(|source| NodeError::Replay {
                height,
                source: Box::new(source),
            })?;
        }

        let mempool = Mempool::new(&chain, mempool_policy(params, cfg.minimum_fee));

        Ok(Node {
            params,
            chain,
            mempool,
            utxos,
            txindex,
            outputs,
            store,
        })
    }

    /// Offers a transaction to the mempool.
    pub fn submit(&mut self, block_tx_bytes: &[u8]) -> Result<TxID, NodeError> {
        let block_tx =
            BlockTx::from_bytes_bounded(block_tx_bytes, self.params.version, self.params.limits)
                .map_err(NodeError::Decode)?;
        Ok(self.mempool.admit(block_tx)?.txid())
    }

    /// The current tip.
    pub fn tip(&self) -> TipInfo {
        TipInfo {
            hash: self.chain.tip(),
            height: self.chain.height(),
            contract_root: self.chain.state_commitment().contracts.0,
        }
    }

    /// What the node knows about one contract.
    pub fn proof(&self, id: &ContractID) -> ProofStatus {
        if let Some(proof) = self.utxos.get(id) {
            return ProofStatus::Unspent(proof.clone());
        }
        if let Some(spend) = self.outputs.spend_of(id) {
            return ProofStatus::Spent {
                height: spend.height,
                txid: spend.txid,
            };
        }
        ProofStatus::Unknown
    }

    /// The same question for several contracts.
    pub fn proofs(&self, ids: &[ContractID]) -> Vec<(ContractID, ProofStatus)> {
        ids.iter().map(|id| (*id, self.proof(id))).collect()
    }

    /// One contract as the node archived it.
    pub fn contract(&self, id: &ContractID) -> Option<&OutputRecord> {
        self.outputs.get(id)
    }

    /// A header and its executions, including internal deliveries.
    pub fn block(&self, height: u64) -> Result<BlockResult, NodeError> {
        let summary = self
            .txindex
            .block(height)
            .cloned()
            .ok_or_else(|| NodeError::NotFound(format!("block {height}")))?;
        let executions = self
            .txindex
            .at_height(height)
            .iter()
            .filter_map(|id| self.txindex.get(id))
            .map(|r| r.summary.clone())
            .collect();
        Ok(BlockResult {
            summary,
            executions,
        })
    }

    /// Confirmed external or internal execution details.
    pub fn transaction(
        &self,
        id: TxId,
        decode_effects: bool,
    ) -> Result<TransactionResult, NodeError> {
        let record = self
            .txindex
            .get(&TxID(id.0))
            .ok_or_else(|| NodeError::NotFound(format!("transaction {id}")))?;

        Ok(TransactionResult {
            summary: record.summary.clone(),
            height: record.height,
            block: BlockId(record.block.into_bytes()),
            log: record.log.clone(),
            effects: if decode_effects {
                Some(inspect::effects(&record.log)?)
            } else {
                None
            },
        })
    }

    /// Confirmed executions, newest first.
    pub fn transactions(
        &self,
        before: Option<TxId>,
        limit: u32,
    ) -> Result<TransactionsResult, NodeError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(NodeError::Limit {
                got: limit as usize,
                max: MAX_PAGE_SIZE as usize,
            });
        }

        self.txindex
            .transactions(before.map(|id| TxID(id.0)), limit as usize)
            .ok_or_else(|| NodeError::NotFound("transaction cursor".into()))
    }

    /// Where a transaction is.
    pub fn tx_status(&self, txid: &TxID) -> TxStatus {
        if self.mempool.entries().any(|entry| entry.txid() == *txid) {
            return TxStatus::Mempool;
        }
        match self.txindex.get(txid) {
            Some(record) => TxStatus::Confirmed {
                height: record.height,
                block: record.block,
            },
            None => TxStatus::Unknown,
        }
    }

    /// Every contract created at `since_height` or later under one of these
    /// predicates, in height order, each with the note that followed it.
    pub fn scan(&self, predicates: &[[u8; 32]], since_height: u64) -> Vec<ScanEntry> {
        let unique: BTreeSet<[u8; 32]> = predicates.iter().copied().collect();
        let mut hits = Vec::new();
        for predicate in unique {
            for id in self.outputs.under(&predicate) {
                let Some(record) = self.outputs.get(id) else {
                    continue;
                };
                if record.height < since_height {
                    continue;
                }
                hits.push(ScanEntry {
                    id: ContractId(*id),
                    height: record.height,
                    txid: TxId(record.txid.0),
                    predicate: PredicatePoint(record.predicate),
                    bytes: ContractEnvelope(record.contract.clone()),
                    note: record.note.clone().map(NoteEnvelope),
                    spent: self.outputs.spend_of(id).map(|spend| SpentAt {
                        height: spend.height,
                        txid: TxId(spend.txid.0),
                    }),
                });
            }
        }
        // Stable, so contracts created in one block keep their order.
        hits.sort_by_key(|hit| hit.height);
        hits
    }

    /// Whether a proof really proves membership at this tip.
    ///
    /// A `Transient` proof never does: it is a promise that a block is about
    /// to insert the leaf, not evidence that one has.
    pub fn verify_proof(&self, id: &ContractID, proof: &Proof) -> bool {
        let Some(path) = proof.as_path() else {
            return false;
        };
        self.chain
            .contract_forest()
            .verify(&ContractLeaf(*id), path, &utreexo_hasher::<ContractLeaf>())
            .is_ok()
    }

    /// How many transactions are waiting.
    pub fn mempool_len(&self) -> usize {
        self.mempool.len()
    }

    /// How many contracts are unspent.
    pub fn utxo_count(&self) -> usize {
        self.utxos.len()
    }

    /// How many blocks are archived.
    pub fn block_count(&self) -> usize {
        self.store.len()
    }

    /// Mints the next block from whatever the mempool holds.
    ///
    /// The order below is the whole of this method's correctness, and the
    /// two places it is unusual are called out where they happen.
    #[cfg(any(test, feature = "devnet"))]
    pub fn mint_block(&mut self) -> Result<BlockHash, NodeError> {
        // 1. Write every candidate down before draining the pool. `BlockTx`
        //    is not `Clone` and `build_block` consumes what it is given, so
        //    bytes are the only form that survives a retry — and a candidate
        //    this node cannot write down is one it could not put back.
        let candidates: Vec<(TxID, Vec<u8>)> = self
            .mempool
            .entries()
            .map(|entry| Ok((entry.txid(), entry.transaction().to_bytes()?)))
            .collect::<Result<_, CellError>>()
            .map_err(NodeError::Cell)?;
        let _ = self.mempool.take_all();

        let height = self.chain.height().saturating_add(1);
        let core = core_block_hash(height);
        // The pool is already drained, so an error here would take every
        // candidate with it — and the minter treats a failed build as
        // recoverable and keeps ticking. Put them all back first.
        let (block, taken, mut dropped) = match self.build_next(core, height, &candidates) {
            Ok(built) => built,
            Err(error) => {
                let restored = self.readmit(&candidates);
                eprintln!(
                    "no block at height {height}: {error}; kept {restored} of {} transaction(s)",
                    candidates.len()
                );
                return Err(error);
            }
        };

        // 2. The leftovers go back now, while the tip is still the one they
        //    were built against. The cost is known and accepted: a
        //    transaction spending a contract this very block creates is
        //    refused here rather than deferred, and its sender resubmits.
        dropped += candidates.len() - taken - self.readmit(&candidates[taken..]);

        // 3. Disk before memory, always.
        self.store
            .append(&block)
            .map_err(|source| NodeError::Archive {
                height,
                source: Box::new(source.into()),
            })?;

        let (hash, rebase) = connect_block(
            &mut self.chain,
            &mut self.utxos,
            &mut self.txindex,
            &mut self.outputs,
            Some(&mut self.mempool),
            &block,
        )
        .map_err(|source| NodeError::Archive {
            height,
            source: Box::new(source),
        })?;

        dropped += rebase.dropped;
        println!(
            "block {height} {} txs={} dropped={dropped}",
            hex::encode(hash.as_bytes()),
            block.transactions.len()
        );
        Ok(hash)
    }

    /// Builds the largest prefix of `candidates` that fits in a block.
    ///
    /// Returns the block, how many candidates it accounted for, and how many
    /// of those were dropped outright.
    #[cfg(any(test, feature = "devnet"))]
    fn build_next(
        &mut self,
        core: [u8; 32],
        height: u64,
        candidates: &[(TxID, Vec<u8>)],
    ) -> Result<(Block, usize, usize), NodeError> {
        let mut take = candidates.len();
        loop {
            let transactions = self.decode_batch(&candidates[..take])?;
            match self.chain.build_block(core, transactions) {
                Ok(block) => return Ok((block, take, 0)),
                // Too much for one block: halve and try again. The rest are
                // put back by the caller. Both shapes have to be caught —
                // `check_header` counts transactions and witness bytes
                // itself and says `LimitExceeded`, but it finishes by
                // re-decoding the whole encoded block, and that bound comes
                // back as a `CellError`. A block that is merely too big
                // must never look like a bad transaction.
                Err(error) if take > 1 && is_too_big(&error) => take /= 2,
                // Anything else is one bad transaction somewhere in the
                // batch, and one bad transaction must never halt minting.
                Err(error) => {
                    let block = self.chain.build_block(core, Vec::new())?;
                    // Said only once the empty block is a fact.
                    for (txid, _) in &candidates[..take] {
                        eprintln!(
                            "dropping tx {} at height {height}: {error}",
                            hex::encode(txid.0)
                        );
                    }
                    return Ok((block, take, take));
                }
            }
        }
    }

    /// Offers a slice of the byte snapshot back to the mempool, and says
    /// how many it took. Anything refused is gone, and says so.
    #[cfg(any(test, feature = "devnet"))]
    fn readmit(&mut self, candidates: &[(TxID, Vec<u8>)]) -> usize {
        let height = self.chain.height().saturating_add(1);
        candidates
            .iter()
            .filter(|(txid, bytes)| match self.submit(bytes) {
                Ok(_) => true,
                Err(error) => {
                    eprintln!(
                        "dropping tx {} at height {height}: {error}",
                        hex::encode(txid.0)
                    );
                    false
                }
            })
            .count()
    }

    /// Decodes a slice of the byte snapshot back into transactions.
    #[cfg(any(test, feature = "devnet"))]
    fn decode_batch(&self, candidates: &[(TxID, Vec<u8>)]) -> Result<Vec<BlockTx>, NodeError> {
        candidates
            .iter()
            .map(|(_, bytes)| {
                BlockTx::from_bytes_bounded(bytes, self.params.version, self.params.limits)
                    .map_err(NodeError::Decode)
            })
            .collect()
    }
}

/// Whether a build failure means the block was too large rather than that
/// something in it was wrong.
#[cfg(any(test, feature = "devnet"))]
fn is_too_big(error: &ChainError) -> bool {
    matches!(
        error,
        ChainError::LimitExceeded | ChainError::Cells(CellError::LimitExceeded)
    )
}

/// This node's mempool policy for a chain with these parameters.
///
/// The per-transaction caps are taken from consensus, not from
/// `MempoolPolicy::default()`, which pins them to the *default* block
/// limits: a network that raises `max_transaction_gas` in
/// `chainparams.toml` would otherwise have every node silently refuse to
/// relay what the chain itself accepts. The two capacity fields are
/// genuinely local — they bound this pool, not one block — and keep their
/// defaults.
fn mempool_policy(params: ChainParams, minimum_fee: u64) -> MempoolPolicy {
    MempoolPolicy {
        max_transaction_gas: params.limits.max_transaction_gas,
        max_gas_credit: params.limits.max_gas_credit,
        max_script_bytes: params.limits.max_transaction_script_bytes,
        max_multiplications: params.limits.max_multiplications_per_transaction,
        max_proofs_per_transaction: params.limits.max_proofs_per_transaction,
        max_proof_depth: params.limits.max_proof_depth,
        minimum_fee,
        ..Default::default()
    }
}

/// The chain a genesis file describes.
///
/// Seeded with the allocations on a devnet build; empty otherwise, because
/// contracts enter a public chain by minting and nothing else.
#[cfg(any(test, feature = "devnet"))]
fn build_chain(
    params: ChainParams,
    ids: &[ContractID],
) -> Result<(Blockchain, Option<flamechain::utreexo::Catchup>), NodeError> {
    let (chain, catchup) = Blockchain::devnet_genesis(params, ids)?;
    Ok((chain, Some(catchup)))
}

#[cfg(not(any(test, feature = "devnet")))]
fn build_chain(
    params: ChainParams,
    ids: &[ContractID],
) -> Result<(Blockchain, Option<flamechain::utreexo::Catchup>), NodeError> {
    if !ids.is_empty() {
        return Err(NodeError::DevnetRequired);
    }
    Ok((Blockchain::new(params)?, None))
}

/// Applies one block to the chain and to everything the chain forgets.
///
/// One function for minting and for replay, so an archived chain and a live
/// one cannot drift apart. `mempool` is `None` during replay, where no pool
/// exists yet.
fn connect_block(
    chain: &mut Blockchain,
    utxos: &mut UtxoSet,
    txindex: &mut TxIndex,
    outputs: &mut OutputIndex,
    mempool: Option<&mut Mempool>,
    block: &Block,
) -> Result<(BlockHash, RebaseReport), NodeError> {
    // 1. `connect` reports only `ExecutionRecord { kind, txid }`, so the
    //    effects are read through an observer during block validation.
    //    A log decoded from this node's own archive would be the archive
    //    checking itself.
    let height = block.header.height;
    let block_id = block.header.id();
    let size_bytes = block.to_bytes()?.len() as u64;
    let mut messages = BTreeMap::new();
    let mut staged: Vec<(TxID, TxRecord, Vec<UtxoEffect>)> = Vec::new();

    // 2. Nothing is recorded before the chain has accepted the block.
    let applied = chain.connect_with_observer(block, |record, log, error| {
        let txid = record.txid;

        let mut changes = Vec::new();
        let mut received = None;
        let mut inputs = 0;
        let mut output_count = 0;
        let mut sends = 0;
        let mut fees = 0u128;

        // 3. In log order, not inputs then outputs: a contract created and
        //    spent inside one block has to end up spent, and two passes
        //    would leave it unspent and spent at once. An output keeps the
        //    entry after it when that entry is `Data`: the output's note,
        //    kept byte for byte and never parsed here.
        let entries = log.entries();
        for (index, entry) in entries.iter().enumerate() {
            match entry {
                TxEntry::Input(id) => {
                    inputs += 1;
                    changes.push(UtxoEffect::Input(*id));
                }
                TxEntry::Output(contract) => {
                    output_count += 1;
                    let id = contract.id();
                    changes.push(UtxoEffect::Output(
                        id,
                        OutputRecord {
                            height,
                            txid,
                            predicate: contract.predicate.to_point().to_bytes(),
                            contract: contract_bytes(contract)?,
                            note: match entries.get(index + 1) {
                                Some(TxEntry::Data(note)) => Some(note.clone()),
                                _ => None,
                            },
                        },
                    ));
                }
                TxEntry::Fee(q) => fees += u128::from(*q),
                TxEntry::Send(m) => {
                    sends += 1;
                    let target = ActorId(m.target.to_hash());
                    messages.insert(m.id().0, (TxId(txid.0), target));
                }
                TxEntry::Receive(id) => {
                    received = Some(messages.remove(id).ok_or(ChainError::InvalidEffectLog)?);
                }
                _ => {}
            }
        }

        let execution = match (record.kind, received, error) {
            (ExecutionKind::External, None, None) => ExecutionData::External,
            (ExecutionKind::Internal, Some((parent, actor)), None) => {
                ExecutionData::Internal { parent, actor }
            }
            (ExecutionKind::InternalFailed, Some((parent, actor)), Some(error)) => {
                ExecutionData::InternalFailed {
                    parent,
                    actor,
                    error: error.to_owned(),
                }
            }
            _ => return Err(ChainError::InvalidEffectLog),
        };
        let summary = TransactionSummary {
            id: TxId(txid.0),
            execution,
            fee_sparks: fees.to_string(),
            inputs,
            outputs: output_count,
            messages: sends,
        };

        // 4. The log itself, for anyone who asks what this transaction did.
        staged.push((
            txid,
            TxRecord {
                height,
                block: block_id,
                log: log_bytes(log)?,
                summary,
            },
            changes,
        ));
        Ok(())
    })?;

    // 5. Every execution the chain ran that was not one of these external
    //    transactions produced effects this node can now see through
    //    `connect_with_observer`. It is reachable
    //    today, without a single actor existing — a `send` in an external
    //    transaction finds no actor, bounces, and the bounce mints a
    //    refund contract under the sender's refund predicate. That
    //    contract is real and in the accumulator, and this node can serve
    //    both a proof and a record for it.
    for (txid, record, changes) in staged {
        for change in changes {
            match change {
                UtxoEffect::Input(id) => {
                    utxos.remove(&id);
                    outputs.spend(id, SpendRecord { height, txid });
                }
                UtxoEffect::Output(id, output) => {
                    utxos.insert_transient(id);
                    outputs.insert(id, output);
                }
            }
        }
        txindex.insert(txid, record);
    }

    let internal = applied
        .records
        .iter()
        .filter(|r| r.kind != ExecutionKind::External)
        .count() as u32;
    let failed = applied
        .records
        .iter()
        .filter(|r| r.kind == ExecutionKind::InternalFailed)
        .count() as u32;

    txindex.insert_block(block_summary(
        &block.header,
        block.transactions.len() as u32,
        internal,
        failed,
        size_bytes,
    ));

    // 6. Every surviving proof moves to the new tip, and the ids this block
    //    inserted turn from `Transient` into `Committed`. After the removals,
    //    because a deleted leaf has no proof to lift; after the inserts,
    //    because a new id has to be in the map to be promoted.
    utxos.apply_catchup(&applied.catchup)?;

    // 7. Whatever the mempool still holds is replayed on the new tip.
    let rebase = match mempool {
        Some(mempool) => {
            let report = mempool.rebase(chain, Some(&applied.catchup));
            if report.dropped != 0 {
                eprintln!(
                    "mempool dropped {} transaction(s) rebasing onto height {height}",
                    report.dropped
                );
            }
            report
        }
        None => RebaseReport {
            kept: 0,
            dropped: 0,
        },
    };

    Ok((applied.id, rebase))
}

enum UtxoEffect {
    Input(ContractID),
    Output(ContractID, OutputRecord),
}

fn block_summary(
    header: &flamechain::BlockHeader,
    transactions: u32,
    internal: u32,
    failed: u32,
    size_bytes: u64,
) -> BlockSummary {
    BlockSummary {
        id: BlockId(header.id().into_bytes()),
        header: BlockHeader {
            version: header.version,
            height: header.height,
            core_block_hash: header.core_block_hash,
            parent: BlockId(header.parent.into_bytes()),
            witness_root: header.witness_root,
            effects_root: header.effects_root,
            state: StateCommitment {
                contracts: header.state.contracts.0,
                actors: header.state.actors,
                available_storage_units: header.state.available_storage_units,
            },
        },
        transactions,
        internal,
        failed,
        size_bytes,
    }
}

/// Something the node could not do.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// The node has no such contract.
    #[error("no contract {0}")]
    UnknownContract(String),
    /// A requested block or confirmed execution is absent.
    #[error("not found: {0}")]
    NotFound(String),
    /// A request named more items than the method allows.
    #[error("{got} items, at most {max}")]
    Limit {
        /// The bound.
        max: usize,
        /// What was asked for.
        got: usize,
    },
    /// Inbound bytes that this chain will not read as a transaction —
    /// malformed, or well formed and past one of this chain's limits. The
    /// `CellError` says which.
    #[error("the submitted bytes do not decode as a transaction under this chain's limits: {0}")]
    Decode(CellError),
    /// The mempool refused a transaction.
    #[error(transparent)]
    Mempool(#[from] MempoolError),
    /// The recorded genesis hash is not the one this node derives.
    #[error("genesis.json records {recorded} but this node derives {derived}")]
    GenesisHashMismatch {
        /// What the file says.
        recorded: String,
        /// What the parameters and allocations give.
        derived: String,
    },
    /// A genesis file that cannot be believed.
    #[error(transparent)]
    Genesis(#[from] GenesisError),
    /// An archived block that will not replay.
    #[error("the block archived at height {height} does not replay: {source}")]
    Replay {
        /// Which height.
        height: u64,
        /// Why.
        source: Box<NodeError>,
    },
    /// A genesis file with allocations, on a build that cannot have them.
    #[error("this build has no genesis allocations; rebuild with --features devnet")]
    DevnetRequired,
    /// Disk and memory may no longer agree.
    #[error("the archive and memory may disagree at height {height}: {source}")]
    Archive {
        /// Which height.
        height: u64,
        /// Why.
        source: Box<NodeError>,
    },
    /// The chain refused something.
    #[error(transparent)]
    Chain(#[from] ChainError),
    /// A transaction did not verify.
    #[error(transparent)]
    Vm(#[from] VMError),
    /// A proof could not be lifted.
    #[error(transparent)]
    Utreexo(#[from] UtreexoError),
    /// This node's own encoding failed.
    #[error(transparent)]
    Cell(#[from] CellError),
    /// The archive.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The filesystem.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
