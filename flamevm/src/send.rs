//! Outbound message sends and SendID identity.

use crate::actor::ActorID;
use crate::int253::Int253;
use crate::cell::Predicate;
use crate::value::Value;
use crate::vm::Anchor;

/// Deterministic identity for a scheduled internal transaction.
/// Equal to the originating [`Message::anchor`] (the anchor
/// ratcheted from the external tx's chain just before the
/// `TxEntry::Send` is emitted), so this is known at external-tx
/// broadcast time and survives intact into the internal tx's
/// `Receive` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SendID(pub [u8; 32]);

impl SendID {
    /// Constructs a `SendID` from an `Anchor`. Stable lift — the
    /// SendID's bytes are exactly the anchor's bytes, but the
    /// distinct name keeps call sites readable when the same
    /// 32-byte value is being treated as a send-identity rather
    /// than an anchor-chain element.
    pub fn from_anchor(anchor: Anchor) -> Self {
        SendID(anchor.0)
    }

    /// Returns the underlying 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One queued outbound message. Built by `op_send`, drained by the
/// consensus layer after external-tx commit. The fields mirror
/// `spec.md` §Messages with one addition: `refund_predicate`, the
/// sender-chosen unlock predicate for the bounce cell produced when
/// the delivered internal tx fails (Q3 — failure path emits an
/// Output directly via consensus, not a fresh sub-VM).
pub struct Message {
    /// Destination actor (either Hash for an already-deployed
    /// actor, or Constructor for transparent on-the-fly deployment
    /// per Q4).
    pub target: ActorID,

    /// Which method on `target` to dispatch. `Int253(0)` is
    /// `recv`, the only method an external sender can target.
    pub method: Int253,

    /// Originating actor's id if this send was emitted by an
    /// internal tx; `None` if it was emitted by an external tx
    /// (the external sender has no actor identity).
    pub caller: Option<ActorID>,

    /// Anchor ratcheted from the external tx's chain right before
    /// the `TxEntry::Send` was appended. Per Q5 this is the
    /// SendID: deterministic at broadcast time and unique within
    /// the originating tx.
    pub anchor: Anchor,

    /// Args to deliver to the method's stack. Walked at delivery
    /// time and pushed in payload order before the dispatched
    /// method runs.
    pub payload: Vec<Value>,

    /// Gas allotment committed by the originator. Fully consumed
    /// regardless of internal-tx outcome (no refunds for sends per
    /// spec.md).
    pub gas: u64,

    /// Vbyte allotment delivered to the target on success — used
    /// to fund storage. Restores a frozen actor (per ADR 0005:
    /// top-up resets the counter and clears `frozen_since`).
    pub vbytes: u64,

    /// Sender-chosen bounce predicate. If the delivered internal
    /// tx fails, consensus seals `payload` into a fresh cell under
    /// this predicate and emits the cell as an Output effect (Q3).
    pub refund_predicate: Predicate,
}

impl Message {
    /// Computes the [`SendID`] for this message — simply lifts the
    /// stored anchor (per Q5: SendID == anchor for the send).
    pub fn id(&self) -> SendID {
        SendID::from_anchor(self.anchor)
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
        Predicate::Opaque(CompressedRistretto([0u8; 32]))
    }

    #[test]
    fn sendid_from_anchor_lifts_bytes() {
        let a = Anchor([0x42; 32]);
        let id = SendID::from_anchor(a);
        assert_eq!(id.as_bytes(), &[0x42; 32]);
    }

    #[test]
    fn message_id_equals_anchor() {
        let m = Message {
            target: ActorID::Hash([0x11; 32]),
            method: Int253::from(0u64),
            caller: None,
            anchor: Anchor([0x99; 32]),
            payload: Vec::new(),
            gas: 1_000,
            vbytes: 0,
            refund_predicate: dummy_predicate(),
        };
        assert_eq!(m.id(), SendID([0x99; 32]));
    }

    #[test]
    fn sendid_ord_matches_anchor_ord() {
        let a1 = SendID([0x00; 32]);
        let a2 = SendID([0x01; 32]);
        assert!(a1 < a2);
    }
}
