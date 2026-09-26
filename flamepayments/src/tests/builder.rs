//! The transfer builder: a cleartext spend, a confidential spend, the
//! opening checks, and the wire test that is this phase's whole point.

use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamekd::{util, Network};
use flamevm::{Anchor, ClearToken, Contract, ContractID, Scalar, TxLog, Value, FLAME_FLAVOR};
use rand::rngs::StdRng;

use super::{fees, header, inputs, limits, native_output, outputs, publish, receive, rng};
use crate::builder::{
    build_transfer, sign, BuilderError, InputSpec, Opening, OutputSpec, MAX_OUTPUTS,
};
use crate::keys::Account;

const SEED: [u8; 64] = [0x21; 64];

/// A distinctive quantity: the wire test scans the bytes for it, and a round
/// number with a long run of zeros would make a chance match likelier.
const GENESIS_QTY: u64 = 12_345_678_901;

fn account() -> Account {
    Account::from_seed(&SEED, Network::Testnet).expect("account from seed")
}

/// A cleartext contract under `RECEIVING/n`, shaped like a devnet genesis
/// allocation. `anchor` keeps sibling contracts distinct.
fn clear_contract(account: &Account, n: u32, anchor: u8, qty: u64) -> Contract {
    Contract::new(
        account
            .predicate_at(util::RECEIVING, n)
            .expect("receiving predicate"),
        Anchor([anchor; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(qty), FLAME_FLAVOR)),
    )
    .expect("a non-negative clear token is portable")
}

/// The allocation most tests spend, under `RECEIVING/3`.
fn genesis_contract(account: &Account, qty: u64) -> Contract {
    clear_contract(account, 3, 0x07, qty)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// One cleartext payment: the whole genesis contract to `RECEIVING/4`, with
/// `fee` taken out of the single output. The opening is the one the
/// recipient reads from the output's note.
struct Payment {
    bytes: Vec<u8>,
    log: TxLog,
    genesis_id: ContractID,
    received: Contract,
    opening: Opening,
}

fn cleartext_payment(account: &Account, fee: u64, rng: &mut StdRng) -> Payment {
    let genesis = genesis_contract(account, GENESIS_QTY);
    let genesis_id = genesis.id();
    let spend_key = account
        .spending_key_at(util::RECEIVING, 3)
        .expect("spending key");
    let input = InputSpec::clear(genesis, Proof::Transient, spend_key).expect("clear input");
    let output = native_output(
        account.address_at(util::RECEIVING, 4).expect("address"),
        GENESIS_QTY - fee,
    );

    let unsigned =
        build_transfer(&[input], &[output], fee, header(), limits(), rng).expect("build");
    let tx = sign(unsigned, &[spend_key]).expect("sign");
    let (bytes, log) = publish(&tx);
    let (received, note) = receive(&log, account, util::RECEIVING, 4);
    Payment {
        bytes,
        log,
        genesis_id,
        received,
        opening: note.opening,
    }
}

/// The recipient of `payment` spends it on, paying `fee`.
fn confidential_spend(
    account: &Account,
    payment: &Payment,
    fee: u64,
    rng: &mut StdRng,
) -> (Vec<u8>, TxLog) {
    let spend_key = account
        .spending_key_at(util::RECEIVING, 4)
        .expect("spending key");
    let input = InputSpec::confidential(
        &payment.received,
        &payment.opening,
        Proof::Transient,
        spend_key,
    )
    .expect("confidential input");
    let output = native_output(
        account.address_at(util::RECEIVING, 5).expect("address"),
        payment.opening.qty - fee,
    );

    let unsigned =
        build_transfer(&[input], &[output], fee, header(), limits(), rng).expect("build");
    let tx = sign(unsigned, &[spend_key]).expect("sign");
    publish(&tx)
}

#[test]
fn a_cleartext_contract_pays_one_output_with_no_fee() {
    let account = account();
    let payment = cleartext_payment(&account, 0, &mut rng(1));

    assert_eq!(inputs(&payment.log), vec![payment.genesis_id]);
    assert_eq!(outputs(&payment.log).len(), 1);
    assert!(
        fees(&payment.log).is_empty(),
        "a zero fee emits no fee opcode"
    );
    assert_eq!(
        payment.received.predicate.to_point(),
        account
            .predicate_at(util::RECEIVING, 4)
            .expect("predicate")
            .to_point()
    );
}

#[test]
fn a_cleartext_contract_pays_one_output_with_a_fee() {
    let account = account();
    let payment = cleartext_payment(&account, 1_000, &mut rng(2));

    assert_eq!(inputs(&payment.log), vec![payment.genesis_id]);
    assert_eq!(outputs(&payment.log).len(), 1);
    assert_eq!(fees(&payment.log), vec![1_000]);
    assert_eq!(payment.opening.qty, GENESIS_QTY - 1_000);
}

/// A cleartext contract spends to one confidential output; the recipient
/// then spends that output back through the opening its note carried. The
/// second half is the path no upstream test exercises end to end.
#[test]
fn a_confidential_output_is_spent_through_its_opening() {
    let account = account();
    let mut rng = rng(3);
    let payment = cleartext_payment(&account, 0, &mut rng);
    let received_id = payment.received.id();
    let (_, log) = confidential_spend(&account, &payment, 1_000, &mut rng);

    assert_eq!(inputs(&log), vec![received_id], "the confidential input");
    assert_eq!(outputs(&log).len(), 1);
    assert_eq!(fees(&log), vec![1_000]);
}

/// **The wire test.** A spend of a confidential output must publish nothing
/// about the amount it spends.
#[test]
fn a_confidential_spend_publishes_no_amounts() {
    let account = account();
    let mut rng = rng(4);
    let payment = cleartext_payment(&account, 0, &mut rng);
    let opening = payment.opening;

    // The payment that created the contract spends the cleartext genesis
    // contract, whose body rides in the execution bag with its quantity in
    // the clear by design. That is the positive control: it proves the byte
    // scan below can find a quantity when one is really there.
    assert!(
        contains(&payment.bytes, &GENESIS_QTY.to_le_bytes()),
        "the cleartext input's quantity is public by design"
    );
    // Its own output's blindings, though, never leave the wallet.
    assert!(!contains(&payment.bytes, opening.qty_blinding.as_bytes()));
    assert!(!contains(&payment.bytes, opening.flv_blinding.as_bytes()));

    // The spend is where the leak lived. Nothing about the spent amount may
    // appear: not the quantity as eight little-endian bytes, not the same
    // quantity as a 32-byte scalar, not either blinding factor.
    let (bytes, _) = confidential_spend(&account, &payment, 1_000, &mut rng);
    assert!(
        !contains(&bytes, &opening.qty.to_le_bytes()),
        "the spent quantity must not appear as 8 little-endian bytes"
    );
    assert!(
        !contains(&bytes, &Scalar::from(opening.qty).to_bytes()),
        "the spent quantity must not appear as a 32-byte scalar"
    );
    assert!(
        !contains(&bytes, opening.qty_blinding.as_bytes()),
        "the quantity blinding factor must not appear"
    );
    assert!(
        !contains(&bytes, opening.flv_blinding.as_bytes()),
        "the flavor blinding factor must not appear"
    );
}

#[test]
fn confidential_refuses_a_wrong_blinding_factor() {
    let account = account();
    let payment = cleartext_payment(&account, 0, &mut rng(5));
    let wrong = Opening {
        qty_blinding: payment.opening.qty_blinding + DalekScalar::ONE,
        ..payment.opening
    };

    let Err(error) = InputSpec::confidential(
        &payment.received,
        &wrong,
        Proof::Transient,
        account.spending_key_at(util::RECEIVING, 4).expect("key"),
    ) else {
        panic!("a wrong opening must rebuild a different contract id");
    };
    assert!(
        matches!(error, BuilderError::OpeningMismatch),
        "got {error:?}"
    );
}

#[test]
fn confidential_refuses_a_cleartext_payload() {
    let account = account();
    let payment = cleartext_payment(&account, 0, &mut rng(6));

    let Err(error) = InputSpec::confidential(
        &genesis_contract(&account, GENESIS_QTY),
        &payment.opening,
        Proof::Transient,
        account.spending_key_at(util::RECEIVING, 3).expect("key"),
    ) else {
        panic!("a ClearToken needs no opening");
    };
    assert!(
        matches!(error, BuilderError::OpeningNotNeeded),
        "got {error:?}"
    );
}

#[test]
fn clear_refuses_a_confidential_payload() {
    let account = account();
    let payment = cleartext_payment(&account, 0, &mut rng(7));

    let Err(error) = InputSpec::clear(
        payment.received,
        Proof::Transient,
        account.spending_key_at(util::RECEIVING, 4).expect("key"),
    ) else {
        panic!("a published Token's commitments are closed");
    };
    assert!(
        matches!(error, BuilderError::OpeningMissing),
        "got {error:?}"
    );
}

#[test]
fn an_input_that_holds_no_token_is_refused() {
    let account = account();
    let contract = Contract::new(
        account.predicate_at(util::RECEIVING, 3).expect("predicate"),
        Anchor([0x07; 32]),
        Value::Scalar(Scalar::from(1u64)),
    )
    .expect("a scalar is portable");

    let Err(error) = InputSpec::clear(
        contract,
        Proof::Transient,
        account.spending_key_at(util::RECEIVING, 3).expect("key"),
    ) else {
        panic!("only token payloads can be spent as a transfer input");
    };
    assert!(
        matches!(error, BuilderError::PayloadNotToken),
        "got {error:?}"
    );
}

#[test]
fn more_than_sixteen_outputs_is_refused() {
    let account = account();
    let genesis = genesis_contract(&account, GENESIS_QTY);
    let spend_key = account.spending_key_at(util::RECEIVING, 3).expect("key");
    let input = InputSpec::clear(genesis, Proof::Transient, spend_key).expect("clear input");

    let address = account.address_at(util::RECEIVING, 4).expect("address");
    let too_many = MAX_OUTPUTS + 1;
    let outputs: Vec<OutputSpec> = (0..too_many).map(|_| native_output(address, 1)).collect();

    // Refused before any proving: the guard is the first thing build_transfer
    // does, which is also why this test costs nothing.
    let Err(error) = build_transfer(&[input], &outputs, 0, header(), limits(), &mut rng(8)) else {
        panic!("roll_k cannot address a seventeenth output");
    };
    assert!(
        matches!(error, BuilderError::TooManyOutputs(n) if n == too_many),
        "got {error:?}"
    );
}

/// The most outputs that can actually be proven, which is **not**
/// [`MAX_OUTPUTS`]. `MAX_OUTPUTS` is the `roll_k` encoding bound; the
/// prover's shared `BulletproofGens::new(1024, 1)` runs out first, because
/// `mix` range-proves every output over 64 bits. Measured: 13 proves, 14
/// does not. If this constant has to move, flamevm's generator capacity
/// changed.
const PROVABLE_OUTPUTS: usize = 13;

/// The widest transfer in the suite, and the one that proves the roll keeps
/// each output's commitments, predicate and note together. The predicate is
/// pushed immediately before `output` whatever the stack holds, so a
/// misplaced roll would still produce the right predicates, over the wrong
/// commitments. The test finds every output by its predicate and checks it
/// twice:
///
/// - `open_note` accepts the note after it only if the opening the note
///   carries rebuilds the published contract, and that opening must state
///   the quantity the caller asked this output for;
/// - `InputSpec::confidential`, the check a spend relies on and separate
///   code from `open_note`, must accept the same opening against the same
///   published contract.
#[test]
fn every_output_keeps_its_own_amount() {
    let account = account();
    let genesis = genesis_contract(&account, GENESIS_QTY);
    let spend_key = account
        .spending_key_at(util::RECEIVING, 3)
        .expect("spending key");
    let input = InputSpec::clear(genesis, Proof::Transient, spend_key).expect("clear input");

    // Distinct amounts and distinct predicates, so any mispairing shows.
    // The last output absorbs the remainder: `mix` balances or the prover
    // fails, and it fails as an opaque `R1CSProofConstruction`.
    let share = GENESIS_QTY / PROVABLE_OUTPUTS as u64;
    let mut qtys: Vec<u64> = (0..PROVABLE_OUTPUTS - 1)
        .map(|i| share + i as u64)
        .collect();
    qtys.push(GENESIS_QTY - qtys.iter().sum::<u64>());

    let specs: Vec<OutputSpec> = qtys
        .iter()
        .enumerate()
        .map(|(index, qty)| {
            native_output(
                account
                    .address_at(util::RECEIVING, 10 + index as u32)
                    .expect("address"),
                *qty,
            )
        })
        .collect();

    let unsigned =
        build_transfer(&[input], &specs, 0, header(), limits(), &mut rng(9)).expect("build");
    let tx = sign(unsigned, &[spend_key]).expect("sign");
    let (_, log) = publish(&tx);

    assert_eq!(
        outputs(&log).len(),
        PROVABLE_OUTPUTS,
        "every output is created"
    );
    for (index, spec) in specs.iter().enumerate() {
        let n = 10 + index as u32;
        let (contract, note) = receive(&log, &account, util::RECEIVING, n);
        assert_eq!(
            note.opening.qty, spec.qty,
            "output {index} carries its own amount"
        );
        assert_eq!(note.opening.flv, FLAME_FLAVOR, "output {index} flavor");
        let key = account.spending_key_at(util::RECEIVING, n).expect("key");
        if let Err(error) = InputSpec::confidential(&contract, &note.opening, Proof::Transient, key)
        {
            panic!("output {index}'s opening does not rebuild it as an input: {error}");
        }
    }
}

/// `sign` pairs `keys[i]` with the i-th `signtx` authorization. Every other
/// test spends one input, where that pairing is satisfied by construction.
#[test]
fn two_inputs_are_signed_in_input_order() {
    let account = account();
    // Two different keys: swapping identical ones would be a no-op.
    let key_a = account
        .spending_key_at(util::RECEIVING, 3)
        .expect("spending key");
    let key_b = account
        .spending_key_at(util::RECEIVING, 6)
        .expect("spending key");
    let qty_b = 7_654_321;

    let mut rng = rng(10);
    let mut transfer = || {
        let a = clear_contract(&account, 3, 0x07, GENESIS_QTY);
        let b = clear_contract(&account, 6, 0x08, qty_b);
        let ids = vec![a.id(), b.id()];
        let inputs = [
            InputSpec::clear(a, Proof::Transient, key_a).expect("first input"),
            InputSpec::clear(b, Proof::Transient, key_b).expect("second input"),
        ];
        let output = native_output(
            account.address_at(util::RECEIVING, 4).expect("address"),
            GENESIS_QTY + qty_b - 1_000,
        );
        let unsigned =
            build_transfer(&inputs, &[output], 1_000, header(), limits(), &mut rng).expect("build");
        (unsigned, ids)
    };

    let (unsigned, ids) = transfer();
    let tx = sign(unsigned, &[key_a, key_b]).expect("sign");
    let (_, log) = publish(&tx);
    assert_eq!(inputs(&log), ids, "both inputs, in input order");
    assert_eq!(fees(&log), vec![1_000]);

    // The control. `sign_multi` checks only that the key count matches, so
    // the swapped list still produces a signature — it just is not one over
    // the (key, contract) pairs the VM recorded.
    let (unsigned, _) = transfer();
    let tx = sign(unsigned, &[key_b, key_a]).expect("sign_multi only counts keys");
    assert!(
        tx.verify(limits()).is_err(),
        "keys out of input order must not authorize the transfer"
    );
}
