//! The facade exercised only through its exported surface, the way a
//! binding would: bytes in from a chain, bytes out to one.

use flamechain::codec::{contract_bytes, proof_bytes};
use flamechain::utreexo::{Catchup, Proof};
use flamechain::{utreexo_hasher, BlockTx, Blockchain, ChainParams, ContractLeaf};
use flamevm::{Anchor, ClearToken, Contract, ContractID, Predicate, Scalar, Value, FLAME_FLAVOR};

use crate::*;

const A_SEED: [u8; 64] = [0x41; 64];
const B_SEED: [u8; 64] = [0x42; 64];
const FLAME: u64 = flamechain::SPARKS_PER_FLAME;
const ALLOCATION: u64 = 1_000 * FLAME;
const FEE: u64 = 1_000;
const GAS: u64 = 10_000_000;

fn wallet(seed: &[u8; 64]) -> std::sync::Arc<Wallet> {
    Wallet::new(seed.to_vec(), Network::Testnet, 0).expect("wallet from seed")
}

fn receiving(index: u32) -> KeyPath {
    KeyPath {
        branch: RECEIVING,
        index,
    }
}

/// What an indexer would serve for `id` after the block behind `catchup`.
fn served_proof(chain: &Blockchain, catchup: &Catchup, id: ContractID, proof: Proof) -> Vec<u8> {
    let hasher = utreexo_hasher::<ContractLeaf>();
    let leaf = ContractLeaf(id);
    let updated = catchup
        .update_proof(&leaf, proof, &hasher)
        .expect("the catchup accepts the proof");
    let Proof::Committed(path) = &updated else {
        panic!("a contract the tip holds must have a committed proof");
    };
    chain
        .contract_forest()
        .verify(&leaf, path, &hasher)
        .expect("the committed proof verifies against the tip forest");
    proof_bytes(&updated)
}

/// Decodes a transfer the way a node receives it, and connects it.
fn connect(chain: &mut Blockchain, height: u8, transfer: &Transfer) -> flamechain::AppliedBlock {
    let params = ChainParams::default();
    let tx = BlockTx::from_bytes_bounded(&transfer.block_tx, params.version, params.limits)
        .expect("the published bytes decode");
    let block = chain
        .build_block([height; 32], vec![tx])
        .expect("build block");
    chain.connect(&block).expect("connect block")
}

fn id(bytes: &[u8]) -> ContractID {
    bytes.try_into().expect("a 32-byte id")
}

#[test]
fn mnemonic_round_trip() {
    let phrase = generate_mnemonic(24).expect("generate");
    assert_eq!(phrase.split(' ').count(), 24);
    assert!(validate_mnemonic(phrase.clone()));
    assert!(!validate_mnemonic("abandon abandon".into()));
    assert!(generate_mnemonic(13).is_err());

    let seed = mnemonic_to_seed(phrase.clone(), "pass".into()).expect("seed");
    assert_eq!(seed.len(), 64);
    let from_seed = Wallet::new(seed, Network::Mainnet, 0).expect("wallet");
    let from_phrase =
        Wallet::from_mnemonic(phrase, "pass".into(), Network::Mainnet, 0).expect("wallet");
    assert_eq!(
        from_seed.address(receiving(3)).expect("address"),
        from_phrase.address(receiving(3)).expect("address"),
    );
}

#[test]
fn addresses_and_counter() {
    let alice = wallet(&A_SEED);
    let first = alice.next_address().expect("issue");
    assert_eq!(first.path, receiving(0));
    assert!(first.address.starts_with("tf1"));
    assert_eq!(alice.next_index(), 1);
    assert_eq!(
        address_to_predicate(first.address.clone(), Network::Testnet).expect("parse"),
        first.predicate
    );
    assert!(matches!(
        address_to_predicate(first.address, Network::Mainnet),
        Err(FlameError::InvalidAddress { .. })
    ));

    // A reopened wallet finds an address issued past its counter only once
    // the counter is restored or the gap covers it.
    let later = alice.address(receiving(40)).expect("address");
    let fresh = wallet(&A_SEED);
    assert_eq!(fresh.owns(later.predicate.clone(), 20).expect("owns"), None);
    let reopened = Wallet::new(A_SEED.to_vec(), Network::Testnet, 41).expect("restore");
    assert_eq!(reopened.next_index(), 41);
    assert_eq!(
        reopened.owns(later.predicate, 0).expect("owns"),
        Some(receiving(40))
    );
    assert_eq!(reopened.next_address().expect("issue").path, receiving(41));
    assert!(Wallet::new(A_SEED.to_vec(), Network::Testnet, 1 << 31).is_err());
    assert!(reopened.receiving_key().starts_with("testrecv1"));
}

#[test]
fn two_wallets_spend_across_two_blocks() {
    let alice = wallet(&A_SEED);
    let bob = wallet(&B_SEED);
    let alice_address = alice.next_address().expect("issue");

    // 1. A cleartext genesis allocation to Alice, as an indexer serves it.
    let allocation = Contract::new(
        Predicate::opaque(curve25519_dalek::ristretto::CompressedRistretto(
            alice_address
                .predicate
                .clone()
                .try_into()
                .expect("32 bytes"),
        )),
        Anchor([0x07; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(ALLOCATION), FLAME_FLAVOR)),
    )
    .expect("a non-negative clear token is portable");
    let allocation_bytes = contract_bytes(&allocation).expect("encode");
    let (mut chain, genesis) =
        Blockchain::devnet_genesis(ChainParams::default(), &[allocation.id()])
            .expect("devnet genesis");
    let allocation_proof = served_proof(&chain, &genesis, allocation.id(), Proof::Transient);

    let info = decode_contract(allocation_bytes.clone()).expect("decode");
    assert_eq!(info.id, allocation.id().to_vec());
    assert_eq!(
        info.value,
        ContractValue::Clear {
            qty: ALLOCATION,
            flavor: FLAME_FLAVOR.to_bytes().to_vec()
        }
    );
    assert_eq!(
        alice.owns(info.predicate, 20).expect("owns"),
        Some(receiving(0))
    );

    // 2. Alice pays Bob and keeps the change.
    let bob_address = bob.next_address().expect("issue");
    let change = alice
        .address(KeyPath {
            branch: CHANGE,
            index: 0,
        })
        .expect("change");
    let payment = 400 * FLAME;
    let request = TransferRequest {
        inputs: vec![TransferInput {
            contract: allocation_bytes.clone(),
            proof: allocation_proof.clone(),
            path: receiving(0),
            opening: None,
        }],
        outputs: vec![
            TransferOutput {
                predicate: address_to_predicate(bob_address.address, Network::Testnet)
                    .expect("parse"),
                qty: payment,
                flavor: None,
            },
            TransferOutput {
                predicate: change.predicate,
                qty: ALLOCATION - payment - FEE,
                flavor: None,
            },
        ],
        fee: FEE,
        gas: GAS,
        locktime: 0,
    };

    // The wrong key path is refused before anything is proven.
    let mut wrong = request.clone();
    wrong.inputs[0].path = receiving(1);
    assert!(matches!(
        alice.build_transfer(wrong),
        Err(FlameError::KeyMismatch { input: 0 })
    ));

    let transfer = alice.build_transfer(request).expect("build the payment");
    assert_eq!(transfer.txid.len(), 32);
    assert_eq!(transfer.outputs.len(), 2);
    let applied = connect(&mut chain, 1, &transfer);

    // 3. Bob reads his output, checks the opening Alice sent him, and
    // recognizes it as his.
    let to_bob = &transfer.outputs[0];
    let info = decode_contract(to_bob.contract.clone()).expect("decode");
    assert_eq!(info.value, ContractValue::Confidential);
    assert_eq!(
        bob.owns(info.predicate, 20).expect("owns"),
        Some(receiving(0))
    );
    assert!(opening_matches(to_bob.contract.clone(), to_bob.opening.clone()).expect("check"));
    assert!(
        !opening_matches(to_bob.contract.clone(), transfer.outputs[1].opening.clone())
            .expect("check")
    );
    let bob_proof = served_proof(
        &chain,
        &applied.catchup,
        id(&to_bob.contract_id),
        Proof::Transient,
    );

    // 4. Bob spends it on, confidentially, back to Alice.
    let back = 100 * FLAME;
    let bob_change = bob
        .address(KeyPath {
            branch: CHANGE,
            index: 0,
        })
        .expect("change");
    let alice_second = alice.next_address().expect("issue");
    let transfer = bob
        .build_transfer(TransferRequest {
            inputs: vec![TransferInput {
                contract: to_bob.contract.clone(),
                proof: bob_proof,
                path: receiving(0),
                opening: Some(to_bob.opening.clone()),
            }],
            outputs: vec![
                TransferOutput {
                    predicate: alice_second.predicate.clone(),
                    qty: back,
                    flavor: None,
                },
                TransferOutput {
                    predicate: bob_change.predicate,
                    qty: payment - back - FEE,
                    flavor: None,
                },
            ],
            fee: FEE,
            gas: GAS,
            locktime: 0,
        })
        .expect("build Bob's payment");
    let applied = connect(&mut chain, 2, &transfer);
    served_proof(
        &chain,
        &applied.catchup,
        id(&transfer.outputs[0].contract_id),
        Proof::Transient,
    );
    assert_eq!(
        alice.owns(alice_second.predicate, 0).expect("owns"),
        Some(receiving(1))
    );
}

#[test]
fn malformed_bytes_are_named() {
    let alice = wallet(&A_SEED);
    let bad = |what: &str, error: FlameError| match error {
        FlameError::InvalidBytes { what: named, .. } => assert_eq!(named, what),
        other => panic!("expected InvalidBytes({what}), got {other:?}"),
    };
    bad("contract", decode_contract(vec![1, 2, 3]).unwrap_err());
    bad("predicate", alice.owns(vec![0; 31], 1).unwrap_err());
    assert!(matches!(
        Wallet::new(vec![0; 63], Network::Testnet, 0),
        Err(FlameError::InvalidSeed { .. })
    ));
}
