//! Fixtures shared by the two round trips.
//!
//! Every transaction here is built by `flamepayments` and nothing else, and
//! every opening a test spends with comes from a note the node returned. No
//! test assembles a script, so both round trips exercise the same client
//! the wallet will be.

mod actor;
mod block;
mod contract;
mod node;
mod rpc;
mod transactions;

use std::path::Path;

use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamechain::{BlockTx, SPARKS_PER_FLAME};
use flamekd::{util, Network};
use flamepayments::{
    block_tx, build_transfer, open_note, sign, Account, InputSpec, Opening, OutputSpec,
};
use flamevm::{Contract, ContractID, Limits, TxEntry, TxHeader, TxLog, FLAME_FLAVOR};
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::cells::contract_from_bytes;
use crate::config::{ChainParamsFile, GenesisFile, GenesisSpec, NetworkName, NodeConfig};
use crate::node::{Node, ProofStatus};

/// The account the tests call A, which starts with the whole supply.
pub(crate) const SEED_A: [u8; 64] = [0xa1; 64];
/// The account the tests call B.
pub(crate) const SEED_B: [u8; 64] = [0xb2; 64];

/// 1000 Flame, all of the supply this devnet ever has.
pub(crate) const GENESIS_SPARKS: u64 = 1_000 * SPARKS_PER_FLAME;
/// One Flame, in sparks.
pub(crate) const FLAME: u64 = SPARKS_PER_FLAME;
/// The fee every test transfer pays.
pub(crate) const FEE: u64 = 1_000;

pub(crate) fn account(seed: &[u8; 64]) -> Account {
    Account::from_seed(seed, Network::Testnet, 0).expect("a 64-byte seed derives an account")
}

pub(crate) fn header() -> TxHeader {
    TxHeader {
        version: 1,
        locktime: 0,
    }
}

pub(crate) fn limits() -> Limits {
    Limits { gas: 10_000_000 }
}

/// The generator a test draws every `r` from: seeded, so a failing run
/// replays exactly. One per test, threaded through every transfer it
/// builds, because a transfer rebuilt from a re-seeded generator would draw
/// the same `r` again.
pub(crate) fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

/// One native-flavor output to `m/…/branch/n`, with no memo.
pub(crate) fn output(account: &Account, branch: u32, n: u32, qty: u64) -> OutputSpec {
    OutputSpec {
        address: account.address_at(branch, n).expect("address"),
        qty,
        flv: FLAME_FLAVOR,
        memo: Vec::new(),
    }
}

/// The network definition under test: the whole supply under A's first
/// receiving address, written the way an operator writes one.
pub(crate) fn chainparams(a: &Account) -> ChainParamsFile {
    let address = a
        .address_at(util::RECEIVING, 0)
        .expect("first receiving address")
        .to_bech32(Network::Testnet);
    assert!(address.starts_with("tf1"), "a testnet address");
    ChainParamsFile {
        version: 1,
        network: NetworkName::Testnet,
        storage: Default::default(),
        limits: Default::default(),
        genesis: vec![GenesisSpec {
            address: Some(address),
            predicate: None,
            qty_sparks: GENESIS_SPARKS,
        }],
    }
}

/// Writes `genesis.json` into `dir` through the very function
/// `flamed genesis` runs, then reads it back. Hand-assembled JSON would
/// leave that path untested.
pub(crate) fn devnet(dir: &Path, a: &Account) -> (GenesisFile, NodeConfig) {
    let path = dir.join("genesis.json");
    crate::genesis::write(&chainparams(a), &path).expect("derive genesis.json");
    let genesis = GenesisFile::load(&path).expect("read genesis.json back");
    let cfg = NodeConfig {
        data_dir: dir.to_path_buf(),
        genesis: Some(path),
        rpc_bind: "127.0.0.1:0".parse().expect("a literal socket address"),
        block_interval_secs: 15,
        minimum_fee: 0,
    };
    (genesis, cfg)
}

/// The contracts an effect log says a transaction creates.
pub(crate) fn created(log: &TxLog) -> Vec<Contract> {
    log.entries()
        .iter()
        .filter_map(|entry| match entry {
            TxEntry::Output(contract) => Some(contract.clone()),
            _ => None,
        })
        .collect()
}

/// The one contract of `contracts` that pays `m/…/branch/n`. A transfer
/// sorts its outputs, so a contract is found by its predicate, never by its
/// position.
pub(crate) fn paying(contracts: &[Contract], account: &Account, branch: u32, n: u32) -> Contract {
    let point = account
        .predicate_at(branch, n)
        .expect("predicate")
        .to_point();
    let mut found = contracts
        .iter()
        .filter(|contract| contract.predicate.to_point() == point);
    let contract = found.next().expect("a contract pays the address");
    assert!(found.next().is_none(), "one contract pays the address");
    contract.clone()
}

/// Builds, signs and packages one transfer, and hands back the contracts
/// its effect log says it creates — read before signing consumes it.
pub(crate) fn signed_transfer(
    inputs: Vec<InputSpec>,
    outputs: &[OutputSpec],
    fee: u64,
    rng: &mut StdRng,
) -> (BlockTx, Vec<Contract>) {
    let unsigned = build_transfer(&inputs, outputs, fee, header(), limits(), rng).expect("builds");
    let contracts = created(unsigned.log());
    let keys: Vec<DalekScalar> = inputs.iter().map(InputSpec::signing_key).collect();
    let proofs: Vec<Proof> = inputs.iter().map(|input| input.proof().clone()).collect();
    let tx = sign(unsigned, &keys).expect("aggregate signature");
    (block_tx(tx, limits(), proofs), contracts)
}

/// The bytes a packaged transfer is submitted as.
pub(crate) fn submitted(packaged: &BlockTx) -> Vec<u8> {
    packaged.to_bytes().expect("a built transfer encodes")
}

/// The proof the node holds for a contract it believes is unspent.
pub(crate) fn live(node: &Node, id: &ContractID) -> Proof {
    match node.proof(id) {
        ProofStatus::Unspent(proof) => proof,
        other => panic!("expected {} to be unspent, got {other:?}", hex::encode(id)),
    }
}

/// What a recipient learns from the node about the one contract paid to
/// `m/…/branch/n`: the contract as `scan` returned it, and the opening read
/// from the note `scan` returned beside it.
pub(crate) fn received(node: &Node, account: &Account, branch: u32, n: u32) -> (Contract, Opening) {
    let predicate = account.predicate_at(branch, n).expect("predicate");
    let hits = node.scan(&[predicate.to_point().to_bytes()], 0);
    assert_eq!(hits.len(), 1, "one contract pays m/…/{branch}/{n}");
    let contract = contract_from_bytes(&hits[0].bytes.0).expect("canonical contract bytes decode");
    let note = open_note(
        &contract,
        hits[0].note.as_ref().map(|note| note.0.as_slice()),
        &account.address_at(branch, n).expect("address"),
        &account.viewing_key_at(branch, n).expect("viewing key"),
    )
    .expect("the note scan returned opens");
    (contract, note.opening)
}

/// The contract bytes the node published, decoded as a recipient would.
pub(crate) fn contract_of(node: &Node, id: &ContractID) -> Contract {
    let record = node
        .contract(id)
        .unwrap_or_else(|| panic!("the node has contract {}", hex::encode(id)));
    contract_from_bytes(&record.contract).expect("canonical contract bytes decode")
}

#[test]
#[ignore = "prints the address configs/chainparams.toml must allocate to"]
fn print_account_a_address() {
    let a = account(&SEED_A);
    println!(
        "{}",
        a.address_at(util::RECEIVING, 0)
            .expect("address")
            .to_bech32(Network::Testnet)
    );
}

/// `blocks.bin` is the only durable state this node has, so the way it
/// fails matters as much as the way it works.
mod store {
    use std::fs::OpenOptions;
    use std::io::Write;

    use flamechain::ChainParams;
    use tempfile::TempDir;

    use crate::store::{BlockStore, StoreError};

    #[test]
    fn an_empty_archive_opens_and_replays_to_nothing() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("blocks.bin");
        let mut store = BlockStore::open(&path).expect("create");
        assert!(store.is_empty());
        let Ok(blocks) = store.replay(ChainParams::default()) else {
            panic!("an empty archive replays");
        };
        assert!(blocks.is_empty());
        assert!(path.exists(), "opening creates the archive");
    }

    #[test]
    fn a_torn_record_is_refused_at_its_own_offset() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("blocks.bin");
        BlockStore::open(&path).expect("create");

        // A length prefix promising 4 KiB, followed by nothing: exactly
        // what a process killed mid-append leaves behind.
        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(&4096u64.to_le_bytes()).expect("write");
        file.sync_data().expect("sync");

        let mut store = BlockStore::open(&path).expect("reopen");
        // `Block` is not `Debug`, so unwrap the error by hand.
        let Err(error) = store.replay(ChainParams::default()) else {
            panic!("a torn record is not a block");
        };
        // The offset is the record's own start, so truncating there is the
        // recovery flamed.md documents.
        assert!(
            matches!(error, StoreError::Truncated { offset: 0 }),
            "expected a truncation at byte 0, got {error}"
        );
    }

    #[test]
    fn a_length_prefix_that_would_overflow_is_a_truncation() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("blocks.bin");
        BlockStore::open(&path).expect("create");

        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(&u64::MAX.to_le_bytes()).expect("write");
        file.sync_data().expect("sync");

        let mut store = BlockStore::open(&path).expect("reopen");
        let Err(error) = store.replay(ChainParams::default()) else {
            panic!("a length prefix of u64::MAX is not a block");
        };
        assert!(matches!(error, StoreError::Truncated { offset: 0 }));
    }
}
