//! Tests for the wallet crate, and the fixtures they share.

mod builder;
mod keys;
mod roundtrip;

use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamevm::{
    CellEncode, Contract, ContractID, ExternalTx, Limits, Predicate, TxEntry, TxHeader, TxLog,
    FLAME_FLAVOR,
};

use crate::builder::{Opening, OutputSpec};

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

/// Deterministic blinding factors for output `index`. A transcript rather
/// than a random source, so a failing test replays exactly.
pub(crate) fn test_blinding(index: u64) -> (DalekScalar, DalekScalar) {
    let mut transcript = merlin::Transcript::new(b"flamepayments.test.blinding");
    transcript.append_u64(b"output", index);
    let mut qty = [0u8; 64];
    transcript.challenge_bytes(b"qty", &mut qty);
    let mut flv = [0u8; 64];
    transcript.challenge_bytes(b"flv", &mut flv);
    (
        DalekScalar::from_bytes_mod_order_wide(&qty),
        DalekScalar::from_bytes_mod_order_wide(&flv),
    )
}

/// A native-flavor output of `qty` sparks under `predicate`, together with
/// the opening its recipient needs in order to spend it.
pub(crate) fn native_output(predicate: Predicate, qty: u64, index: u64) -> (OutputSpec, Opening) {
    let (qty_blinding, flv_blinding) = test_blinding(index);
    (
        OutputSpec {
            predicate,
            qty,
            flv: FLAME_FLAVOR,
            qty_blinding,
            flv_blinding,
        },
        Opening {
            qty,
            flv: FLAME_FLAVOR,
            qty_blinding,
            flv_blinding,
        },
    )
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
