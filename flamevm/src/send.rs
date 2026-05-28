//! Outbound message sends and SendID identity.

use merlin::Transcript;
use readerwriter::{WriteError, Writer};

use crate::actor::ActorID;
use crate::cell::Predicate;
use crate::encoding::{write_int253, write_value};
use crate::int253::Int253;
use crate::value::Value;
use crate::vm::Anchor;

/// Deterministic identity for a scheduled internal transaction —
/// the canonical 32-byte hash of the Send's canonical wire encoding.
/// Analogous to `Cell::id()` for the `Output` effect: the id uniquely
/// names the send and commits to every parameter the resulting
/// internal tx will be delivered with.
///
/// Uniqueness is inherited from the embedded `anchor` (each send
/// consumes a unique-anchored split, so two distinct sends from any
/// tx always carry different anchors → different SendIDs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SendID(pub [u8; 32]);

impl SendID {
    /// Returns the underlying 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One queued outbound message. Built by `op_send`, embedded directly
/// in `TxEntry::Send(Message)`, drained by the consensus layer after
/// external-tx commit. Mirrors `spec.md` §Messages with one addition:
/// `refund_predicate`, the sender-chosen unlock predicate for the
/// bounce cell produced when the delivered internal tx fails (Q3 —
/// failure path emits an Output directly via consensus, not a fresh
/// sub-VM).
pub struct Message {
    /// Destination actor (either `Hash` for an already-deployed actor,
    /// or `Constructor` for transparent on-the-fly deployment per Q4).
    /// `id()` canonicalizes both forms via `to_canonical()` so a send
    /// targeted by hash and a send targeted by constructor bytes that
    /// hash to the same id produce the same SendID.
    pub target: ActorID,

    /// Which method on `target` to dispatch. `Int253(0)` is `recv`,
    /// the only method an external sender can target.
    pub method: Int253,

    /// Originating actor's id if this send was emitted by an internal
    /// tx; `None` if it was emitted by an external tx (the external
    /// sender has no actor identity). Canonicalized in `id()` the same
    /// way as `target`.
    pub caller: Option<ActorID>,

    /// Anchor ratcheted from the external tx's chain right before the
    /// `TxEntry::Send` was appended. Provides uniqueness for the
    /// SendID hash. Also seeds the resulting internal tx's
    /// `last_anchor`.
    pub anchor: Anchor,

    /// Args to deliver to the method's stack. Walked at delivery time
    /// and pushed in payload order before the dispatched method runs.
    pub payload: Vec<Value>,

    /// Gas allotment committed by the originator. Fully consumed
    /// regardless of internal-tx outcome (no refunds for sends per
    /// spec.md).
    pub gas: u64,

    /// Vbyte allotment delivered to the target on success — used to
    /// fund storage. Restores a frozen actor (per ADR 0005: top-up
    /// resets the counter and clears `frozen_since`).
    pub vbytes: u64,

    /// Sender-chosen bounce predicate. If the delivered internal tx
    /// fails, consensus seals `payload` into a fresh cell under this
    /// predicate and emits the cell as an Output effect (Q3).
    pub refund_predicate: Predicate,
}

impl Message {
    /// Writes the Message's canonical wire form into `w`, in the
    /// order used by `id()`. Field order:
    ///
    /// 1. `anchor` — 32 raw bytes.
    /// 2. `target` — `ActorID::encode` of `to_canonical()` so Hash
    ///    and Constructor forms of the same actor produce identical
    ///    bytes.
    /// 3. `caller` — `0x00` for None, `0x01 ‖ ActorID::encode(canonical)`
    ///    for Some.
    /// 4. `method` — `write_int253`.
    /// 5. `refund_predicate` — 32-byte compressed Ristretto.
    /// 6. `gas` — little-endian u64.
    /// 7. `vbytes` — little-endian u64.
    /// 8. `payload` — little-endian u64 count, then each value's
    ///    canonical `write_value` encoding.
    ///
    /// Fails only if the underlying writer runs out of capacity —
    /// payload portability is guaranteed by `op_send`, so
    /// `write_value` never errors here in valid flow.
    pub fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        w.write(b"send.anchor", &self.anchor.0)?;
        self.target.to_canonical().encode(w)?;
        match &self.caller {
            None => w.write_u8(b"send.caller.tag", 0)?,
            Some(c) => {
                w.write_u8(b"send.caller.tag", 1)?;
                c.to_canonical().encode(w)?;
            }
        }
        write_int253(w, &self.method)?;
        w.write(
            b"send.refund_predicate",
            self.refund_predicate.to_point().as_bytes(),
        )?;
        w.write_u64(b"send.gas", self.gas)?;
        w.write_u64(b"send.vbytes", self.vbytes)?;
        w.write_u64(b"send.payload.len", self.payload.len() as u64)?;
        for v in &self.payload {
            // `Vec<u8>` writer never errors; payload portability
            // (gated at op_send) guarantees each value encodes.
            write_value(w, v).map_err(|_| WriteError::InsufficientCapacity)?;
        }
        Ok(())
    }

    /// Computes the canonical [`SendID`] for this message — the
    /// 32-byte hash of `encode()`'s output under a Merlin transcript
    /// labelled `flamevm.send.id`. Analogous to `Cell::id()` for
    /// cells: one wire encoding, one hash, one identity.
    pub fn id(&self) -> SendID {
        let mut buf = Vec::new();
        self.encode(&mut buf)
            .expect("Vec<u8> writer never fails and payload portability is gated at op_send");
        let mut t = Transcript::new(b"flamevm.send.id");
        t.append_message(b"send", &buf);
        let mut h = [0u8; 32];
        t.challenge_bytes(b"id", &mut h);
        SendID(h)
    }
}

/// Manual `Debug` — `Predicate::Opaque` carries a 32-byte point;
/// printing the full payload keeps the impl simple. Payload values
/// are skipped (linear-type variants don't derive Debug uniformly).
impl core::fmt::Debug for Message {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Message")
            .field("target", &self.target)
            .field("method", &self.method)
            .field("caller", &self.caller)
            .field("anchor", &self.anchor)
            .field("payload.len", &self.payload.len())
            .field("gas", &self.gas)
            .field("vbytes", &self.vbytes)
            .field("refund_predicate.point", self.refund_predicate.to_point().as_bytes())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use curve25519_dalek::ristretto::CompressedRistretto;

    fn dummy_predicate() -> Predicate {
        Predicate::opaque(CompressedRistretto([0u8; 32]))
    }

    fn fixture_message(anchor: Anchor) -> Message {
        Message {
            target: ActorID::Hash([0x11; 32]),
            method: Int253::from(0u64),
            caller: None,
            anchor,
            payload: Vec::new(),
            gas: 1_000,
            vbytes: 0,
            refund_predicate: dummy_predicate(),
        }
    }

    #[test]
    fn sendid_is_not_just_the_anchor() {
        // SendID hashes the whole send; it must NOT be byte-equal
        // to the anchor (that would be the old broken design).
        let anchor = Anchor([0x42; 32]);
        let id = fixture_message(anchor).id();
        assert_ne!(id.as_bytes(), &anchor.0);
    }

    #[test]
    fn sendid_is_deterministic_for_identical_messages() {
        let a = fixture_message(Anchor([0x99; 32])).id();
        let b = fixture_message(Anchor([0x99; 32])).id();
        assert_eq!(a, b);
    }

    #[test]
    fn sendid_diverges_when_anchor_differs() {
        let a = fixture_message(Anchor([0x01; 32])).id();
        let b = fixture_message(Anchor([0x02; 32])).id();
        assert_ne!(a, b);
    }

    #[test]
    fn sendid_diverges_when_target_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.target = ActorID::Hash([0xaa; 32]);
        m2.target = ActorID::Hash([0xbb; 32]);
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_diverges_when_method_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.method = Int253::from(7u64);
        m2.method = Int253::from(11u64);
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_diverges_when_gas_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.gas = 1_000;
        m2.gas = 2_000;
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_diverges_when_vbytes_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.vbytes = 0;
        m2.vbytes = 100;
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_diverges_when_caller_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.caller = None;
        m2.caller = Some(ActorID::Hash([0xcc; 32]));
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_diverges_when_refund_predicate_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.refund_predicate = Predicate::opaque(CompressedRistretto([0u8; 32]));
        m2.refund_predicate = Predicate::opaque(CompressedRistretto([0xff; 32]));
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_diverges_when_payload_differs() {
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.payload = vec![Value::Int253(Int253::from(7u64))];
        m2.payload = vec![Value::Int253(Int253::from(8u64))];
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn sendid_canonical_actor_form() {
        // SendID canonicalizes target via `to_canonical()` so the same
        // actor accessed via `Hash` vs `Constructor` variant produces
        // the same SendID.
        let ctor = ActorID::Constructor(vec![0x42, 0x42, 0x42]);
        let hash_form = ActorID::Hash(ctor.to_hash());
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.target = ctor;
        m2.target = hash_form;
        assert_eq!(m1.id(), m2.id(), "canonical actor form must dominate");
    }

    #[test]
    fn sendid_canonical_caller_form() {
        // Same canonicalization for caller.
        let ctor = ActorID::Constructor(vec![0x11, 0x22, 0x33]);
        let hash_form = ActorID::Hash(ctor.to_hash());
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.caller = Some(ctor);
        m2.caller = Some(hash_form);
        assert_eq!(m1.id(), m2.id(), "canonical caller form must dominate");
    }

    #[test]
    fn encode_is_deterministic() {
        let m = fixture_message(Anchor([0x55; 32]));
        let mut a = Vec::new();
        let mut b = Vec::new();
        m.encode(&mut a).unwrap();
        m.encode(&mut b).unwrap();
        assert_eq!(a, b);
        // SendID = hash(domain ‖ encode()) — sanity: not empty, not the anchor.
        assert!(!a.is_empty());
        assert_ne!(&a[..32], &fixture_message(Anchor([0x66; 32])).id().0);
    }
}
