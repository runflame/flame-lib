//! The overlay chain: real headers, injected transactions, contents fixed per block hash.
//!
//! Every change is a [`Record`] appended to a log before it is applied, and the state is
//! rebuilt by replaying the log. A block's contents are written into its `Connect` record
//! once, so later code changes or restarts cannot rewrite what clients have already read.

mod wallet;

pub use wallet::{Coin, FundError, Funded};

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
};

use corepc_client::bitcoin::{
    Amount, BlockHash, OutPoint, ScriptBuf, Transaction, TxOut, Txid, WScriptHash, absolute, block,
    consensus::encode, hashes::Hash, script, transaction,
};
use serde::{Deserialize, Serialize};

use crate::headers::Header;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub transaction: Transaction,
    /// Aligned with `transaction.input`; `None` only for the coinbase.
    pub prevouts: Vec<Option<TxOut>>,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub header: Header,
    /// The injected transactions, without the coinbase.
    pub injected: Vec<Entry>,
}

impl Block {
    pub fn hash(&self) -> BlockHash {
        self.header.hash()
    }

    /// The coinbase followed by the injected transactions.
    pub fn entries(&self) -> Vec<Entry> {
        let coinbase = Entry {
            transaction: coinbase(&self.header),
            prevouts: vec![None],
        };
        std::iter::once(coinbase)
            .chain(self.injected.iter().cloned())
            .collect()
    }
}

/// A stand-in coinbase, unique per block: it commits to the height (BIP 34) and the hash.
fn coinbase(header: &Header) -> Transaction {
    let script_sig = script::Builder::new()
        .push_int(header.height as i64)
        .push_slice(header.hash().to_byte_array())
        .into_script();
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![transaction::TxIn {
            previous_output: OutPoint::null(),
            script_sig,
            sequence: transaction::Sequence::MAX,
            witness: Default::default(),
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return([]),
        }],
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("log I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("log line {line} is invalid: {reason}")]
    Corrupt { line: usize, reason: String },
    #[error("{0}")]
    Invalid(String),
}

/// Why a submitted transaction was refused, with Bitcoin Core's error code.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("{0}")]
    Rejected(&'static str, i32),
    #[error(transparent)]
    State(#[from] StateError),
}

impl SubmitError {
    pub fn code(&self) -> i32 {
        match self {
            Self::Rejected(_, code) => *code,
            Self::State(_) => -1,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    /// The first record: the height of the first block the overlay serves.
    Start {
        height: u64,
    },
    Submit {
        transaction: String,
    },
    /// A block connected for the first time, with its contents.
    Connect {
        height: u64,
        header: String,
        injected: Vec<RecordEntry>,
    },
    /// A block connected again after a reorg; its contents are those of its `Connect`.
    Reconnect {
        hash: BlockHash,
    },
    Disconnect {
        hash: BlockHash,
    },
    /// A watch-only wallet, created empty.
    CreateWallet {
        name: String,
    },
    /// A script the wallet watches.
    Watch {
        wallet: String,
        script: String,
    },
    /// A made-up confirmed coin paying `script`, so a watching wallet never runs dry.
    Mint {
        script: String,
        sats: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordEntry {
    transaction: String,
    prevouts: Vec<RecordOutput>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordOutput {
    sats: u64,
    script: String,
}

pub struct State {
    start_height: Option<u64>,
    /// Canonical chain; `chain[i]` is at `start_height + i`.
    chain: Vec<BlockHash>,
    /// Every block ever connected, on any branch.
    blocks: HashMap<BlockHash, Block>,
    queue: Vec<Transaction>,
    /// Outputs of every injected transaction ever connected and of minted coins, for prevouts.
    outputs: HashMap<OutPoint, TxOut>,
    /// Watched scripts per wallet name.
    wallets: BTreeMap<String, Vec<ScriptBuf>>,
    /// Minted coins, in order; the index makes each outpoint unique.
    minted: Vec<OutPoint>,
    log: Option<File>,
}

impl State {
    pub fn in_memory() -> Self {
        Self {
            start_height: None,
            chain: Vec::new(),
            blocks: HashMap::new(),
            queue: Vec::new(),
            outputs: HashMap::new(),
            wallets: BTreeMap::new(),
            minted: Vec::new(),
            log: None,
        }
    }

    /// Replays the log at `path`, creating it if missing, and appends to it from then on.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let mut state = Self::in_memory();
        if path.exists() {
            for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
                let line = line?;
                let corrupt = |reason: String| StateError::Corrupt {
                    line: index + 1,
                    reason,
                };
                let record = serde_json::from_str(&line).map_err(|e| corrupt(e.to_string()))?;
                state.apply(record).map_err(corrupt)?;
            }
        }
        state.log = Some(OpenOptions::new().create(true).append(true).open(path)?);
        Ok(state)
    }

    fn commit(&mut self, record: Record) -> Result<(), StateError> {
        if let Some(log) = &mut self.log {
            let mut line = serde_json::to_string(&record).expect("records serialize");
            line.push('\n');
            log.write_all(line.as_bytes())?;
            log.sync_data()?;
        }
        self.apply(record)
            .expect("live records are checked before they are committed");
        Ok(())
    }

    fn apply(&mut self, record: Record) -> Result<(), String> {
        match record {
            Record::Start { height } => {
                if self.start_height.replace(height).is_some() {
                    return Err("second start record".into());
                }
            }
            Record::Submit { transaction } => {
                let transaction: Transaction =
                    encode::deserialize_hex(&transaction).map_err(|e| e.to_string())?;
                self.drop_conflicts(&transaction);
                self.queue.push(transaction);
            }
            Record::Connect {
                height,
                header,
                injected,
            } => {
                let header = Header {
                    height,
                    header: encode::deserialize_hex::<block::Header>(&header)
                        .map_err(|e| e.to_string())?,
                };
                let injected = injected
                    .into_iter()
                    .map(RecordEntry::decode)
                    .collect::<Result<Vec<_>, _>>()?;
                let block = Block { header, injected };
                if self.blocks.contains_key(&block.hash()) {
                    return Err(format!("block {} connected twice", block.hash()));
                }
                for entry in &block.injected {
                    let txid = entry.transaction.compute_txid();
                    for (vout, output) in entry.transaction.output.iter().enumerate() {
                        self.outputs
                            .insert(OutPoint::new(txid, vout as u32), output.clone());
                    }
                }
                let hash = block.hash();
                self.blocks.insert(hash, block);
                self.push(hash)?;
            }
            Record::Reconnect { hash } => self.push(hash)?,
            Record::Disconnect { hash } => {
                if self.chain.last() != Some(&hash) {
                    return Err(format!("disconnect of {hash}, which is not the tip"));
                }
                self.chain.pop();
                // Like Core's mempool: the block's transactions return, conflicts are dropped.
                let returned: Vec<_> = self.blocks[&hash]
                    .injected
                    .iter()
                    .map(|entry| entry.transaction.clone())
                    .collect();
                for transaction in &returned {
                    self.drop_conflicts(transaction);
                }
                self.queue.splice(0..0, returned);
            }
            Record::CreateWallet { name } => {
                if self.wallets.insert(name.clone(), Vec::new()).is_some() {
                    return Err(format!("wallet {name} created twice"));
                }
            }
            Record::Watch { wallet, script } => {
                let script = ScriptBuf::from_hex(&script).map_err(|e| e.to_string())?;
                let watched = self
                    .wallets
                    .get_mut(&wallet)
                    .ok_or(format!("unknown wallet {wallet}"))?;
                if !watched.contains(&script) {
                    watched.push(script);
                }
            }
            Record::Mint { script, sats } => {
                let script_pubkey = ScriptBuf::from_hex(&script).map_err(|e| e.to_string())?;
                let outpoint = wallet::minted_outpoint(self.minted.len());
                self.outputs.insert(
                    outpoint,
                    TxOut {
                        value: Amount::from_sat(sats),
                        script_pubkey,
                    },
                );
                self.minted.push(outpoint);
            }
        }
        Ok(())
    }

    fn push(&mut self, hash: BlockHash) -> Result<(), String> {
        let block = self
            .blocks
            .get(&hash)
            .ok_or(format!("unknown block {hash}"))?;
        let start = self.start_height.ok_or("block before the start record")?;
        let expected_parent = self.chain.last().copied();
        if block.header.height != start + self.chain.len() as u64
            || expected_parent.is_some_and(|parent| block.header.header.prev_blockhash != parent)
        {
            return Err(format!("block {hash} does not extend the tip"));
        }
        let included: Vec<_> = block
            .injected
            .iter()
            .map(|entry| entry.transaction.clone())
            .collect();
        for transaction in &included {
            self.drop_conflicts(transaction);
        }
        self.chain.push(hash);
        Ok(())
    }

    /// Removes queued transactions that are `transaction` or spend any of its inputs.
    fn drop_conflicts(&mut self, transaction: &Transaction) {
        let txid = transaction.compute_txid();
        let spent: HashSet<_> = inputs(transaction).collect();
        self.queue.retain(|queued| {
            queued.compute_txid() != txid && !inputs(queued).any(|input| spent.contains(&input))
        });
    }

    pub fn start_height(&self) -> Option<u64> {
        self.start_height
    }

    pub fn tip(&self) -> Option<&Block> {
        self.chain.last().map(|hash| &self.blocks[hash])
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn block(&self, hash: &BlockHash) -> Option<&Block> {
        self.blocks.get(hash)
    }

    pub fn hash_at(&self, height: u64) -> Option<BlockHash> {
        let index = height.checked_sub(self.start_height?)?;
        self.chain.get(usize::try_from(index).ok()?).copied()
    }

    pub fn is_canonical(&self, header: &Header) -> bool {
        self.hash_at(header.height) == Some(header.hash())
    }

    pub fn start(&mut self, height: u64) -> Result<(), StateError> {
        if self.start_height.is_some() {
            return Err(StateError::Invalid(
                "the overlay has already started".into(),
            ));
        }
        self.commit(Record::Start { height })
    }

    /// Makes `header` the new tip. A block seen before keeps its contents; a new one takes
    /// the whole queue.
    pub fn connect(&mut self, header: Header) -> Result<(), StateError> {
        let start = self
            .start_height
            .ok_or_else(|| StateError::Invalid("connect before start".into()))?;
        if header.height != start + self.chain.len() as u64
            || self
                .chain
                .last()
                .is_some_and(|tip| header.header.prev_blockhash != *tip)
        {
            return Err(StateError::Invalid(format!(
                "block {} at {} does not extend the tip",
                header.hash(),
                header.height
            )));
        }
        if self.blocks.contains_key(&header.hash()) {
            return self.commit(Record::Reconnect {
                hash: header.hash(),
            });
        }
        let mut outputs = HashMap::new();
        let injected: Vec<_> = self
            .queue
            .iter()
            .map(|transaction| {
                let prevouts = transaction
                    .input
                    .iter()
                    .map(|input| {
                        let known = outputs.get(&input.previous_output);
                        let known = known.or_else(|| self.outputs.get(&input.previous_output));
                        Some(known.cloned().unwrap_or_else(|| made_up_prevout(input)))
                    })
                    .collect();
                let txid = transaction.compute_txid();
                for (vout, output) in transaction.output.iter().enumerate() {
                    outputs.insert(OutPoint::new(txid, vout as u32), output.clone());
                }
                RecordEntry::encode(&Entry {
                    transaction: transaction.clone(),
                    prevouts,
                })
            })
            .collect();
        self.commit(Record::Connect {
            height: header.height,
            header: encode::serialize_hex(&header.header),
            injected,
        })
    }

    pub fn disconnect_tip(&mut self) -> Result<(), StateError> {
        let hash = *self
            .chain
            .last()
            .ok_or_else(|| StateError::Invalid("disconnect without a tip".into()))?;
        self.commit(Record::Disconnect { hash })
    }

    /// Queues `transaction` for the next new block, replacing queued ones it conflicts with.
    pub fn submit(
        &mut self,
        transaction: Transaction,
        max_burn: Amount,
    ) -> Result<Txid, SubmitError> {
        let txid = transaction.compute_txid();
        if transaction.is_coinbase() || transaction.input.is_empty() {
            return Err(SubmitError::Rejected("bad-txns-vin-empty", -26));
        }
        let burned: Amount = transaction
            .output
            .iter()
            .filter(|output| output.script_pubkey.is_op_return())
            .map(|output| output.value)
            .sum();
        if burned > max_burn {
            return Err(SubmitError::Rejected(
                "Unspendable output exceeds maximum configured by user (maxburnamount)",
                -25,
            ));
        }
        if self
            .queue
            .iter()
            .any(|queued| queued.compute_txid() == txid)
        {
            return Ok(txid);
        }
        let spent: HashSet<_> = inputs(&transaction).collect();
        for hash in &self.chain {
            for entry in &self.blocks[hash].injected {
                if entry.transaction.compute_txid() == txid {
                    return Err(SubmitError::Rejected(
                        "Transaction already in block chain",
                        -27,
                    ));
                }
                if inputs(&entry.transaction).any(|input| spent.contains(&input)) {
                    return Err(SubmitError::Rejected("bad-txns-inputs-missingorspent", -25));
                }
            }
        }
        self.commit(Record::Submit {
            transaction: encode::serialize_hex(&transaction),
        })?;
        Ok(txid)
    }
}

fn inputs(transaction: &Transaction) -> impl Iterator<Item = OutPoint> + '_ {
    transaction.input.iter().map(|input| input.previous_output)
}

/// The output an input spends, when the overlay never saw it: a P2WSH of the witness script
/// for a P2WSH spend (all an endorsement check needs), else an empty script that matches
/// nothing. Values are unknown and set to zero.
fn made_up_prevout(input: &transaction::TxIn) -> TxOut {
    let script_pubkey = match input.witness.len() {
        0 | 1 => ScriptBuf::new(),
        _ => {
            let witness_script = script::Script::from_bytes(input.witness.last().unwrap());
            ScriptBuf::new_p2wsh(&WScriptHash::hash(witness_script.as_bytes()))
        }
    };
    TxOut {
        value: Amount::ZERO,
        script_pubkey,
    }
}

impl RecordEntry {
    fn encode(entry: &Entry) -> Self {
        Self {
            transaction: encode::serialize_hex(&entry.transaction),
            prevouts: entry
                .prevouts
                .iter()
                .map(|prevout| {
                    let prevout = prevout.as_ref().expect("injected inputs have prevouts");
                    RecordOutput {
                        sats: prevout.value.to_sat(),
                        script: prevout.script_pubkey.to_hex_string(),
                    }
                })
                .collect(),
        }
    }

    fn decode(self) -> Result<Entry, String> {
        let transaction: Transaction =
            encode::deserialize_hex(&self.transaction).map_err(|e| e.to_string())?;
        let prevouts = self
            .prevouts
            .into_iter()
            .map(|output| {
                Ok(Some(TxOut {
                    value: Amount::from_sat(output.sats),
                    script_pubkey: ScriptBuf::from_hex(&output.script)
                        .map_err(|e| e.to_string())?,
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if prevouts.len() != transaction.input.len() {
            return Err("prevouts do not match inputs".into());
        }
        Ok(Entry {
            transaction,
            prevouts,
        })
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
