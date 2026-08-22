//! Outbound messages and MessageID identity.

use merlin::Transcript;
use readerwriter::{Encodable, WriteError, Writer};

use crate::actor::ActorID;
use crate::cell::Predicate;
use crate::encoding::write_admitted_value;
use crate::errors::VMError;
use crate::value::Value;
use crate::vm::Anchor;

/// Unique identity of a scheduled internal transaction (a queued `Send`).
/// Uniqueness is inherited from the embedded `anchor`: each send consumes a
/// unique-anchored split, so two distinct sends always carry distinct MessageIDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageID(pub [u8; 32]);

impl MessageID {
    /// Returns the underlying 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One queued outbound message. Built by `op_send`, embedded directly
/// in `TxEntry::Send(Message)`, drained by the consensus layer after
/// external-tx execution. `refund_predicate` is the sender-chosen unlock
/// predicate for the Cell emitted directly by consensus when delivery fails.
#[derive(Clone, Debug)]
pub struct Message {
    /// Destination actor (either `Hash` for an already-deployed actor,
    /// or `Constructor` for transparent on-the-fly deployment). Encoding
    /// preserves the variant because constructor code must reach first
    /// delivery, so Hash and Constructor forms have different MessageIDs.
    pub target: ActorID,

    /// Originating actor's id if this send was emitted by an internal
    /// tx; `None` if it was emitted by an external tx (the external
    /// sender has no actor identity). Canonicalized during encoding so
    /// equivalent constructor-form caller ids commit identically.
    pub caller: Option<ActorID>,

    /// Anchor ratcheted from the current tx's chain right before the
    /// `TxEntry::Send` was appended. Provides uniqueness for the
    /// MessageID hash. Also seeds the resulting internal tx's
    /// `last_anchor`.
    pub anchor: Anchor,

    /// Args to deliver to the method's stack. Walked at delivery time
    /// and pushed in payload order before the dispatched method runs.
    payload: Vec<Value>,

    /// Gas allotment committed by the originator. Fully consumed
    /// regardless of internal-tx outcome; asynchronous sends have no
    /// caller frame to refund.
    pub gas: u64,

    /// Sender-chosen bounce predicate. If the delivered internal tx
    /// fails, consensus seals `payload` into a fresh Cell under this
    /// predicate and emits it as an Output effect.
    pub refund_predicate: Predicate,
}

impl Message {
    /// Constructs a message, rejecting values that cannot cross into the
    /// asynchronous execution domain.
    pub fn new(
        target: ActorID,
        caller: Option<ActorID>,
        anchor: Anchor,
        payload: Vec<Value>,
        gas: u64,
        refund_predicate: Predicate,
    ) -> Result<Self, VMError> {
        if payload.iter().any(|v| !v.is_portable()) {
            return Err(VMError::NonPortableInSend);
        }
        Ok(Self {
            target,
            caller,
            anchor,
            payload,
            gas,
            refund_predicate,
        })
    }

    /// Borrows the immutable payload.
    pub fn payload(&self) -> &[Value] {
        &self.payload
    }

    /// Consumes the message and returns its payload.
    pub fn into_payload(self) -> Vec<Value> {
        self.payload
    }

    /// Canonical message length without allocating an encoded buffer.
    pub fn encoded_size(&self) -> usize {
        let mut size = readerwriter::SizeWriter::new();
        self.encode(&mut size)
            .expect("admitted message is encodable");
        size.len()
    }

    /// Unique ID identifying the message that spawns the internal transaction.
    /// Note: MessageID is not the same as TxID, which can only be determined after
    /// processing the message.
    pub fn id(&self) -> MessageID {
        let buf = self.encode_to_vec();
        let mut t = Transcript::new(b"flamevm.message.id");
        t.append_message(b"send", &buf);
        let mut h = [0u8; 32];
        t.challenge_bytes(b"id", &mut h);
        MessageID(h)
    }
}

/// Canonical wire form. Field order:
///
/// 1. `anchor` — 32 raw bytes (via `Anchor: Encodable`).
/// 2. `target` — `ActorID::encode`, preserving Hash or Constructor form.
/// 3. `caller` — `0x00` for None, `0x01 ‖ ActorID::encode(canonical)`
///    for Some.
/// 4. `refund_predicate` — 32-byte compressed Ristretto (via
///    `Predicate: Encodable`).
/// 5. `gas` — little-endian u64.
/// 6. `payload` — little-endian u64 count, then each value's
///    canonical `write_value` encoding.
///
/// Fails only if the writer runs out of capacity. Portability is enforced by
/// [`Message::new`] before a payload enters the asynchronous domain.
impl Encodable for Message {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        self.anchor.encode(w)?;
        // Preserve Constructor bytes: they are the code needed for
        // deploy-on-first-delivery. Registry lookup still canonicalizes the id.
        self.target.encode(w)?;
        match &self.caller {
            None => w.write_u8(b"send.caller.tag", 0)?,
            Some(c) => {
                w.write_u8(b"send.caller.tag", 1)?;
                c.to_canonical().encode(w)?;
            }
        }
        self.refund_predicate.encode(w)?;
        w.write_u64(b"send.gas", self.gas)?;
        w.write_u64(b"send.payload.len", self.payload.len() as u64)?;
        for v in &self.payload {
            write_admitted_value(w, v)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use curve25519_dalek::ristretto::CompressedRistretto;

    use crate::{ClearToken, Dict, Int253, FLAME_FLAVOR};

    fn dummy_predicate() -> Predicate {
        Predicate::opaque(CompressedRistretto([0u8; 32]))
    }

    fn fixture_message(anchor: Anchor) -> Message {
        Message::new(
            ActorID::Hash([0x11; 32]),
            None,
            anchor,
            Vec::new(),
            1_000,
            dummy_predicate(),
        )
        .expect("empty payload is portable")
    }

    #[test]
    fn sendid_preserves_constructor_code() {
        // Constructor bytes must survive the queued message so first delivery
        // can deploy code. Registry identity still canonicalizes separately.
        let ctor = ActorID::Constructor(vec![0x42, 0x42, 0x42]);
        let hash_form = ActorID::Hash(ctor.to_hash());
        let mut m1 = fixture_message(Anchor([0x99; 32]));
        let mut m2 = fixture_message(Anchor([0x99; 32]));
        m1.target = ctor;
        m2.target = hash_form;
        assert_ne!(m1.id(), m2.id(), "constructor witness must be committed");
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
    fn new_rejects_nested_nonportable_payload() {
        let mut inner = Dict::new();
        inner.insert(
            Int253::ZERO,
            Value::ClearToken(ClearToken::new(Int253::from(-1i64), FLAME_FLAVOR)),
        );
        let mut outer = Dict::new();
        outer.insert(Int253::ZERO, Value::Dict(inner));

        assert!(matches!(
            Message::new(
                ActorID::Hash([0x11; 32]),
                None,
                Anchor([0x22; 32]),
                vec![Value::Dict(outer)],
                1_000,
                dummy_predicate(),
            ),
            Err(VMError::NonPortableInSend)
        ));
    }
}
