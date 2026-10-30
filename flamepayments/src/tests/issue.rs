//! Confidential issuance on a real chain: a genesis allocation mints a
//! flavor, its change mints more of the same flavor, and a signed issuance
//! program refuses to run on any other contract under the same key.

use flamechain::utreexo::Proof;
use flamechain::{utreexo_hasher, Blockchain, ChainParams, ContractLeaf};
use flamekd::{util, Network};
use flamevm::{
    Anchor, ClearToken, Contract, ContractID, Scalar, TxEntry, TxLog, Value, FLAME_FLAVOR,
};

use super::{header, inputs, limits, publish, receive, rng};
use crate::builder::{
    block_tx, build_issue, issue_program, issued_flavor, signcall_tx, InputSpec, OutputSpec,
};
use crate::keys::SpendAccount;

const A_SEED: [u8; 64] = [0x51; 64];
const B_SEED: [u8; 64] = [0x52; 64];

const ALLOCATION: u64 = 1_000 * flamechain::SPARKS_PER_FLAME;
const FEE: u64 = 1_000;

fn account(seed: &[u8; 64]) -> SpendAccount {
    SpendAccount::from_seed(seed, Network::Testnet, 0).expect("account from seed")
}

/// A cleartext allocation under `RECEIVING/0`, as a devnet genesis makes them.
fn allocation(account: &SpendAccount, anchor: u8) -> Contract {
    Contract::new(
        account.predicate_at(util::RECEIVING, 0).expect("predicate"),
        Anchor([anchor; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(ALLOCATION), FLAME_FLAVOR)),
    )
    .expect("a non-negative clear token is portable")
}

fn committed(chain: &Blockchain, catchup: &flamechain::utreexo::Catchup, id: ContractID) -> Proof {
    let hasher = utreexo_hasher::<ContractLeaf>();
    catchup
        .update_proof(&ContractLeaf(id), Proof::Transient, &hasher)
        .inspect(|proof| {
            let Proof::Committed(path) = proof else {
                panic!("a contract the tip holds has a committed proof");
            };
            chain
                .contract_forest()
                .verify(&ContractLeaf(id), path, &hasher)
                .expect("the proof verifies against the tip forest");
        })
        .expect("the catchup accepts the proof")
}

fn issued(log: &TxLog) -> usize {
    log.entries()
        .iter()
        .filter(|entry| matches!(entry, TxEntry::IssuePriv(..)))
        .count()
}

#[test]
fn issuance_mints_one_flavor_per_issuer_and_tag() {
    let alice = account(&A_SEED);
    let bob = account(&B_SEED);
    let mut rng = rng(30);
    let issuer = alice.predicate_at(util::RECEIVING, 0).expect("predicate");
    let issuer_address = alice.address_at(util::RECEIVING, 0).expect("address");
    let bob_address = bob.address_at(util::RECEIVING, 0).expect("address");
    let key = alice.spending_key_at(util::RECEIVING, 0).expect("key");
    let soot = issued_flavor(&issuer, b"soot");

    let genesis = allocation(&alice, 0x07);
    let (mut chain, catchup) =
        Blockchain::devnet_genesis(ChainParams::default(), &[genesis.id()]).expect("genesis");
    let proof = committed(&chain, &catchup, genesis.id());

    // 1. 1000 soot to Bob; the Flame, less the fee, back to the issuer.
    let input = InputSpec::clear(genesis.clone(), proof.clone(), key).expect("input");
    let tx = build_issue(
        &input,
        b"soot",
        1_000,
        &[
            OutputSpec {
                address: bob_address,
                qty: 1_000,
                flv: soot,
                memo: b"fresh soot".to_vec(),
                clear: false,
            },
            OutputSpec {
                address: issuer_address,
                qty: ALLOCATION - FEE,
                flv: FLAME_FLAVOR,
                memo: Vec::new(),
                clear: false,
            },
        ],
        FEE,
        header(),
        limits(),
        &mut rng,
    )
    .expect("build the issuance")
    .0;
    let (_, log) = publish(&tx);
    assert_eq!(inputs(&log), vec![genesis.id()]);
    assert_eq!(issued(&log), 1);
    let (bob_soot, bob_note) = receive(&log, &bob, util::RECEIVING, 0);
    assert_eq!(bob_note.opening.qty, 1_000);
    assert_eq!(bob_note.opening.flv, soot);
    assert_eq!(bob_note.memo, b"fresh soot");
    let (change, change_note) = receive(&log, &alice, util::RECEIVING, 0);
    assert_eq!(change_note.opening.qty, ALLOCATION - FEE);
    assert_eq!(change_note.opening.flv, FLAME_FLAVOR);

    let block = chain
        .build_block([1; 32], vec![block_tx(tx, limits(), vec![proof])])
        .expect("build block 1");
    let applied = chain.connect(&block).expect("connect block 1");
    committed(&chain, &applied.catchup, bob_soot.id());

    // 2. The issuer's confidential change mints again under the same tag:
    // the same flavor, this time kept.
    let change_proof = committed(&chain, &applied.catchup, change.id());
    let input = InputSpec::confidential(&change, &change_note.opening, change_proof.clone(), key)
        .expect("confidential input");
    let tx = build_issue(
        &input,
        b"soot",
        500,
        &[
            OutputSpec {
                address: issuer_address,
                qty: 500,
                flv: soot,
                memo: Vec::new(),
                clear: false,
            },
            OutputSpec {
                address: alice.address_at(util::CHANGE, 0).expect("address"),
                qty: ALLOCATION - 2 * FEE,
                flv: FLAME_FLAVOR,
                memo: Vec::new(),
                clear: false,
            },
        ],
        FEE,
        header(),
        limits(),
        &mut rng,
    )
    .expect("build the second issuance")
    .0;
    let (_, log) = publish(&tx);
    let (_, kept) = receive(&log, &alice, util::RECEIVING, 0);
    assert_eq!((kept.opening.qty, kept.opening.flv), (500, soot));
    let block = chain
        .build_block([2; 32], vec![block_tx(tx, limits(), vec![change_proof])])
        .expect("build block 2");
    chain.connect(&block).expect("connect block 2");

    // Another tag, or another issuer, is another flavor.
    assert_ne!(issued_flavor(&issuer, b"ash"), soot);
    let other = alice.predicate_at(util::RECEIVING, 1).expect("predicate");
    assert_ne!(issued_flavor(&other, b"soot"), soot);
}

#[test]
fn a_signed_issuance_runs_on_its_own_contract_only() {
    let alice = account(&A_SEED);
    let mut rng = rng(31);
    let issuer = alice.predicate_at(util::RECEIVING, 0).expect("predicate");
    let address = alice.address_at(util::RECEIVING, 0).expect("address");
    let key = alice.spending_key_at(util::RECEIVING, 0).expect("key");

    // Two allocations under the same key and of the same amount, so the
    // program's `mix` would balance on either: only its anchor check tells
    // them apart.
    let signed_for = allocation(&alice, 0x07);
    let other = allocation(&alice, 0x08);
    let outputs = [
        OutputSpec {
            address,
            qty: 1_000,
            flv: issued_flavor(&issuer, b"soot"),
            memo: Vec::new(),
            clear: false,
        },
        OutputSpec {
            address: alice.address_at(util::CHANGE, 0).expect("address"),
            qty: ALLOCATION - FEE,
            flv: FLAME_FLAVOR,
            memo: Vec::new(),
            clear: false,
        },
    ];
    let input = InputSpec::clear(signed_for.clone(), Proof::Transient, key).expect("input");
    let (program, signature) =
        issue_program(&input, b"soot", 1_000, &outputs, FEE, &mut rng).expect("program");

    let replay = signcall_tx(&other, program.clone(), signature, header(), limits());
    assert!(replay.is_err(), "the program refuses another contract");

    let (original, _) = signcall_tx(&signed_for, program, signature, header(), limits())
        .expect("the program runs on its own contract");
    let (_, log) = publish(&original);
    assert_eq!(issued(&log), 1);
}
