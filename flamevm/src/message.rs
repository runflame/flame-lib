//! Outbound messages and MessageID identity.

use cells::{CellBuilder, CellDecode, CellEncode, CellError, CellRef, CellResolver, CellSlice};

use crate::actor::ActorID;
use crate::contract::Predicate;
use crate::errors::VMError;
use crate::value::Value;
use crate::vm::Anchor;
use crate::{Dict, Scalar};

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
/// predicate for the Contract emitted directly by consensus when delivery fails.
#[derive(Clone, Debug)]
pub struct Message {
    /// Destination actor (either `Hash` for an already-deployed actor,
    /// or `Constructor` for transparent on-the-fly deployment). Encoding
    /// preserves the variant because constructor code must reach first
    /// delivery, so Hash and Constructor forms have different MessageIDs.
    pub target: ActorID,

    /// Originating actor's id when the sending frame has actor authority.
    /// `None` means no authenticated actor principal: this includes
    /// ExternalRoot and ContractOpen sends. Canonicalized during encoding so
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
    /// fails, consensus seals `payload` into a fresh Contract under this
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
        let message = Self {
            target,
            caller,
            anchor,
            payload,
            gas,
            refund_predicate,
        };
        // Admission must establish encodability before id()/TxLog hashing can
        // assume it: portable values may still exceed the typed depth bound.
        message.to_cell()?;
        Ok(message)
    }

    /// Borrows the immutable payload.
    pub fn payload(&self) -> &[Value] {
        &self.payload
    }

    /// Consumes the message and returns its payload.
    pub fn into_payload(self) -> Vec<Value> {
        self.payload
    }

    /// Canonical standalone Cell-envelope length, including resident bodies.
    pub fn encoded_size(&self) -> usize {
        self.to_envelope()
            .expect("admitted message is encodable")
            .encode()
            .len()
    }

    /// Unique ID identifying the message that spawns the internal transaction.
    /// Note: MessageID is not the same as TxID, which can only be determined after
    /// processing the message.
    pub fn id(&self) -> MessageID {
        MessageID(self.to_cell().expect("admitted message is encodable").id())
    }
}

/// Canonical wire form. Field order:
///
/// 1. `anchor` — 32 raw bytes.
/// 2. `target` — `ActorID::encode`, preserving Hash or Constructor form.
/// 3. `caller` — `0x00` for None, `0x01 ‖ canonical actor hash`
///    for Some.
/// 4. `refund_predicate` — 32-byte compressed Ristretto.
/// 5. `gas` — little-endian u64.
/// 6. `payload` — reference to a Dict with consecutive keys `0..k`.
///
/// Portability is enforced by
/// [`Message::new`] before a payload enters the asynchronous domain.
impl CellEncode for Message {
    fn encode(&self, w: &mut CellBuilder) -> Result<(), CellError> {
        w.store(&self.anchor)?;
        // Preserve Constructor bytes: they are the code needed for
        // deploy-on-first-delivery. Registry lookup still canonicalizes the id.
        w.store(&self.target)?;
        match &self.caller {
            None => {
                w.store_u8(0)?;
            }
            Some(c) => {
                w.store_u8(1)?.store_bytes(&c.to_hash())?;
            }
        }
        w.store(&self.refund_predicate)?.store_u64(self.gas)?;
        w.store_ref(CellRef::resident(
            Dict::from_values(self.payload.clone()).to_cell()?,
        ))?;
        Ok(())
    }
}

impl CellDecode for Message {
    fn decode<R: CellResolver + ?Sized>(
        s: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        let anchor = Anchor::decode(s, cells)?;
        let target = ActorID::decode(s, cells)?;
        let caller = match s.load_u8()? {
            0 => None,
            1 => Some(ActorID::Hash(<[u8; 32]>::decode(s, cells)?)),
            _ => return Err(CellError::InvalidFormat),
        };
        let refund = Predicate::decode(s, cells)?;
        let gas = s.load_u64()?;
        let root = cells::resolve_cell(cells, &s.load_ref()?)?;
        let mut dict = Dict::from_cell(&root, cells)?;
        let mut payload = Vec::new();
        for i in 0..dict.len() {
            payload.push(
                dict.remove_resolved(&Scalar::from(i as u64), cells)?
                    .ok_or(CellError::InvalidFormat)?,
            );
        }
        Self::new(target, caller, anchor, payload, gas, refund)
            .map_err(|_| CellError::InvalidFormat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use curve25519_dalek::ristretto::CompressedRistretto;

    use crate::{ClearToken, Dict, Scalar, FLAME_FLAVOR};

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
            Scalar::ZERO,
            Value::ClearToken(ClearToken::new(Scalar::from(-1i64), FLAME_FLAVOR)),
        );
        let mut outer = Dict::new();
        outer.insert(Scalar::ZERO, Value::Dict(inner));

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

    #[test]
    fn new_rejects_unencodable_depth_before_message_identity() {
        let mut value = Value::Scalar(Scalar::ONE);
        for _ in 0..=crate::encoding::MAX_VALUE_DEPTH {
            value = Value::Dict(Dict::from_values(vec![value]));
        }
        assert!(value.is_portable());
        assert!(matches!(
            Message::new(
                ActorID::Hash([0x11; 32]),
                None,
                Anchor([0x22; 32]),
                vec![value],
                1_000,
                dummy_predicate(),
            ),
            Err(VMError::Cell(CellError::LimitExceeded))
        ));
    }
}
