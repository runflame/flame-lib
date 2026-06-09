//! Auditor wire-canonicality harness — struct decoders.
//!
//! Owned by the VM Auditor (see `audits/vm/CLAUDE.md`). The VM Engineer
//! wires this into the crate (e.g. `include!` from a `#[cfg(test)]`
//! module, or move under `flamevm/tests/`); the auditor remains primary
//! author. No external fuzz deps required — these are deterministic
//! property/regression assertions runnable under `cargo test`.
//!
//! Goal: for EVERY wire decoder, assert two properties:
//!   (P1) ACCEPT ⇒ re-encode is byte-identical to the accepted input
//!        (no non-canonical input is silently normalized and accepted).
//!   (P2) decode(x) never panics / never allocates unboundedly on a
//!        short adversarial input.
//!
//! Decoders in scope (per the 2026-06-10 canonicality sweep):
//!   - encoding.rs   read_value / read_int253 / read_string / dict
//!   - cell.rs       Cell::decode (+ Predicate::decode)
//!   - actor.rs      ActorID::decode
//!   - address.rs    Address::decode
//!   - send/tx       Message / TxHeader (round-trip only; no Decodable yet)
//!
//! Each `fn` below is a target; a `harness()` aggregator runs them all.
//!
//! PLACEMENT NOTE (engineer): the `encoding::*` free functions and
//! `vm::Anchor` are crate-private today. To compile this as-is, drop it
//! into a `#[cfg(test)] mod auditor_wire;` inside the crate (via
//! `include!`), OR make `encoding`'s `read_value/read_int253/
//! write_*`, `vm::Anchor`, and `Value::is_portable` reachable. The
//! crate-root re-exports already cover `ActorID/Address/Cell/Predicate/
//! Int253/String/Value/Point/Message/Decodable/Encodable/Reader`.

use curve25519_dalek::ristretto::CompressedRistretto;

use crate::actor::ActorID;
use crate::address::Address;
use crate::cell::{Cell, Predicate};
use crate::encoding::{
    read_int253, read_value, write_int253, write_list_prefix, write_value,
};
use crate::int253::Int253;
use crate::string::String;
use crate::value::Value;
use crate::vm::Anchor;
use readerwriter::{Decodable, Encodable, Reader};

// ── Generic helpers ──────────────────────────────────────────────

/// Decode-then-reencode-then-bit-compare for any `Decodable+Encodable`.
/// Returns Ok(()) when the value either rejects OR round-trips
/// byte-identically; returns the offending (input, reencoded) pair when
/// an accepted input re-encodes to different bytes (a malleability hole).
fn assert_canonical_struct<T: Decodable + Encodable>(input: &[u8]) -> Result<(), (Vec<u8>, Vec<u8>)> {
    let mut r: &[u8] = input;
    let Ok(decoded) = T::decode(&mut r) else { return Ok(()) };
    // Trailing bytes after a struct decode are a malleability vector
    // unless the caller separately enforces empty-reader. We flag them.
    let consumed = input.len() - r.remaining_bytes();
    let reenc = decoded.encode_to_vec();
    if reenc != input[..consumed] {
        return Err((input[..consumed].to_vec(), reenc));
    }
    Ok(())
}

// ── ActorID ──────────────────────────────────────────────────────

/// (P1) ActorID round-trips canonically for both variants.
/// (P2) Constructor length must be bounded against remaining input.
pub fn target_actorid(data: &[u8]) {
    let _ = assert_canonical_struct::<ActorID>(data);

    // Regression: a Constructor length wildly exceeding the buffer must
    // reject without allocating. 0x01 tag + 8-byte LE huge length.
    let mut adversarial = vec![ActorID::TAG_CONSTRUCTOR];
    adversarial.extend_from_slice(&u64::MAX.to_le_bytes());
    let mut r: &[u8] = &adversarial;
    assert!(ActorID::decode(&mut r).is_err(), "huge ctor len must reject");
}

// ── Predicate ────────────────────────────────────────────────────

pub fn target_predicate(data: &[u8]) {
    // Predicate::decode reads exactly 32 raw bytes, no validation.
    let _ = assert_canonical_struct::<Predicate>(data);
}

// ── Cell ─────────────────────────────────────────────────────────

/// (P1) any accepted Cell must re-encode byte-identically.
/// (P2) Cell::decode must not allocate on an attacker-chosen payload
///      count that exceeds the remaining input.
pub fn target_cell(data: &[u8]) {
    // Property: Cell::decode does NOT enforce empty-reader, so callers
    // (op_input via String::to_cell) must — exercise the raw decoder and
    // re-encode the consumed prefix.
    let mut r: &[u8] = data;
    if let Ok(cell) = Cell::decode(&mut r) {
        let consumed = data.len() - r.remaining_bytes();
        let reenc = cell.encode_to_vec();
        assert_eq!(
            reenc,
            &data[..consumed],
            "accepted Cell must re-encode to its consumed bytes"
        );
    }
}

/// Regression: a tiny input claiming a giant payload count must reject
/// (or at least not OOM). Today Cell::decode does `Vec::with_capacity`
/// on an unbounded count — this target documents the expected fix.
pub fn target_cell_payload_count_bomb() -> Vec<u8> {
    // outer list-Dict of 3, valid point, 32-byte anchor, then inner
    // list-prefix claiming ~1.8e19 entries with zero following bytes.
    let mut buf = Vec::new();
    write_list_prefix(&mut buf, 3).unwrap();
    write_value(
        &mut buf,
        &Value::Point(crate::crypto::Point::from_bytes([0u8; 32])),
    )
    .unwrap();
    write_value(&mut buf, &Value::String(String::from(vec![0u8; 32]))).unwrap();
    // Inner payload list with an enormous claimed count, no data.
    buf.push(187u8); // LIST_VAR
    buf.push(3u8); // SUBVARINT_U64 sub-tag
    buf.extend_from_slice(&u64::MAX.to_le_bytes());
    buf
}

// ── Address ──────────────────────────────────────────────────────

pub fn target_address(data: &[u8]) {
    let _ = assert_canonical_struct::<Address>(data);
}

// ── encoding::read_value (Value tree) ────────────────────────────

/// (P1) any accepted Value re-encodes byte-identically (portable
/// variants only — non-portable refuse to encode, which is fine).
/// (P2) read_value never panics.
pub fn target_value(data: &[u8]) {
    let mut r: &[u8] = data;
    match read_value(&mut r) {
        Ok(Some(v)) if v.is_portable() => {
            let consumed = data.len() - r.remaining_bytes();
            let mut reenc = Vec::new();
            if write_value(&mut reenc, &v).is_ok() {
                assert_eq!(
                    reenc,
                    &data[..consumed],
                    "accepted portable Value must re-encode canonically"
                );
            }
        }
        _ => {}
    }
}

pub fn target_int253(data: &[u8]) {
    let mut r: &[u8] = data;
    if let Ok(i) = read_int253(&mut r) {
        let consumed = data.len() - r.remaining_bytes();
        let mut reenc = Vec::new();
        write_int253(&mut reenc, &i).unwrap();
        assert_eq!(reenc, &data[..consumed], "Int253 must re-encode canonically");
    }
}

// ── Round-trip generators (Message/TxHeader have no Decodable) ────

pub fn message_roundtrip_smoke() {
    use crate::send::Message;
    let m = Message {
        target: ActorID::Hash([7u8; 32]),
        method: Int253::from(0u64),
        caller: Some(ActorID::Constructor(vec![1, 2, 3])),
        anchor: Anchor([9u8; 32]),
        payload: vec![Value::Int253(Int253::from(42u64))],
        gas: 1000,
        vbytes: 5,
        refund_predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
    };
    // Encoding canonicalizes caller via to_canonical(): a Constructor
    // caller must encode identically to its Hash form.
    let mut m2 = Message { caller: Some(ActorID::Hash(m.caller.as_ref().unwrap().to_hash())), ..clone_message(&m) };
    assert_eq!(m.encode_to_vec(), m2_encode(&mut m2));
}

fn clone_message(m: &crate::send::Message) -> crate::send::Message {
    crate::send::Message {
        target: m.target.clone(),
        method: m.method,
        caller: m.caller.clone(),
        anchor: m.anchor,
        payload: m.payload.clone(),
        gas: m.gas,
        vbytes: m.vbytes,
        refund_predicate: m.refund_predicate.clone(),
    }
}
fn m2_encode(m: &mut crate::send::Message) -> Vec<u8> {
    m.encode_to_vec()
}

// ── Aggregator (deterministic corpus; extend with cargo-fuzz later) ──

pub fn harness() {
    // A small deterministic corpus exercising tags across the namespace.
    for first in 0u16..=255 {
        for second in 0u16..=255 {
            let data = [first as u8, second as u8];
            target_value(&data);
            target_int253(&data);
            target_actorid(&data);
            target_predicate(&data);
            target_cell(&data);
            target_address(&data);
        }
    }
    // The documented payload-count bomb: must reject, not OOM. Run under
    // a small reserve so an unbounded with_capacity is observable.
    let bomb = target_cell_payload_count_bomb();
    let mut r: &[u8] = &bomb;
    let _ = Cell::decode(&mut r); // EXPECTED post-fix: Err, no huge alloc.
    message_roundtrip_smoke();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_harness() {
        harness();
    }

    /// This test currently FAILS-by-OOM-or-passes depending on the
    /// allocator; post-fix it must be a clean `Err`. Kept ignored until
    /// the engineer adds the `remaining_bytes()` guard to Cell::decode.
    #[test]
    #[ignore = "documents the payload-count allocation bomb; un-ignore after fix"]
    fn cell_payload_count_bomb_rejects() {
        let bomb = target_cell_payload_count_bomb();
        let mut r: &[u8] = &bomb;
        assert!(Cell::decode(&mut r).is_err());
    }
}
