//! The transactions the benchmarks measure, built once and outside every
//! timed loop.
//!
//! Two entry points, because their costs differ by an order of magnitude:
//! [`shapes`] builds the four shapes from one funding transaction, and
//! [`pool`] builds the 256-transaction verifier-scaling pool from twenty.
//! Tests call `shapes` only.
//!
//! Every shape spends confidential inputs the way a wallet does: a clear
//! allocation funds a 1→13 transfer to the sender's own addresses, the
//! sender opens each output's note, and the openings become
//! [`InputSpec::confidential`] inputs. `verify` does not check Utreexo
//! membership, so every input carries [`Proof::Transient`].
//!
//! Each shape also keeps its [`Transfer`]: the published inputs, their
//! openings and keys, and the outputs. A send rebuilds from it as often as
//! a benchmark likes, without touching the funding.

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamechain::{BlockTx, ChainParams, SPARKS_PER_FLAME};
use flamekd::{util, Network, ReceivingAddress};
use flamepayments::{
    block_tx, build_transfer, open_note, outputs_with_notes, sign, Account, BuilderError,
    InputSpec, Opening, OutputSpec,
};
use flamevm::{
    Anchor, CellEncode, CellError, ClearToken, Contract, ContractID, ExternalTx, Limits, Scalar,
    TxHeader, TxID, TxLog, TxMetrics, UnsignedTx, Value, FLAME_FLAVOR,
};
use merlin::Transcript;
use musig::{Multisignature, MusigError, Signature, VerificationKey};
use rand::rngs::StdRng;
use rand::SeedableRng;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

/// The sender's seed.
const SENDER_SEED: [u8; 64] = [0x61; 64];
/// The recipient's seed.
const RECIPIENT_SEED: [u8; 64] = [0x62; 64];

/// [`KeyPath::account`] of the sender, who funds and spends every shape.
pub const SENDER: u8 = 0;
/// [`KeyPath::account`] of the recipient, whom every payment pays.
pub const RECIPIENT: u8 = 1;

/// Each clear allocation.
const ALLOCATION: u64 = 1_000 * SPARKS_PER_FLAME;
/// What every payment to the recipient carries.
const PAYMENT: u64 = SPARKS_PER_FLAME;
/// The fee every transaction pays.
pub const FEE: u64 = 1_000;

/// Outputs of one funding transaction, and so inputs it yields.
const FUNDING_OUTPUTS: usize = 13;
/// Funding transactions behind the throughput pool.
const POOL_FUNDING: usize = 20;
/// Transactions in the verifier-scaling pool.
pub const POOL_SIZE: usize = 256;

/// The seed of every generator here.
const RNG_SEED: u64 = 0x666c_616d_6562_656e;

/// The seed of the generator every benchmarked send draws its `r` from.
pub const SEND_RNG_SEED: u64 = 0x666c_616d_6573_6e64;

/// A generator seeded with `seed`, as every send here uses.
pub fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

/// The transaction header every fixture uses.
pub fn header() -> TxHeader {
    TxHeader {
        version: 1,
        locktime: 0,
    }
}

/// The gas budget every fixture is built and verified with, as the
/// `flamepayments` tests use.
pub fn limits() -> Limits {
    Limits { gas: 10_000_000 }
}

/// The chain parameters `flamed` decodes a submitted transaction with.
pub fn chain_params() -> ChainParams {
    ChainParams::default()
}

/// One transaction shape: inputs → outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeSpec {
    /// The benchmark id: ASCII, safe as a directory name.
    pub id: &'static str,
    /// The label in tables and charts.
    pub label: &'static str,
    pub inputs: usize,
    pub outputs: usize,
}

/// The four shapes, in the order every table lists them. 1→13 is the most
/// outputs one transaction can prove; [`OVER_CAPACITY`] is one more.
pub const SHAPES: [ShapeSpec; 4] = [
    ShapeSpec {
        id: "1to2",
        label: "1 → 2",
        inputs: 1,
        outputs: 2,
    },
    ShapeSpec {
        id: "2to2",
        label: "2 → 2",
        inputs: 2,
        outputs: 2,
    },
    ShapeSpec {
        id: "4to4",
        label: "4 → 4",
        inputs: 4,
        outputs: 4,
    },
    ShapeSpec {
        id: "1to13",
        label: "1 → 13",
        inputs: 1,
        outputs: 13,
    },
];

/// The transfer one output past the proof's capacity. It never proves.
pub const OVER_CAPACITY: ShapeSpec = ShapeSpec {
    id: "1to14",
    label: "1 → 14",
    inputs: 1,
    outputs: 14,
};

/// How the decode group decodes a shape's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeMethod {
    /// `BlockTx::from_bytes_bounded`, as `flamed` decodes a submitted
    /// transaction.
    BlockTx,
    /// `ExternalTx::from_bytes_bounded` on the transaction's envelope, the
    /// fallback for a `BlockTx` that does not round-trip.
    Envelope,
}

/// Who an output pays: what its owner opens the note with.
#[derive(Clone, Copy)]
pub struct Owner {
    pub address: ReceivingAddress,
    pub view_key: DalekScalar,
}

/// Where a key sits: which fixture account, and `(branch, n)` in it. All a
/// fresh process needs to re-derive the keys from the fixed seeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPath {
    /// [`SENDER`] or [`RECIPIENT`].
    pub account: u8,
    pub branch: u32,
    pub n: u32,
}

impl KeyPath {
    /// The account behind the path.
    pub fn account(&self) -> Account {
        account(self.account)
    }

    /// The address at the path.
    pub fn address(&self) -> ReceivingAddress {
        self.account()
            .address_at(self.branch, self.n)
            .expect("a fixture path derives an address")
    }

    /// The spending key at the path.
    pub fn spending_key(&self) -> DalekScalar {
        self.account()
            .spending_key_at(self.branch, self.n)
            .expect("a fixture path derives a spending key")
    }

    /// Who the path is, as the owner of an output.
    pub fn owner(&self) -> Owner {
        owner(&self.account(), self.branch, self.n)
    }
}

/// The fixture account `index`: [`SENDER`] or [`RECIPIENT`].
pub fn account(index: u8) -> Account {
    let seed = match index {
        SENDER => &SENDER_SEED,
        RECIPIENT => &RECIPIENT_SEED,
        other => panic!("no fixture account {other}"),
    };
    Account::from_seed(seed, Network::Testnet).expect("a fixture seed derives an account")
}

/// One confidential input as its owner holds it: the published contract,
/// the note that followed it, the opening that note gave, and the key.
#[derive(Clone)]
pub struct Spend {
    pub contract: Contract,
    pub note: Vec<u8>,
    pub opening: Opening,
    pub key: DalekScalar,
    pub path: KeyPath,
}

/// One output of a transfer, and who it pays.
#[derive(Clone)]
pub struct Payment {
    pub output: OutputSpec,
    pub path: KeyPath,
}

/// Everything a send starts from: the unsigned inputs and the outputs.
#[derive(Clone)]
pub struct Transfer {
    pub inputs: Vec<Spend>,
    pub outputs: Vec<Payment>,
}

/// A send that failed, and in which step.
#[derive(Debug)]
pub enum SendError {
    Prepare(BuilderError),
    Build(BuilderError),
    Sign(MusigError),
    Package(CellError),
}

impl Transfer {
    /// `InputSpec::confidential` for every input: `send/prepare`.
    pub fn prepare(&self) -> Result<Vec<InputSpec>, BuilderError> {
        self.inputs
            .iter()
            .map(|spend| {
                InputSpec::confidential(
                    &spend.contract,
                    &spend.opening,
                    Proof::Transient,
                    spend.key,
                )
            })
            .collect()
    }

    /// The output specs, in the order they were asked for.
    pub fn output_specs(&self) -> Vec<OutputSpec> {
        self.outputs
            .iter()
            .map(|payment| payment.output.clone())
            .collect()
    }

    /// The inputs' signing keys, in input order.
    pub fn keys(&self) -> Vec<DalekScalar> {
        self.inputs.iter().map(|spend| spend.key).collect()
    }

    /// `build_transfer`, sealing every note and proving: `send/build`.
    pub fn build<R: RngCore + CryptoRng>(
        &self,
        inputs: &[InputSpec],
        outputs: &[OutputSpec],
        rng: &mut R,
    ) -> Result<UnsignedTx, BuilderError> {
        build_transfer(inputs, outputs, FEE, header(), limits(), rng)
    }

    /// One whole send: prepare, build, sign and package. Returns the bytes
    /// a wallet submits.
    pub fn send<R: RngCore + CryptoRng>(&self, rng: &mut R) -> Result<Vec<u8>, SendError> {
        let inputs = self.prepare().map_err(SendError::Prepare)?;
        let unsigned = self
            .build(&inputs, &self.output_specs(), rng)
            .map_err(SendError::Build)?;
        let tx = sign(unsigned, &self.keys()).map_err(SendError::Sign)?;
        package(tx, self.inputs.len()).map_err(SendError::Package)
    }
}

/// `block_tx` and its encoding: the bytes a wallet submits, one transient
/// proof per input. `send/package`.
pub fn package(tx: ExternalTx, inputs: usize) -> Result<Vec<u8>, CellError> {
    block_tx(
        tx,
        limits(),
        (0..inputs).map(|_| Proof::Transient).collect(),
    )
    .to_bytes()
}

/// Everything `flamepayments::sign` computes, without consuming `unsigned`:
/// the signing instructions, the transcript, the aggregate signature, and
/// the txid `UnsignedTx::sign` derives once more when it attaches the
/// signature. Only moving the fields into an `ExternalTx` is left out.
/// `send/sign`.
pub fn signature(
    unsigned: &UnsignedTx,
    keys: &[DalekScalar],
) -> Result<(Signature, TxID), MusigError> {
    let instructions = unsigned.signing_instructions();
    let mut transcript = Transcript::new(b"flamevm.signtx");
    transcript.append_message(b"txid", &instructions.txid.0);
    let items: Vec<_> = instructions
        .items
        .iter()
        .map(|(key, contract)| (VerificationKey::from_compressed(*key), *contract))
        .collect();
    let signature = Signature::sign_multi(keys, items, &mut transcript)?;
    Ok((signature, unsigned.log().txid()))
}

/// A field-by-field copy of `tx`, which is not `Clone`.
pub fn copy_tx(tx: &ExternalTx) -> ExternalTx {
    ExternalTx {
        header: tx.header,
        script: tx.script.clone(),
        signature: tx.signature,
        proof: tx.proof.clone(),
        witnesses: tx.witnesses.clone(),
        txid: tx.txid,
    }
}

/// One built shape and everything the benchmarks read from it.
pub struct Shape {
    pub spec: ShapeSpec,
    /// The signed transaction.
    pub tx: ExternalTx,
    /// Its encoded bytes, as `decode` says.
    pub bytes: Vec<u8>,
    pub decode: DecodeMethod,
    /// From one `verify_with_metrics`.
    pub metrics: TxMetrics,
    /// What the signature signs, captured before signing.
    pub txid: TxID,
    pub items: Vec<(CompressedRistretto, ContractID)>,
    /// What the shape was built from, to build it again. Each output's
    /// owner is its key path's: the builder sorts outputs, so an output is
    /// matched to its owner by predicate, never by position.
    pub transfer: Transfer,
}

impl Shape {
    /// Decodes the shape's bytes the way the decode group does.
    pub fn decode_bytes(&self) -> Result<ExternalTx, CellError> {
        decode(&self.bytes, self.decode)
    }
}

/// The verifier-scaling pool: distinct 1→2 transactions, each spending its
/// own input.
pub struct Pool {
    pub txs: Vec<ExternalTx>,
}

/// Decodes `bytes` with `method`, bounded by the default chain parameters.
pub fn decode(bytes: &[u8], method: DecodeMethod) -> Result<ExternalTx, CellError> {
    let params = chain_params();
    match method {
        DecodeMethod::BlockTx => {
            BlockTx::from_bytes_bounded(bytes, params.version, params.limits).map(|tx| tx.tx)
        }
        DecodeMethod::Envelope => ExternalTx::from_bytes_bounded(
            bytes,
            params.version,
            params.limits.max_transaction_script_bytes,
            params.limits.max_witness_bytes,
        ),
    }
}

/// Builds the four shapes from one funding transaction: 13 inputs, of which
/// 8 are spent.
pub fn shapes() -> Vec<Shape> {
    shapes_and_over_capacity().0
}

/// The four shapes, and the transfer past the proof's capacity: one more
/// input of the same funding, paid out to 13 recipients plus change. That
/// transfer is never built here: building it fails, and that failure is
/// what the benchmark records.
pub fn shapes_and_over_capacity() -> (Vec<Shape>, Transfer) {
    let mut wallet = Wallet::new();
    let mut inputs = wallet.fund(1).into_iter();
    let shapes = SHAPES
        .iter()
        .map(|spec| {
            let spent: Vec<Spend> = inputs.by_ref().take(spec.inputs).collect();
            assert_eq!(spent.len(), spec.inputs, "one funding covers every shape");
            wallet.shape(*spec, spent)
        })
        .collect();
    let spare: Vec<Spend> = inputs.take(OVER_CAPACITY.inputs).collect();
    assert_eq!(spare.len(), OVER_CAPACITY.inputs, "the funding has a spare");
    (shapes, wallet.transfer(OVER_CAPACITY, spare))
}

/// The inputs of one funding transaction: 13 confidential outputs of a
/// 1→13 transfer to the sender's own addresses, as the sender holds them.
/// One transaction proved, nothing else.
pub fn funding() -> Vec<Spend> {
    Wallet::new().fund(1)
}

/// The transfer of shape `spec` that spends `spent`, as the first shape
/// built from a fresh wallet pays: nothing is built or proved.
pub fn transfer(spec: ShapeSpec, spent: Vec<Spend>) -> Transfer {
    Wallet::new().transfer(spec, spent)
}

/// Builds the verifier-scaling pool from twenty funding transactions: 260
/// inputs, of which 256 are spent. Several seconds of proving.
pub fn pool() -> Pool {
    let mut wallet = Wallet::new();
    let inputs = wallet.fund(POOL_FUNDING);
    assert!(inputs.len() >= POOL_SIZE, "the funding covers the pool");
    let txs = inputs
        .into_iter()
        .take(POOL_SIZE)
        .map(|input| wallet.shape(SHAPES[0], vec![input]).tx)
        .collect();
    Pool { txs }
}

/// The two accounts, the generator every `r` comes from, and the next
/// unused address on each branch that pays.
struct Wallet {
    sender: Account,
    recipient: Account,
    rng: StdRng,
    next_funding: u32,
    next_payment: u32,
    next_change: u32,
}

impl Wallet {
    fn new() -> Wallet {
        Wallet {
            sender: account(SENDER),
            recipient: account(RECIPIENT),
            rng: rng(RNG_SEED),
            // RECEIVING/0 holds the clear allocations.
            next_funding: 1,
            next_payment: 0,
            next_change: 0,
        }
    }

    /// `count` clear allocations, each spent by one 1→13 transaction to the
    /// sender's own addresses, whose outputs come back as confidential
    /// inputs.
    fn fund(&mut self, count: usize) -> Vec<Spend> {
        let key = self
            .sender
            .spending_key_at(util::RECEIVING, 0)
            .expect("allocation key");
        let predicate = self
            .sender
            .predicate_at(util::RECEIVING, 0)
            .expect("allocation predicate");
        let mut spendable = Vec::with_capacity(count * FUNDING_OUTPUTS);
        for index in 0..count {
            let mut anchor = [0u8; 32];
            anchor[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
            let allocation = Contract::new(
                predicate.clone(),
                Anchor(anchor),
                Value::ClearToken(ClearToken::new(Scalar::from(ALLOCATION), FLAME_FLAVOR)),
            )
            .expect("a non-negative clear token is portable");
            let input =
                InputSpec::clear(allocation, Proof::Transient, key).expect("clear allocation");

            let share = (ALLOCATION - FEE) / FUNDING_OUTPUTS as u64;
            let mut owners = Vec::with_capacity(FUNDING_OUTPUTS);
            let mut outputs = Vec::with_capacity(FUNDING_OUTPUTS);
            for output in 0..FUNDING_OUTPUTS {
                let n = self.next_funding;
                self.next_funding += 1;
                let qty = if output + 1 == FUNDING_OUTPUTS {
                    ALLOCATION - FEE - share * (FUNDING_OUTPUTS as u64 - 1)
                } else {
                    share
                };
                owners.push((n, owner(&self.sender, util::RECEIVING, n)));
                outputs.push(native_output(owners[output].1.address, qty));
            }

            let unsigned =
                build_transfer(&[input], &outputs, FEE, header(), limits(), &mut self.rng)
                    .expect("build a funding transaction");
            let tx = sign(unsigned, &[key]).expect("sign a funding transaction");
            let bytes = tx.to_envelope().expect("envelope").encode();
            let log = decode(&bytes, DecodeMethod::Envelope)
                .expect("decode a funding transaction")
                .verify(limits())
                .expect("verify a funding transaction");

            for (n, owner) in owners {
                let (contract, note, received) = open_with_note(&log, &owner);
                let key = self
                    .sender
                    .spending_key_at(util::RECEIVING, n)
                    .expect("input key");
                InputSpec::confidential(&contract, &received.opening, Proof::Transient, key)
                    .expect("a confidential input on a funding output");
                spendable.push(Spend {
                    contract,
                    note,
                    opening: received.opening,
                    key,
                    path: KeyPath {
                        account: SENDER,
                        branch: util::RECEIVING,
                        n,
                    },
                });
            }
        }
        spendable
    }

    /// One shape's transfer: `outputs − 1` payments to the recipient, plus
    /// change back to the sender.
    fn transfer(&mut self, spec: ShapeSpec, spent: Vec<Spend>) -> Transfer {
        let total: u64 = spent.iter().map(|spend| spend.opening.qty).sum();
        let payments = spec.outputs as u64 - 1;
        let change = total - payments * PAYMENT - FEE;

        let mut outputs = Vec::with_capacity(spec.outputs);
        for _ in 0..payments {
            let path = KeyPath {
                account: RECIPIENT,
                branch: util::RECEIVING,
                n: self.next_payment,
            };
            self.next_payment += 1;
            outputs.push(Payment {
                output: native_output(
                    self.recipient
                        .address_at(path.branch, path.n)
                        .expect("address"),
                    PAYMENT,
                ),
                path,
            });
        }
        let path = KeyPath {
            account: SENDER,
            branch: util::CHANGE,
            n: self.next_change,
        };
        self.next_change += 1;
        outputs.push(Payment {
            output: native_output(
                self.sender
                    .address_at(path.branch, path.n)
                    .expect("address"),
                change,
            ),
            path,
        });
        Transfer {
            inputs: spent,
            outputs,
        }
    }

    /// One shape, built and signed.
    fn shape(&mut self, spec: ShapeSpec, spent: Vec<Spend>) -> Shape {
        let transfer = self.transfer(spec, spent);
        let inputs = transfer
            .prepare()
            .unwrap_or_else(|error| panic!("prepare the {} shape: {error:?}", spec.label));
        let unsigned = transfer
            .build(&inputs, &transfer.output_specs(), &mut self.rng)
            .unwrap_or_else(|error| panic!("build the {} shape: {error:?}", spec.label));
        let instructions = unsigned.signing_instructions();
        let tx = sign(unsigned, &transfer.keys()).expect("sign a shape");
        let (_, metrics) = tx
            .verify_with_metrics(limits())
            .unwrap_or_else(|error| panic!("verify the {} shape: {error:?}", spec.label));
        let (tx, bytes, decode) = encode(tx, spec.inputs);

        Shape {
            spec,
            tx,
            bytes,
            decode,
            metrics,
            txid: instructions.txid,
            items: instructions.items,
            transfer,
        }
    }
}

/// Encodes a signed transaction as the `BlockTx` a wallet submits, one
/// transient proof per input, and keeps that encoding if it round-trips.
/// Otherwise the transaction's own envelope stands in for it.
fn encode(tx: ExternalTx, inputs: usize) -> (ExternalTx, Vec<u8>, DecodeMethod) {
    let block_tx = BlockTx {
        tx,
        limits: limits(),
        proofs: (0..inputs).map(|_| Proof::Transient).collect(),
    };
    if let Ok(bytes) = block_tx.to_bytes() {
        if decode(&bytes, DecodeMethod::BlockTx).is_ok() {
            return (block_tx.tx, bytes, DecodeMethod::BlockTx);
        }
    }
    let tx = block_tx.tx;
    let bytes = tx.to_envelope().expect("envelope").encode();
    (tx, bytes, DecodeMethod::Envelope)
}

/// The owner of `m/…/branch/n` in `account`.
fn owner(account: &Account, branch: u32, n: u32) -> Owner {
    Owner {
        address: account.address_at(branch, n).expect("address"),
        view_key: account.viewing_key_at(branch, n).expect("viewing key"),
    }
}

/// A native-flavor output with an empty memo.
fn native_output(address: ReceivingAddress, qty: u64) -> OutputSpec {
    OutputSpec {
        address,
        qty,
        flv: FLAME_FLAVOR,
        memo: Vec::new(),
    }
}

/// Finds the output `owner` was paid in `log` and opens its note.
pub fn open(log: &TxLog, owner: &Owner) -> (Contract, flamepayments::ReceivedNote) {
    let (contract, _, received) = open_with_note(log, owner);
    (contract, received)
}

/// Finds the output `owner` was paid in `log`, and returns it with its note
/// bytes and what they open to.
pub fn open_with_note(
    log: &TxLog,
    owner: &Owner,
) -> (Contract, Vec<u8>, flamepayments::ReceivedNote) {
    let (contract, note) = paid_output(log, owner);
    let received = open_note(contract, note, &owner.address, &owner.view_key)
        .expect("the owner opens its note");
    (
        contract.clone(),
        note.expect("every output has a note").to_vec(),
        received,
    )
}

/// The output that pays `owner` in `log`, and its note.
pub fn paid_output<'a>(log: &'a TxLog, owner: &Owner) -> (&'a Contract, Option<&'a [u8]>) {
    let point = owner.address.spending_key().compress();
    outputs_with_notes(log)
        .into_iter()
        .find(|(contract, _)| contract.predicate.to_point() == point)
        .expect("an output pays the owner")
}
