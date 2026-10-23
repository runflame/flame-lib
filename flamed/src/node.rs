//! One node: a chain, a mempool, and the three things the chain forgets.
//!
//! A `Blockchain` keeps roots and an accumulator. It does not keep a proof
//! for anyone, it does not remember which transaction created which
//! contract, and it never knew which predicate locked it. A wallet needs all
//! three, so this node keeps them: `UtxoSet` for the proofs, `TxIndex` and
//! `OutputIndex` for the history. All three are rebuilt by replaying
//! `blocks.bin`, which is the only durable state there is.
//!
//! A block's time is in its header, put there by the minter's clock; a
//! transaction's admission time is this node's clock, kept in memory only.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::{Arc, Mutex};

use flamechain::utreexo::{Proof, UtreexoError};
use flamechain::{
    utreexo_hasher, Block, BlockHash, BlockTx, Blockchain, ChainError, ChainParams, ContractLeaf,
    ExecutionKind, Mempool, MempoolError, MempoolPolicy, RebaseReport,
};
#[cfg(any(test, feature = "devnet"))]
use flamechain::MintProof;
use flamed_rpc::{
    ActorCode, ActorId, ActorQueryResult, ActorResult, ActorStateEnvelope, BlockHeader, BlockId,
    BlockResult, BlockSummary, BlockTxEnvelope, BlocksResult, ContractEnvelope, ContractId,
    Counterparty, ExecutionData, HistoryContract, HistoryEntry, HistoryResult, NoteEnvelope,
    PendingTransaction, PendingTransactionsResult, PredicatePoint, ProofBytes, ScanEntry, SpentAt,
    StateCommitment, TipResult, TransactionResult, TransactionSummary, TransactionsResult, TxId,
    UnspentEntry, UnspentResult, ValueEnvelope, MAX_HISTORY_ADDRESSES, MAX_PAGE_SIZE,
    MAX_QUERY_BYTES, MAX_QUERY_GAS,
};
use flamekd::{Network, ReceivingAddress, TrackingAddress};
use flamevm::{
    ActorID, Cell, CellDecode, CellEncode, CellEnvelope, CellError, CellID, CellReader, ContractID,
    Predicate, TxEntry, TxID, VMError, Value,
};

use crate::cells::{contract_bytes, log_bytes, proof_bytes};
use crate::config::{GenesisFile, NodeConfig};
use crate::genesis::{self, GenesisError};
use crate::history::{self, Cursor, Direction, END_OF_BLOCK};
use crate::index::{OutputIndex, OutputRecord, Position, SpendRecord, Touch, TxIndex, TxRecord};
use crate::indexer::{unix_now, CoreError, CoreIndexer, MockIndexer};
use crate::inspect;
use crate::store::{BlockStore, StoreError};
use crate::utxos::UtxoSet;

/// The txid the indexes record for a genesis allocation, which no
/// transaction created.
pub const GENESIS_TXID: TxID = TxID([0; 32]);

/// One node, shared by the RPC handlers and the minter.
pub type SharedNode = Arc<Mutex<Node>>;

// A compact shared Cell graph can expand into many Dict values during decoding.
struct QueryDecode<'a> {
    envelope: &'a mut CellEnvelope,
    gas: u64,
}

impl CellReader for QueryDecode<'_> {
    fn read(&mut self, id: CellID, resident: Option<&Arc<Cell>>) -> Result<Arc<Cell>, CellError> {
        let cell = self.envelope.read(id, resident)?;
        self.gas = self
            .gas
            .checked_sub(1 + cell.record_size() as u64 + cell.refs().len() as u64)
            .ok_or(CellError::ResourceExhausted)?;
        Ok(cell)
    }
}

pub use crate::indexer::core_block_hash;

/// An archival node.
pub struct Node {
    params: ChainParams,
    network: Network,
    chain: Blockchain,
    mempool: Mempool,
    utxos: UtxoSet,
    txindex: TxIndex,
    outputs: OutputIndex,
    store: BlockStore,
    // Read again only by the minter; a build without one asks it only
    // while opening.
    #[cfg_attr(not(any(test, feature = "devnet")), allow(dead_code))]
    indexer: Box<dyn CoreIndexer>,
    /// When each pending transaction was first admitted, by txid. A
    /// transaction that leaves the pool by confirming takes its time into
    /// `TxIndex`; one that leaves any other way takes it nowhere.
    received: BTreeMap<[u8; 32], u64>,
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
    /// Opens a node over the mock core indexer in `cfg.indexer_dir()`.
    pub fn open(genesis: &GenesisFile, cfg: &NodeConfig) -> Result<Node, NodeError> {
        let indexer = MockIndexer::open(&cfg.indexer_dir())?;
        Self::open_with(genesis, cfg, Box::new(indexer))
    }

    /// Opens a node: rebuilds the genesis, replays the archive, and starts
    /// with an empty mempool. `indexer` names the core tip each minted block
    /// anchors to.
    pub fn open_with(
        genesis: &GenesisFile,
        cfg: &NodeConfig,
        indexer: Box<dyn CoreIndexer>,
    ) -> Result<Node, NodeError> {
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
                    index: 0,
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
            network: genesis.chain.network.into(),
            chain,
            mempool,
            utxos,
            txindex,
            outputs,
            store,
            indexer,
            received: BTreeMap::new(),
        })
    }

    /// Offers a transaction to the mempool.
    ///
    /// The admission time is kept from the first admission: a transaction
    /// the minter takes out and puts back is the same pending transaction.
    pub fn submit(&mut self, block_tx_bytes: &[u8]) -> Result<TxID, NodeError> {
        let block_tx =
            BlockTx::from_bytes_bounded(block_tx_bytes, self.params.version, self.params.limits)
                .map_err(NodeError::Decode)?;
        let txid = self.mempool.admit(block_tx)?.txid();
        self.received.entry(txid.0).or_insert_with(unix_now);
        Ok(txid)
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

    /// A page of blocks, newest first.
    pub fn blocks(&self, before: Option<u64>, limit: u32) -> Result<BlocksResult, NodeError> {
        Ok(self.txindex.blocks(before, page_limit(limit)?))
    }

    /// A block header and its execution counts.
    pub fn block_summary(&self, height: u64) -> Result<&BlockSummary, NodeError> {
        self.txindex
            .block(height)
            .ok_or_else(|| NodeError::NotFound(format!("block {height}")))
    }

    /// A header and its executions, including internal deliveries.
    pub fn block(&self, height: u64) -> Result<BlockResult, NodeError> {
        let summary = self.block_summary(height)?.clone();
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

    /// Current actor code, state, and storage availability.
    pub fn actor(&self, id: ActorId) -> Result<ActorResult, NodeError> {
        let info = self
            .chain
            .actor_info(&ActorID::from(id.0))
            .map_err(|error| match error {
                ChainError::Vm(VMError::ActorNotFound) => {
                    NodeError::NotFound(format!("actor {id}"))
                }
                other => NodeError::Chain(other),
            })?;

        let tip = self.tip();

        Ok(ActorResult {
            id,
            height: tip.height,
            block: BlockId(tip.hash.into_bytes()),
            root_hash: flamed_rpc::CellId(info.root),
            frozen: info.frozen,
            code_hash: info.code_root.map(flamed_rpc::CellId),
            state_hash: info.state_root.map(flamed_rpc::CellId),
            code_size: info.code_size,
            state_size: info.state_size,
            storage_used: info.storage_used,
            storage_capacity: info.storage_capacity,
            code: info.code.map(|code| ActorCode(code.encode())),
            state: info.state.map(|state| ActorStateEnvelope(state.encode())),
            decoded_state: None,
            state_error: None,
            instructions: Vec::new(),
            code_error: None,
        })
    }

    /// Executes a getter with no authenticated caller and no write permission.
    /// An expected tip pins multi-hop lookups: callers restart if the tip changes.
    pub fn query_actor(
        &mut self,
        id: ActorId,
        argument: ValueEnvelope,
        gas_limit: u64,
        expected_block: Option<BlockId>,
    ) -> Result<ActorQueryResult, NodeError> {
        if gas_limit == 0 || gas_limit > MAX_QUERY_GAS {
            return Err(NodeError::QueryGasLimit(gas_limit));
        }
        if argument.0.len() > MAX_QUERY_BYTES {
            return Err(NodeError::Limit {
                max: MAX_QUERY_BYTES,
                got: argument.0.len(),
            });
        }
        let tip = self.tip();
        let block = BlockId(tip.hash.into_bytes());
        if expected_block.is_some_and(|expected| expected != block) {
            return Err(NodeError::StaleBlock);
        }
        let mut decode_gas = MAX_QUERY_GAS;
        let mut envelope = CellEnvelope::decode(&argument.0, MAX_QUERY_BYTES, &mut decode_gas)
            .map_err(NodeError::QueryDecode)?;
        let root = envelope
            .cells()
            .get(&envelope.root())
            .ok_or_else(|| NodeError::QueryDecode(CellError::MissingCell(envelope.root())))?;
        // Dict leaves receive the same recursive data checks inside the VM.
        if !matches!(root.payload().first(), Some(0 | 2 | 3 | 13)) {
            return Err(NodeError::QueryDecode(CellError::InvalidFormat));
        }
        let argument = Value::from_cell(
            &root,
            &mut QueryDecode {
                envelope: &mut envelope,
                gas: decode_gas,
            },
        )
        .map_err(NodeError::QueryDecode)?;
        let result = self
            .chain
            .query_actor(
                ActorID::from(id.0),
                argument,
                gas_limit,
                Arc::new(envelope.into_parts().1),
            )
            .map_err(|error| match error {
                VMError::ActorNotFound => NodeError::NotFound(format!("actor {id}")),
                other => NodeError::Query(other),
            })?;
        let value = result.value.to_envelope()?.encode();
        if value.len() > MAX_QUERY_BYTES {
            return Err(NodeError::Limit {
                max: MAX_QUERY_BYTES,
                got: value.len(),
            });
        }
        Ok(ActorQueryResult {
            block,
            height: tip.height,
            value: ValueEnvelope(value),
            gas_used: result.gas_used,
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
            block_time: self.block_time(record.height),
            received_at: record.received_at,
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
        self.txindex
            .transactions(before.map(|id| TxID(id.0)), page_limit(limit)?)
            .ok_or_else(|| NodeError::NotFound("transaction cursor".into()))
    }

    /// The first pending transactions in mempool admission order.
    pub fn pending_transactions(&self, limit: u32) -> Result<PendingTransactionsResult, NodeError> {
        let limit = page_limit(limit)?;
        Ok(PendingTransactionsResult {
            transactions: self
                .mempool
                .entries()
                .take(limit)
                .map(|entry| {
                    Ok(PendingTransaction {
                        id: TxId(entry.txid().0),
                        block_tx: BlockTxEnvelope(entry.transaction().to_bytes()?),
                        fee_sparks: entry.fee().to_string(),
                        witness_bytes: entry.witness_bytes() as u64,
                        received_at: self
                            .received
                            .get(&entry.txid().0)
                            .copied()
                            .unwrap_or_default(),
                        locktime: entry.transaction().tx.header.locktime,
                    })
                })
                .collect::<Result<_, CellError>>()?,
            total: self.mempool.len() as u64,
        })
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
                    time: self.block_time(record.height),
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

    /// Every unspent contract locked to one address, each with its proof at
    /// the tip, in creation order.
    ///
    /// With `with_pending`, the mempool is applied on top in admission
    /// order: what it spends is left out, and what it creates follows the
    /// confirmed contracts without a height or a proof. A contract one
    /// pending transaction creates and a later one spends is in neither.
    pub fn unspent(&self, address: &str, with_pending: bool) -> Result<UnspentResult, NodeError> {
        let predicate = address_predicate(address, self.network)?;

        let mut spent = BTreeSet::new();
        let mut pending = Vec::new();
        if with_pending {
            for entry in self.mempool.entries() {
                let entries = entry.log().entries();
                for (index, effect) in entries.iter().enumerate() {
                    match effect {
                        TxEntry::Input(id) => {
                            spent.insert(*id);
                        }
                        TxEntry::Output(contract)
                            if contract.predicate.to_point().to_bytes() == predicate =>
                        {
                            pending.push(UnspentEntry {
                                id: ContractId(contract.id()),
                                txid: TxId(entry.txid().0),
                                bytes: ContractEnvelope(contract_bytes(contract)?),
                                note: note_after(entries, index).map(NoteEnvelope),
                                height: None,
                                time: None,
                                received_at: self.received.get(&entry.txid().0).copied(),
                                proof: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        let mut outputs = Vec::new();
        for id in self.outputs.under(&predicate) {
            let (Some(proof), Some(record)) = (self.utxos.get(id), self.outputs.get(id)) else {
                continue;
            };
            if spent.contains(id) {
                continue;
            }
            outputs.push(UnspentEntry {
                id: ContractId(*id),
                txid: TxId(record.txid.0),
                bytes: ContractEnvelope(record.contract.clone()),
                note: record.note.clone().map(NoteEnvelope),
                height: Some(record.height),
                time: self.block_time(record.height),
                received_at: None,
                proof: Some(ProofBytes(proof_bytes(proof))),
            });
        }
        outputs.extend(
            pending
                .into_iter()
                .filter(|output| !spent.contains(&output.id.0)),
        );

        let tip = self.tip();
        Ok(UnspentResult {
            tip: TipResult {
                hash: BlockId(tip.hash.into_bytes()),
                height: tip.height,
                contract_root: tip.contract_root,
            },
            predicate: PredicatePoint(predicate),
            outputs,
        })
    }

    /// A page of what happened to a set of addresses; the RPC method
    /// `history` says what each argument and field means.
    pub fn history(
        &self,
        addresses: &[String],
        cursor: Option<&str>,
        limit: u32,
        with_pending: bool,
    ) -> Result<HistoryResult, NodeError> {
        let limit = page_limit(limit)?;
        if addresses.is_empty() || addresses.len() > MAX_HISTORY_ADDRESSES {
            return Err(NodeError::Limit {
                got: addresses.len(),
                max: MAX_HISTORY_ADDRESSES,
            });
        }
        let listed = addresses
            .iter()
            .map(|address| address_predicate(address, self.network))
            .collect::<Result<Vec<_>, _>>()?;
        let predicates: BTreeSet<[u8; 32]> = listed.iter().copied().collect();
        let set = history::address_set(&predicates);
        let cursor = cursor.map(|text| self.read_cursor(text, set)).transpose()?;
        let direction = cursor.map_or(Direction::Older, |cursor| cursor.direction);

        let (touches, more) = history::page(
            predicates
                .iter()
                .map(|predicate| self.outputs.history(predicate)),
            direction,
            cursor.map(|cursor| cursor.position),
            limit,
        );
        let entries = touches
            .iter()
            .map(|touch| self.history_entry(touch, &predicates))
            .collect();

        let tip = self.tip();
        let at = |direction, position: Position| Cursor {
            direction,
            position,
            block: self.block_id(position.0),
            set,
        };
        let last = touches.last().map(|touch| touch.position);
        // Everything up to the tip has been read once a page reaches it.
        let at_tip = at(Direction::Newer, (tip.height, END_OF_BLOCK)).to_string();
        let (older, newer, has_newer) = match (direction, cursor) {
            (Direction::Older, first) => (
                last.filter(|_| more)
                    .map(|position| at(Direction::Older, position).to_string()),
                first.is_none().then_some(at_tip),
                false,
            ),
            (Direction::Newer, _) => match last.filter(|_| more) {
                Some(position) => (None, Some(at(Direction::Newer, position).to_string()), true),
                None => (None, Some(at_tip), false),
            },
        };

        let (pending, pending_truncated) = match (with_pending, direction, cursor) {
            (true, Direction::Newer, _) | (true, _, None) => {
                let (pending, truncated) = self.pending_history(&predicates)?;
                (Some(pending), truncated)
            }
            _ => (None, false),
        };

        Ok(HistoryResult {
            tip: TipResult {
                hash: BlockId(tip.hash.into_bytes()),
                height: tip.height,
                contract_root: tip.contract_root,
            },
            predicates: listed.into_iter().map(PredicatePoint).collect(),
            entries,
            older,
            newer,
            has_newer,
            pending,
            pending_truncated,
        })
    }

    /// A cursor this node wrote, for these addresses, on a block still in
    /// the chain.
    fn read_cursor(&self, text: &str, set: [u8; 8]) -> Result<Cursor, NodeError> {
        let cursor = Cursor::parse(text)
            .ok_or_else(|| NodeError::Cursor(format!("{text:?} is not a history cursor")))?;
        if cursor.set != set {
            return Err(NodeError::Cursor(
                "the cursor was made for other addresses".into(),
            ));
        }
        let height = cursor.position.0;
        if self.txindex.block(height).map(|block| block.id.0) != Some(cursor.block) {
            return Err(NodeError::StaleCursor { height });
        }
        Ok(cursor)
    }

    /// The id of the block at a height this node has.
    fn block_id(&self, height: u64) -> [u8; 32] {
        self.txindex
            .block(height)
            .map(|block| block.id.0)
            .expect("a position or the tip names a block this node has")
    }

    /// One confirmed transaction in a history.
    fn history_entry(&self, touch: &Touch, predicates: &BTreeSet<[u8; 32]>) -> HistoryEntry {
        let record = self.txindex.get(&touch.txid);
        let (all_spent, all_created) = self
            .outputs
            .of_tx(&touch.txid)
            .map_or((&[][..], &[][..]), |tx| (&tx.spent[..], &tx.created[..]));
        let predicate_of = |id: &ContractID| self.outputs.get(id).map(|output| output.predicate);
        let (height, _) = touch.position;
        HistoryEntry {
            txid: TxId(touch.txid.0),
            height: Some(height),
            time: self.block_time(height),
            received_at: record.and_then(|record| record.received_at),
            // The genesis allocation is in no block's executions, and
            // paid no fee.
            fee_sparks: record
                .map_or_else(|| "0".into(), |record| record.summary.fee_sparks.clone()),
            created: touch
                .created
                .iter()
                .filter_map(|id| self.history_contract(id))
                .collect(),
            spent: touch
                .spent
                .iter()
                .filter_map(|id| self.history_contract(id))
                .collect(),
            others_spent: self
                .counterparties(all_spent.iter().filter_map(predicate_of), predicates),
            others_created: self
                .counterparties(all_created.iter().filter_map(predicate_of), predicates),
        }
    }

    /// The predicates in `seen` that are not among `predicates`, each once
    /// in the order first seen, with their tracking addresses.
    fn counterparties(
        &self,
        seen: impl IntoIterator<Item = [u8; 32]>,
        predicates: &BTreeSet<[u8; 32]>,
    ) -> Vec<Counterparty> {
        let mut others: Vec<[u8; 32]> = Vec::new();
        for predicate in seen {
            if !predicates.contains(&predicate) && !others.contains(&predicate) {
                others.push(predicate);
            }
        }
        others
            .into_iter()
            .map(|predicate| Counterparty {
                predicate: PredicatePoint(predicate),
                address: TrackingAddress::from_bytes(&predicate)
                    .ok()
                    .map(|address| address.to_bech32(self.network)),
            })
            .collect()
    }

    /// A contract as a history shows it, as it was created.
    fn history_contract(&self, id: &ContractID) -> Option<HistoryContract> {
        let output = self.outputs.get(id)?;
        Some(HistoryContract {
            id: ContractId(*id),
            predicate: PredicatePoint(output.predicate),
            bytes: ContractEnvelope(output.contract.clone()),
            note: output.note.clone().map(NoteEnvelope),
        })
    }

    /// The pending transactions that touch `predicates`, in admission
    /// order, and whether the list was cut at `MAX_PAGE_SIZE`.
    ///
    /// A pending transaction can spend what an earlier pending one created,
    /// so the contracts the pool creates are kept as they go by: whole for
    /// these addresses, by predicate for the others.
    fn pending_history(
        &self,
        predicates: &BTreeSet<[u8; 32]>,
    ) -> Result<(Vec<HistoryEntry>, bool), NodeError> {
        let mut created_here = BTreeMap::new();
        let mut pool_predicates = BTreeMap::new();
        let mut entries = Vec::new();
        for entry in self.mempool.entries() {
            let log = entry.log().entries();
            let (mut created, mut spent, mut fee) = (Vec::new(), Vec::new(), 0u128);
            let (mut all_spent, mut all_created) = (Vec::new(), Vec::new());
            for (index, effect) in log.iter().enumerate() {
                match effect {
                    TxEntry::Input(id) => {
                        all_spent.extend(
                            pool_predicates
                                .remove(id)
                                .or_else(|| self.outputs.get(id).map(|output| output.predicate)),
                        );
                        let ours = created_here.remove(id).or_else(|| {
                            self.history_contract(id)
                                .filter(|contract| predicates.contains(&contract.predicate.0))
                        });
                        spent.extend(ours);
                    }
                    TxEntry::Output(contract) => {
                        let predicate = contract.predicate.to_point().to_bytes();
                        pool_predicates.insert(contract.id(), predicate);
                        all_created.push(predicate);
                        if predicates.contains(&predicate) {
                            let ours = HistoryContract {
                                id: ContractId(contract.id()),
                                predicate: PredicatePoint(predicate),
                                bytes: ContractEnvelope(contract_bytes(contract)?),
                                note: note_after(log, index).map(NoteEnvelope),
                            };
                            created_here.insert(contract.id(), ours.clone());
                            created.push(ours);
                        }
                    }
                    TxEntry::Fee(q) => fee += u128::from(*q),
                    _ => {}
                }
            }
            if created.is_empty() && spent.is_empty() {
                continue;
            }
            if entries.len() == MAX_PAGE_SIZE as usize {
                return Ok((entries, true));
            }
            entries.push(HistoryEntry {
                txid: TxId(entry.txid().0),
                height: None,
                time: None,
                received_at: self.received.get(&entry.txid().0).copied(),
                fee_sparks: fee.to_string(),
                created,
                spent,
                others_spent: self.counterparties(all_spent, predicates),
                others_created: self.counterparties(all_created, predicates),
            });
        }
        Ok((entries, false))
    }

    /// The time in the header of the block at `height`; none for genesis.
    pub fn block_time(&self, height: u64) -> Option<u64> {
        self.txindex.block(height).and_then(|block| block.time)
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
        // 0. Ask the indexer first: if it cannot answer, nothing has been
        //    touched yet and the next tick tries again. The time is this
        //    node's clock, held at the tip's if the clock has gone back.
        let height = self.chain.height().saturating_add(1);
        let anchor = self.next_anchor()?;
        let time = unix_now().max(self.chain.header().time);

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

        // The pool is already drained, so an error here would take every
        // candidate with it — and the minter treats a failed build as
        // recoverable and keeps ticking. Put them all back first.
        let (block, taken, mut dropped) = match self.build_next(&anchor, time, height, &candidates) {
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
        self.settle_received();

        dropped += rebase.dropped;
        let burns: usize = block.mint.core_blocks.iter().map(Vec::len).sum();
        println!(
            "block {height} {} core={} burns={burns} txs={} dropped={dropped}",
            hex::encode(hash.as_bytes()),
            block.header.core_height,
            block.transactions.len()
        );
        Ok(hash)
    }

    /// The core block the next block is minted on, and the mint proof that
    /// gets it there: every core block since the tip's anchor, up to the
    /// indexer's tip, as many as one block may prove. A core chain that has
    /// not moved keeps the tip's anchor and proves nothing.
    #[cfg(any(test, feature = "devnet"))]
    fn next_anchor(&mut self) -> Result<Anchor, NodeError> {
        let header = self.chain.header();
        let (current, current_hash) = (header.core_height, header.core_block_hash);
        let tip = self.indexer.tip()?;
        if tip.height < current {
            return Err(CoreError::Inconsistent(format!(
                "its tip is core block {} but the chain is anchored at {current}",
                tip.height
            ))
            .into());
        }
        if tip.height == current {
            return Ok(Anchor {
                core_height: current,
                core_hash: current_hash,
                mint: MintProof::default(),
            });
        }

        let limits = self.params.limits;
        let from = current + 1;
        let to = tip.height.min(current.saturating_add(limits.max_core_blocks as u64));
        let mut mint = MintProof::default();
        let mut last = None;
        let mut burns = 0usize;
        for (expected, core) in (from..=to).zip(self.indexer.blocks(from, to)?) {
            if core.block.height != expected {
                return Err(CoreError::Inconsistent(format!(
                    "asked for core block {expected}, got {}",
                    core.block.height
                ))
                .into());
            }
            // Whole core blocks only, while their burns fit in one block.
            burns += core.burns.len();
            if burns > limits.max_burns && last.is_some() {
                break;
            }
            last = Some(core.block);
            mint.core_blocks.push(core.burns);
        }
        let last = last.ok_or_else(|| {
            CoreError::Inconsistent(format!("core block {from} is below its tip but missing"))
        })?;
        Ok(Anchor {
            core_height: last.height,
            core_hash: last.hash,
            mint,
        })
    }

    /// Hands each confirmed transaction its admission time, and forgets the
    /// times of transactions that left the pool without confirming.
    #[cfg(any(test, feature = "devnet"))]
    fn settle_received(&mut self) {
        let pending: BTreeSet<[u8; 32]> =
            self.mempool.entries().map(|entry| entry.txid().0).collect();
        for (txid, time) in std::mem::take(&mut self.received) {
            if pending.contains(&txid) {
                self.received.insert(txid, time);
            } else {
                self.txindex.set_received(&TxID(txid), time);
            }
        }
    }

    /// Builds the largest prefix of `candidates` that fits in a block.
    ///
    /// Returns the block, how many candidates it accounted for, and how many
    /// of those were dropped outright.
    #[cfg(any(test, feature = "devnet"))]
    fn build_next(
        &mut self,
        anchor: &Anchor,
        time: u64,
        height: u64,
        candidates: &[(TxID, Vec<u8>)],
    ) -> Result<(Block, usize, usize), NodeError> {
        let build = |chain: &mut Blockchain, transactions| {
            chain.build_block_on_core(
                anchor.core_height,
                anchor.core_hash,
                time,
                anchor.mint.clone(),
                transactions,
            )
        };
        let mut take = candidates.len();
        loop {
            let transactions = self.decode_batch(&candidates[..take])?;
            match build(&mut self.chain, transactions) {
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
                    let block = build(&mut self.chain, Vec::new())?;
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

/// Where the next block is minted: a core block, and the proof of every
/// core block between it and the tip's.
#[cfg(any(test, feature = "devnet"))]
struct Anchor {
    core_height: u64,
    core_hash: [u8; 32],
    mint: MintProof,
}

/// The predicate payments to a receiving or tracking address are locked
/// by: `Predicate::opaque(S)`, the rule `flamepayments` builds outputs with.
fn address_predicate(text: &str, network: Network) -> Result<[u8; 32], NodeError> {
    predicate_of(text, network).map_err(|error| {
        // `flamekd` reports a foreign network as a bad encoding; an address
        // that decodes for the other network is well formed, just not ours.
        let other = match network {
            Network::Mainnet => Network::Testnet,
            Network::Testnet => Network::Mainnet,
        };
        match predicate_of(text, other) {
            Ok(_) => NodeError::WrongNetwork {
                address: other,
                node: network,
            },
            Err(_) => NodeError::Address(error),
        }
    })
}

/// [`address_predicate`] for one network, with `flamekd`'s own error.
pub(crate) fn predicate_of(text: &str, network: Network) -> Result<[u8; 32], flamekd::Error> {
    // A receiving address is a tracking address with an encryption key
    // appended, so length alone says which one was meant, and the error
    // names the right one.
    let spending = if text.len() > TRACKING_ADDRESS_CHARS {
        *ReceivingAddress::from_bech32(text, network)?.spending_key()
    } else {
        *TrackingAddress::from_bech32(text, network)?.spending_key()
    };
    Ok(Predicate::opaque(spending.compress()).to_point().to_bytes())
}

/// The length of a tracking address on either network.
const TRACKING_ADDRESS_CHARS: usize = 60;

/// An output's note: the entry right after it in its log, when that entry
/// is `Data`, byte for byte.
fn note_after(entries: &[TxEntry], index: usize) -> Option<Vec<u8>> {
    match entries.get(index + 1) {
        Some(TxEntry::Data(note)) => Some(note.clone()),
        _ => None,
    }
}

fn page_limit(limit: u32) -> Result<usize, NodeError> {
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(NodeError::Limit {
            got: limit as usize,
            max: MAX_PAGE_SIZE as usize,
        });
    }
    Ok(limit as usize)
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
        // Its place among the block's executions, the order `staged`, and
        // so `TxIndex::at_height`, keeps.
        let position = staged.len() as u32;

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
        let locktime = match entries.first() {
            Some(TxEntry::Header(header)) => header.locktime,
            _ => 0,
        };
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
                            index: position,
                            txid,
                            predicate: contract.predicate.to_point().to_bytes(),
                            contract: contract_bytes(contract)?,
                            note: note_after(entries, index),
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
            (ExecutionKind::Mint, None, None) => ExecutionData::Mint,
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
            locktime,
        };

        // 4. The log itself, for anyone who asks what this transaction did.
        staged.push((
            txid,
            TxRecord {
                height,
                block: block_id,
                log: log_bytes(log)?,
                summary,
                received_at: None,
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
    for (index, (txid, record, changes)) in staged.into_iter().enumerate() {
        for change in changes {
            match change {
                UtxoEffect::Input(id) => {
                    utxos.remove(&id);
                    outputs.spend(
                        id,
                        SpendRecord {
                            height,
                            index: index as u32,
                            txid,
                        },
                    );
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
        .filter(|r| matches!(r.kind, ExecutionKind::Internal | ExecutionKind::InternalFailed))
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
            core_height: header.core_height,
            time: header.time,
            parent: BlockId(header.parent.into_bytes()),
            witness_root: header.witness_root,
            mint_root: header.mint_root,
            effects_root: header.effects_root,
            state: StateCommitment {
                contracts: header.state.contracts.0,
                actors: header.state.actors,
                available_storage_units: header.state.available_storage_units,
            },
        },
        // Genesis was minted by no one.
        time: (header.height != 0).then_some(header.time),
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
    /// A requested actor, block, or confirmed execution is absent.
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
    /// A query argument is not a bounded, data-only Value envelope.
    #[error("invalid actor query argument: {0}")]
    QueryDecode(CellError),
    /// Query execution gas must be positive and fit the node's bound.
    #[error("actor query gas must be 1..={MAX_QUERY_GAS}, got {0}")]
    QueryGasLimit(u64),
    /// A history cursor's block is no longer in the chain.
    #[error("the block at height {height} that the cursor stands on is no longer in the chain")]
    StaleCursor { height: u64 },
    /// A history cursor this node did not write, or wrote for other
    /// addresses.
    #[error("invalid cursor: {0}")]
    Cursor(String),
    /// A lookup's pinned block is no longer the current tip.
    #[error("the expected query block is not the current tip")]
    StaleBlock,
    /// A getter failed without committing effects.
    #[error("actor query failed: {0}")]
    Query(VMError),
    /// An address that does not decode for this node's network.
    #[error("invalid address: {0}")]
    Address(flamekd::Error),
    /// A well-formed address for the other network.
    #[error("invalid address: it is a {address:?} address, and this node is on {node:?}")]
    WrongNetwork { address: Network, node: Network },
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
    /// The core indexer.
    #[error("core indexer: {0}")]
    Indexer(#[from] CoreError),
    /// The filesystem.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
