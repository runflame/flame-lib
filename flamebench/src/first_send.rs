//! The first send in a fresh process.
//!
//! A process builds the prover's generators the first time it proves, so a
//! wallet that runs one command per process pays that on every send. The
//! `send` bench measures it by running the `first-send` binary many times,
//! each in a new process pinned to the benchmarks' CPU.
//!
//! The rule the child keeps: **it constructs nothing that runs the prover,
//! or builds generators, before its timed send.** Rebuilding a fixture means
//! proving, so the parent hands the child what a wallet would have instead,
//! in a [`Scratch`] file: each input's published contract as a cell
//! envelope, the note bytes that followed it, and where its key sits; and
//! each output's owner and amount. The child decodes the contracts,
//! re-derives the keys from the fixed seeds, and opens the notes. None of
//! that needs the prover or the verifier.
//!
//! Then it times two sends, prepare to package, and prints a [`Report`].

use std::fs;
use std::path::Path;
use std::time::Instant;

use flamepayments::open_note;
use flamevm::{CellDecode, CellEncode, CellEnvelope, CellError, Contract, FLAME_FLAVOR};
use serde::{Deserialize, Serialize};

use crate::alloc::Counting;
use crate::fixtures::{self, KeyPath, Payment, Spend, Transfer};

/// The largest envelope the child decodes a contract from, as `flamed`
/// bounds it.
const MAX_CONTRACT_BYTES: usize = 64 * 1024;

/// What the parent hands the child for one shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scratch {
    /// The shape's id, for messages.
    pub shape: String,
    pub inputs: Vec<ScratchInput>,
    pub outputs: Vec<ScratchOutput>,
}

/// One input: the contract as the chain published it, its note, and where
/// its owner's key sits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScratchInput {
    /// `to_envelope().encode()`, hex.
    pub contract: String,
    /// The `Data` entry that followed the output, hex.
    pub note: String,
    pub path: KeyPath,
}

/// One output: whom it pays, and how much.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScratchOutput {
    pub path: KeyPath,
    pub qty: u64,
}

/// What the child prints: one JSON line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The first send, which builds the prover's generators.
    pub first_ns: u64,
    /// The second send, right after, in the same process.
    pub second_ns: u64,
    /// Peak heap of the first send beyond what was in use before it.
    pub first_peak_bytes: u64,
    /// The same for the second.
    pub second_peak_bytes: u64,
    /// The length of the packaged transaction, both times.
    pub bytes: usize,
}

impl Scratch {
    /// What the child needs to send `transfer` again.
    pub fn new(shape: &str, transfer: &Transfer) -> Result<Scratch, CellError> {
        let inputs = transfer
            .inputs
            .iter()
            .map(|spend| {
                Ok(ScratchInput {
                    contract: hex::encode(contract_bytes(&spend.contract)?),
                    note: hex::encode(&spend.note),
                    path: spend.path,
                })
            })
            .collect::<Result<_, CellError>>()?;
        let outputs = transfer
            .outputs
            .iter()
            .map(|payment| ScratchOutput {
                path: payment.path,
                qty: payment.output.qty,
            })
            .collect();
        Ok(Scratch {
            shape: shape.to_owned(),
            inputs,
            outputs,
        })
    }

    /// The transfer back, as a wallet would hold it: every contract
    /// decoded, every key derived, every note opened.
    pub fn transfer(&self) -> Result<Transfer, String> {
        let inputs = self
            .inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let context = |what: &str| format!("{} input {index}: {what}", self.shape);
                let contract = hex::decode(&input.contract)
                    .map_err(|_| context("the contract is not hex"))
                    .and_then(|bytes| {
                        contract_from_bytes(&bytes)
                            .map_err(|e| context(&format!("the contract does not decode: {e:?}")))
                    })?;
                let note = hex::decode(&input.note).map_err(|_| context("the note is not hex"))?;
                let owner = input.path.owner();
                let received = open_note(&contract, Some(&note), &owner.address, &owner.view_key)
                    .map_err(|e| context(&format!("the note does not open: {e}")))?;
                Ok(Spend {
                    contract,
                    note,
                    opening: received.opening,
                    key: input.path.spending_key(),
                    path: input.path,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let outputs = self
            .outputs
            .iter()
            .map(|output| Payment {
                output: flamepayments::OutputSpec {
                    address: output.path.address(),
                    qty: output.qty,
                    flv: FLAME_FLAVOR,
                    memo: Vec::new(),
                },
                path: output.path,
            })
            .collect();
        Ok(Transfer { inputs, outputs })
    }
}

/// The child's whole run: read `scratch`, rebuild the transfer, check its
/// inputs, then time two sends and measure their heap with `alloc`.
pub fn run(scratch: &Path, alloc: &Counting) -> Result<Report, String> {
    let text = fs::read_to_string(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let scratch: Scratch =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let transfer = scratch.transfer()?;
    // The openings check out before anything is timed. Building an
    // `InputSpec` rebuilds a contract, and runs no prover.
    transfer
        .prepare()
        .map_err(|e| format!("{}: an input does not rebuild: {e}", scratch.shape))?;

    let mut rng = fixtures::rng(fixtures::SEND_RNG_SEED);
    let mut send = || {
        alloc.start();
        let started = Instant::now();
        let bytes = transfer.send(&mut rng);
        let ns = started.elapsed().as_nanos() as u64;
        let peak = alloc.stop();
        bytes
            .map(|bytes| (ns, peak, bytes.len()))
            .map_err(|e| format!("{}: the send failed: {e:?}", scratch.shape))
    };
    let (first_ns, first_peak_bytes, bytes) = send()?;
    let (second_ns, second_peak_bytes, second_bytes) = send()?;
    if second_bytes != bytes {
        return Err(format!(
            "{}: the two sends packaged {bytes} and {second_bytes} bytes",
            scratch.shape
        ));
    }
    Ok(Report {
        first_ns,
        second_ns,
        first_peak_bytes,
        second_peak_bytes,
        bytes,
    })
}

/// A contract as the chain publishes it, as `flamed`'s `contract_bytes`
/// encodes it.
pub fn contract_bytes(contract: &Contract) -> Result<Vec<u8>, CellError> {
    Ok(contract.to_envelope()?.encode())
}

/// A contract back from those bytes, as `flamed`'s `contract_from_bytes`
/// decodes it.
pub fn contract_from_bytes(bytes: &[u8]) -> Result<Contract, CellError> {
    let mut gas = (bytes.len() as u64).saturating_mul(4);
    let mut envelope = CellEnvelope::decode(bytes, MAX_CONTRACT_BYTES, &mut gas)?;
    let root = envelope
        .cells()
        .get(&envelope.root())
        .ok_or(CellError::MissingCell(envelope.root()))?;
    let contract = Contract::from_cell(&root, &mut envelope)?;
    if contract_bytes(&contract)? != bytes {
        return Err(CellError::InvalidFormat);
    }
    Ok(contract)
}
