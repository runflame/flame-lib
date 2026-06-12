//! Outbound message sends and SendID identity.

use merlin::Transcript;
use readerwriter::{Encodable, WriteError, Writer};

use crate::actor::ActorID;
use crate::cell::Predicate;
use crate::encoding::write_value;
use crate::value::Value;
use crate::vm::Anchor;

/// Unique identity of a scheduled internal transaction (a queued `Send`).
/// Uniqueness is inherited from the embedded `anchor`: each send consumes a
/// unique-anchored split, so two distinct sends always carry distinct SendIDs.
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
#[derive(Debug)]
pub struct Message {
    /// Destination actor (either `Hash` for an already-deployed actor,
    /// or `Constructor` for transparent on-the-fly deployment per Q4).
    /// `id()` canonicalizes both forms via `to_canonical()` so a send
    /// targeted by hash and a send targeted by constructor bytes that
    /// hash to the same id produce the same SendID.
    pub target: ActorID,


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
    /// Unique ID identifying the message that spawns the internal transaction.
    /// Note: SendID is not the same as TxID, which can only be determined after
    /// processing the message.
    pub fn id(&self) -> SendID {
        let buf = self.encode_to_vec();
        let mut t = Transcript::new(b"flamevm.send.id");
        t.append_message(b"send", &buf);
        let mut h = [0u8; 32];
        t.challenge_bytes(b"id", &mut h);
        SendID(h)
    }
}

/// Canonical wire form. Field order:
///
/// 1. `anchor` — 32 raw bytes (via `Anchor: Encodable`).
/// 2. `target` — canonical `ActorID::encode` of `to_canonical()` so
///    Hash and Constructor forms of the same actor produce identical
///    bytes.
/// 3. `caller` — `0x00` for None, `0x01 ‖ ActorID::encode(canonical)`
///    for Some.
/// 4. `method` — `write_int253`.
/// 5. `refund_predicate` — 32-byte compressed Ristretto (via
///    `Predicate: Encodable`).
/// 6. `gas` — little-endian u64.
/// 7. `vbytes` — little-endian u64.
/// 8. `payload` — little-endian u64 count, then each value's
///    canonical `write_value` encoding.
///
/// Fails only if the underlying writer runs out of capacity —
/// payload portability is guaranteed by `op_send`, so `write_value`
/// never errors here in valid flow.
impl Encodable for Message {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        self.anchor.encode(w)?;
        self.target.to_canonical().encode(w)?;
        match &self.caller {
            None => w.write_u8(b"send.caller.tag", 0)?,
            Some(c) => {
                w.write_u8(b"send.caller.tag", 1)?;
                c.to_canonical().encode(w)?;
            }
        }
        self.refund_predicate.encode(w)?;
        w.write_u64(b"send.gas", self.gas)?;
        w.write_u64(b"send.vbytes", self.vbytes)?;
        w.write_u64(b"send.payload.len", self.payload.len() as u64)?;
        for v in &self.payload {
            write_value(w, v).map_err(|_| WriteError::InsufficientCapacity)?;
        }
        Ok(())
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
            caller: None,
            anchor,
            payload: Vec::new(),
            gas: 1_000,
            vbytes: 0,
            refund_predicate: dummy_predicate(),
        }
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
}
