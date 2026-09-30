//! Two accounts, two blocks, on a real chain: a cleartext genesis
//! allocation becomes a confidential payment, the payment is spent on, and
//! every membership proof is refreshed through the catchup of the block
//! that moved it.

use flamechain::utreexo::{Catchup, Proof};
use flamechain::{utreexo_hasher, Blockchain, ChainParams, ContractLeaf};
use flamekd::{util, Network};
use flamevm::{
    Anchor, ClearToken, Contract, ContractID, Predicate, Scalar, TxLog, Value, FLAME_FLAVOR,
};

use super::{header, inputs, limits, native_output, outputs, publish};
use crate::builder::{block_tx, build_transfer, sign, InputSpec};
use crate::keys::Account;

const A_SEED: [u8; 64] = [0x41; 64];
const B_SEED: [u8; 64] = [0x42; 64];

const FLAME: u64 = flamechain::SPARKS_PER_FLAME;
const ALLOCATION: u64 = 1_000 * FLAME;
const FEE: u64 = 1_000;

fn account(seed: &[u8; 64]) -> Account {
    Account::from_seed(seed, Network::Testnet).expect("account from seed")
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

fn assert_predicates(log: &TxLog, expected: &[&Predicate]) {
    let created = outputs(log);
    assert_eq!(created.len(), expected.len(), "output count");
    for (contract, predicate) in created.iter().zip(expected) {
        assert_eq!(
            contract.predicate.to_point(),
            predicate.to_point(),
            "output predicate"
        );
    }
}

#[test]
fn two_accounts_spend_across_two_blocks() {
    let alice = account(&A_SEED);
    let bob = account(&B_SEED);

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
    let bob_predicate = bob.predicate_at(util::RECEIVING, 0).expect("predicate");
    let alice_change_predicate = alice.predicate_at(util::CHANGE, 0).expect("predicate");
    let (to_bob, bob_opening) = native_output(bob_predicate.clone(), payment, 0);
    // Alice's own change opening is hers to keep; this test never spends it.
    let (to_change, _) = native_output(alice_change_predicate.clone(), change, 1);

    let unsigned = build_transfer(&[input], &[to_bob, to_change], FEE, header(), limits())
        .expect("build the payment");

    // The log is readable before signing: the wallet knows what it is about
    // to authorize.
    assert_eq!(inputs(unsigned.log()), vec![allocation_id]);
    assert_predicates(unsigned.log(), &[&bob_predicate, &alice_change_predicate]);

    let tx = sign(unsigned, &[alice_key]).expect("sign");
    let (_, published) = publish(&tx);
    let created = outputs(&published);
    let bob_contract = created[0].clone();
    let alice_change = created[1].clone();

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
    // as published, plus the opening Alice sent him out of band.
    let bob_payment = 100 * FLAME;
    let bob_change = payment - bob_payment - FEE;
    let bob_key = bob.spending_key_at(util::RECEIVING, 0).expect("key");
    let input = InputSpec::confidential(&bob_contract, &bob_opening, bob_proof.clone(), bob_key)
        .expect("confidential input on Bob's contract");

    let alice_second_predicate = alice.predicate_at(util::RECEIVING, 1).expect("predicate");
    let bob_change_predicate = bob.predicate_at(util::CHANGE, 0).expect("predicate");
    let (to_alice, _) = native_output(alice_second_predicate.clone(), bob_payment, 2);
    let (to_bob_change, _) = native_output(bob_change_predicate.clone(), bob_change, 3);

    let unsigned = build_transfer(
        &[input],
        &[to_alice, to_bob_change],
        FEE,
        header(),
        limits(),
    )
    .expect("build Bob's payment");
    assert_eq!(inputs(unsigned.log()), vec![bob_contract.id()]);
    assert_predicates(
        unsigned.log(),
        &[&alice_second_predicate, &bob_change_predicate],
    );

    let tx = sign(unsigned, &[bob_key]).expect("sign");
    let (_, published) = publish(&tx);
    let created = outputs(&published);
    let alice_received = created[0].clone();
    let bob_change_contract = created[1].clone();

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
