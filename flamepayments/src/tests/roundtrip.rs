//! Two accounts, two blocks, on a real chain: a cleartext genesis
//! allocation becomes a confidential payment, its recipient opens the note
//! and spends it on, and every membership proof is refreshed through the
//! catchup of the block that moved it.

use flamechain::utreexo::{Catchup, Proof};
use flamechain::{utreexo_hasher, Blockchain, ChainParams, ContractLeaf};
use flamekd::{util, Network, ReceivingAddress};
use flamevm::{Anchor, ClearToken, Contract, ContractID, Scalar, TxLog, Value, FLAME_FLAVOR};

use super::{header, inputs, limits, native_output, outputs, publish, receive, rng};
use crate::builder::{block_tx, build_transfer, sign, InputSpec};
use crate::keys::Account;

const A_SEED: [u8; 64] = [0x41; 64];
const B_SEED: [u8; 64] = [0x42; 64];

const FLAME: u64 = flamechain::SPARKS_PER_FLAME;
const ALLOCATION: u64 = 1_000 * FLAME;
const FEE: u64 = 1_000;

fn account(seed: &[u8; 64]) -> Account {
    Account::from_seed(seed, Network::Testnet, 0).expect("account from seed")
}

/// Runs `proof` through `catchup` and checks the result against the chain's
/// tip forest. A contract the tip holds must come back `Committed`.
fn refresh(chain: &Blockchain, catchup: &Catchup, id: ContractID, proof: Proof) -> Proof {
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
    updated
}

/// The log pays exactly these addresses, in whatever order the builder
/// sorted its outputs into.
fn assert_pays(log: &TxLog, expected: &[ReceivingAddress]) {
    let mut created: Vec<[u8; 32]> = outputs(log)
        .iter()
        .map(|contract| contract.predicate.to_point().to_bytes())
        .collect();
    let mut expected: Vec<[u8; 32]> = expected
        .iter()
        .map(|address| address.spending_key().compress().to_bytes())
        .collect();
    created.sort();
    expected.sort();
    assert_eq!(created, expected, "the outputs pay these addresses");
}

#[test]
fn two_accounts_spend_across_two_blocks() {
    let alice = account(&A_SEED);
    let bob = account(&B_SEED);
    let mut rng = rng(20);

    // 1. One cleartext allocation for Alice, as a devnet genesis makes them.
    let allocation = Contract::new(
        alice.predicate_at(util::RECEIVING, 0).expect("predicate"),
        Anchor([0x07; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(ALLOCATION), FLAME_FLAVOR)),
    )
    .expect("a non-negative clear token is portable");
    let allocation_id = allocation.id();

    // 2. A chain whose accumulator already holds it, and Alice's proof.
    let (mut chain, genesis_catchup) =
        Blockchain::devnet_genesis(ChainParams::default(), &[allocation_id])
            .expect("devnet genesis");
    let allocation_proof = refresh(&chain, &genesis_catchup, allocation_id, Proof::Transient);

    // 3. Alice pays Bob 400 Flame and keeps the change.
    let payment = 400 * FLAME;
    let change = ALLOCATION - payment - FEE;
    let alice_key = alice.spending_key_at(util::RECEIVING, 0).expect("key");
    let input = InputSpec::clear(allocation, allocation_proof.clone(), alice_key)
        .expect("clear input on the allocation");
    let bob_address = bob.address_at(util::RECEIVING, 0).expect("address");
    let alice_change_address = alice.address_at(util::CHANGE, 0).expect("address");
    let to_bob = native_output(bob_address, payment);
    let to_change = native_output(alice_change_address, change);

    let unsigned = build_transfer(
        &[input],
        &[to_bob, to_change],
        FEE,
        header(),
        limits(),
        &mut rng,
    )
    .expect("build the payment");

    // The log is readable before signing: the wallet knows what it is about
    // to authorize.
    assert_eq!(inputs(unsigned.log()), vec![allocation_id]);
    assert_pays(unsigned.log(), &[bob_address, alice_change_address]);

    let tx = sign(unsigned, &[alice_key]).expect("sign");
    let (_, published) = publish(&tx);
    // Each side opens its own output's note; this test never spends Alice's
    // change, but her note must open all the same.
    let (bob_contract, bob_note) = receive(&published, &bob, util::RECEIVING, 0);
    let (alice_change, alice_change_note) = receive(&published, &alice, util::CHANGE, 0);
    assert_eq!(bob_note.opening.qty, payment);
    assert_eq!(alice_change_note.opening.qty, change);

    let block = chain
        .build_block(
            [1; 32],
            vec![block_tx(tx, limits(), vec![allocation_proof])],
        )
        .expect("build block 1");
    let applied = chain.connect(&block).expect("connect block 1");

    // 4. Both new contracts are in the accumulator.
    let bob_proof = refresh(
        &chain,
        &applied.catchup,
        bob_contract.id(),
        Proof::Transient,
    );
    let alice_change_proof = refresh(
        &chain,
        &applied.catchup,
        alice_change.id(),
        Proof::Transient,
    );

    // 5. Bob rebuilds his input the way a recipient must: from the contract
    // as published, plus the opening its note carried.
    let bob_payment = 100 * FLAME;
    let bob_change = payment - bob_payment - FEE;
    let bob_key = bob.spending_key_at(util::RECEIVING, 0).expect("key");
    let input =
        InputSpec::confidential(&bob_contract, &bob_note.opening, bob_proof.clone(), bob_key)
            .expect("confidential input on Bob's contract");

    let alice_second_address = alice.address_at(util::RECEIVING, 1).expect("address");
    let bob_change_address = bob.address_at(util::CHANGE, 0).expect("address");
    let to_alice = native_output(alice_second_address, bob_payment);
    let to_bob_change = native_output(bob_change_address, bob_change);

    let unsigned = build_transfer(
        &[input],
        &[to_alice, to_bob_change],
        FEE,
        header(),
        limits(),
        &mut rng,
    )
    .expect("build Bob's payment");
    assert_eq!(inputs(unsigned.log()), vec![bob_contract.id()]);
    assert_pays(unsigned.log(), &[alice_second_address, bob_change_address]);

    let tx = sign(unsigned, &[bob_key]).expect("sign");
    let (_, published) = publish(&tx);
    let (alice_received, _) = receive(&published, &alice, util::RECEIVING, 1);
    let (bob_change_contract, _) = receive(&published, &bob, util::CHANGE, 0);

    // Bob's proof is still the one from block 1: no block has passed since.
    let block = chain
        .build_block([2; 32], vec![block_tx(tx, limits(), vec![bob_proof])])
        .expect("build block 2");
    let applied = chain.connect(&block).expect("connect block 2");

    // 6. Alice's change survives a block it took no part in; the two new
    // contracts enter the accumulator.
    refresh(
        &chain,
        &applied.catchup,
        alice_change.id(),
        alice_change_proof,
    );
    refresh(
        &chain,
        &applied.catchup,
        bob_change_contract.id(),
        Proof::Transient,
    );
    refresh(
        &chain,
        &applied.catchup,
        alice_received.id(),
        Proof::Transient,
    );

    // Alice recognizes both of her live contracts without holding any index.
    assert_eq!(
        alice.owns(&alice_change.predicate.to_point(), 20),
        Some((util::CHANGE, 0))
    );
    assert_eq!(
        alice.owns(&alice_received.predicate.to_point(), 20),
        Some((util::RECEIVING, 1))
    );
}
