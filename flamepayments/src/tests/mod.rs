//! Tests for the wallet crate, and the fixtures they share.

mod builder;
mod keys;
mod note;
mod roundtrip;

use flamekd::ReceivingAddress;
use flamevm::{
    CellEncode, Contract, ContractID, ExternalTx, Limits, TxEntry, TxHeader, TxLog, FLAME_FLAVOR,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::builder::OutputSpec;
use crate::keys::SpendAccount;
use crate::note::{open_note, outputs_with_notes, ReceivedNote};

/// The header every test transaction uses.
pub(crate) fn header() -> TxHeader {
    TxHeader {
        version: 1,
        locktime: 0,
    }
}

/// The gas budget every test transaction uses.
pub(crate) fn limits() -> Limits {
    Limits { gas: 10_000_000 }
}

/// The generator a test draws every `r` from. Seeded, so a failing test
/// replays exactly; one per test, threaded through every transfer it builds,
/// because a transfer rebuilt from a re-seeded generator would reuse `r`.
pub(crate) fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

/// A native-flavor output of `qty` sparks to `address`, with no memo.
pub(crate) fn native_output(address: ReceivingAddress, qty: u64) -> OutputSpec {
    OutputSpec {
        address,
        qty,
        flv: FLAME_FLAVOR,
        memo: Vec::new(),
    }
}

/// The one output a log created under `address`'s predicate, with its note.
/// A transfer sorts its outputs, so an output is found by what it pays,
/// never by its position.
pub(crate) fn output_to<'a>(
    log: &'a TxLog,
    address: &ReceivingAddress,
) -> (&'a Contract, Option<&'a [u8]>) {
    let point = address.spending_key().compress();
    let mut found = outputs_with_notes(log)
        .into_iter()
        .filter(|(contract, _)| contract.predicate.to_point() == point);
    let output = found.next().expect("an output pays the address");
    assert!(found.next().is_none(), "one output pays the address");
    output
}

/// The output a log paid to `m/…/branch/n`, opened the way its recipient
/// opens it: from the note that follows it.
pub(crate) fn receive(
    log: &TxLog,
    account: &SpendAccount,
    branch: u32,
    n: u32,
) -> (Contract, ReceivedNote) {
    let address = account.address_at(branch, n).expect("address");
    let (contract, note) = output_to(log, &address);
    let received = open_note(
        contract,
        note,
        &address,
        &account.viewing_key_at(branch, n).expect("viewing key"),
    )
    .expect("the recipient opens its note");
    (contract.clone(), received)
}

/// Encode, decode and verify — the verifier's view of a built transaction,
/// and the bytes that actually travel.
pub(crate) fn publish(tx: &ExternalTx) -> (Vec<u8>, TxLog) {
    let bytes = tx.to_envelope().expect("envelope").encode();
    let decoded =
        ExternalTx::from_bytes_bounded(&bytes, 1, tx.script.len(), tx.proof_bytes().len())
            .expect("decode");
    let log = decoded.verify(limits()).expect("verify");
    (bytes, log)
}

/// The contracts a log created, in order. Entries are counted by variant:
/// every external log begins `Header, CellWitness(BoCID)`, so the length of
/// `entries()` says nothing on its own.
pub(crate) fn outputs(log: &TxLog) -> Vec<Contract> {
    log.entries()
        .iter()
        .filter_map(|entry| match entry {
            TxEntry::Output(contract) => Some(contract.clone()),
            _ => None,
        })
        .collect()
}

/// The contracts a log spent, in order.
pub(crate) fn inputs(log: &TxLog) -> Vec<ContractID> {
    log.entries()
        .iter()
        .filter_map(|entry| match entry {
            TxEntry::Input(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// The fees a log paid.
pub(crate) fn fees(log: &TxLog) -> Vec<u64> {
    log.entries()
        .iter()
        .filter_map(|entry| match entry {
            TxEntry::Fee(sparks) => Some(*sparks),
            _ => None,
        })
        .collect()
}
