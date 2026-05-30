//! Routing addresses: spend predicates and actor message targets.

use readerwriter::{Decodable, Encodable, ReadError, Reader, WriteError, Writer};

use crate::actor::ActorID;
use crate::cell::Predicate;
use crate::crypto::Point;
use crate::dict::Dict;
use crate::encoding::{
    read_list_prefix, read_string, read_value, write_list_prefix, write_value,
};
use crate::int253::Int253;
use crate::string::String;
use crate::value::Value;

/// Routing target. See module docs for context.
#[derive(Debug)]
pub enum Address {
    /// Spend authority: the target is a [`Predicate`] gating a
    /// cell. Used by `op_output` (and helpers) that pay to a
    /// key/script address. The script consumer satisfies the
    /// predicate via signature or reveal to unlock the cell later.
    Predicate(Predicate),

    /// Message routing target: deliver `args` to `dst.method` with
    /// the given gas allotment. The vbyte allotment lives outside
    /// the Address (in the enclosing `op_send` operand list) since
    /// it concerns the *send*, not the destination shape.
    MessageTarget {
        dst: ActorID,
        method: Int253,
        args: Dict,
        gas: u64,
    },
}

impl Address {
    /// Tag value for the [`Address::Predicate`] variant on the wire.
    pub const TAG_PREDICATE: u8 = 0x00;
    /// Tag value for the [`Address::MessageTarget`] variant on the wire.
    pub const TAG_MESSAGE_TARGET: u8 = 0x01;

    /// Convenience: encode to a fresh `Vec<u8>`. Returns the
    /// `WriteError` from `Encodable::encode` (a non-portable Dict in
    /// `args` is encoded as `InsufficientCapacity` via `try_clone`).
    pub fn to_bytes(&self) -> Result<Vec<u8>, WriteError> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }
}

/// Canonical wire form: a list-Dict whose first entry is the tag
/// byte (as `Int253`), remaining entries the variant payload. See
/// `Address::TAG_*` constants.
///
/// Layout:
/// - `Predicate`: `[tag, point]`.
/// - `MessageTarget`: `[tag, dst_string, method_int, args_dict, gas_int]`.
///
/// `dst` is wrapped as a `String` carrying the bytes of
/// [`ActorID::encode`] so the outer list-Dict reader sees a uniform
/// value-typed payload.
impl Encodable for Address {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        match self {
            Address::Predicate(p) => {
                write_list_prefix(w, 2)?;
                write_value(
                    w,
                    &Value::Int253(Int253::from(Self::TAG_PREDICATE as u64)),
                )?;
                write_value(w, &Value::Point(Point::from_compressed(p.to_point())))
            }
            Address::MessageTarget {
                dst,
                method,
                args,
                gas,
            } => {
                write_list_prefix(w, 5)?;
                write_value(
                    w,
                    &Value::Int253(Int253::from(Self::TAG_MESSAGE_TARGET as u64)),
                )?;
                let mut dst_bytes = Vec::new();
                dst.encode(&mut dst_bytes)?;
                write_value(w, &Value::String(String::from(dst_bytes)))?;
                write_value(w, &Value::Int253(*method))?;
                // `try_clone` is cheap on portable Dicts; a Dict
                // with non-copyable entries (e.g. a linear Token
                // inside a payload arg) errors at encode time.
                let args_clone = args
                    .try_clone()
                    .map_err(|_| WriteError::InsufficientCapacity)?;
                write_value(w, &Value::Dict(args_clone))?;
                write_value(w, &Value::Int253(Int253::from(*gas)))
            }
        }
    }
}

/// Reads the canonical wire form. Pure parse; strict on shape
/// (rejects unknown tag/arity, non-32-byte point, non-canonical
/// scalar/string/dict, negative gas). VM-level callers that want
/// `VMError::MalformedAddress` map the `ReadError` themselves.
impl Decodable for Address {
    fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        let count = read_list_prefix(r).map_err(|_| ReadError::InvalidFormat)?;
        let tag_val = read_value(r).map_err(|_| ReadError::InvalidFormat)?;
        let tag = match tag_val {
            Some(Value::Int253(i)) => {
                let bytes = i.to_bytes();
                // Reject if anything beyond the low byte is set
                // (so a future signed/large tag can't sneak past).
                if bytes[1..].iter().any(|b| *b != 0) {
                    return Err(ReadError::InvalidFormat);
                }
                bytes[0]
            }
            _ => return Err(ReadError::InvalidFormat),
        };
        match (tag, count) {
            (Self::TAG_PREDICATE, 2) => {
                let p = match read_value(r) {
                    Ok(Some(Value::Point(p))) => Predicate::opaque(p.to_compressed()),
                    _ => return Err(ReadError::InvalidFormat),
                };
                Ok(Address::Predicate(p))
            }
            (Self::TAG_MESSAGE_TARGET, 5) => {
                let dst_bytes = read_string(r)?;
                let mut dst_r = dst_bytes.as_slice();
                let dst = ActorID::decode(&mut dst_r)?;
                if !dst_r.is_empty() {
                    return Err(ReadError::InvalidFormat);
                }
                let method = match read_value(r) {
                    Ok(Some(Value::Int253(i))) => i,
                    _ => return Err(ReadError::InvalidFormat),
                };
                let args = match read_value(r) {
                    Ok(Some(Value::Dict(d))) => d,
                    _ => return Err(ReadError::InvalidFormat),
                };
                let gas = match read_value(r) {
                    Ok(Some(Value::Int253(i))) => {
                        if i.is_negative() {
                            return Err(ReadError::InvalidFormat);
                        }
                        i.to_u64().ok_or(ReadError::InvalidFormat)?
                    }
                    _ => return Err(ReadError::InvalidFormat),
                };
                Ok(Address::MessageTarget {
                    dst,
                    method,
                    args,
                    gas,
                })
            }
            _ => Err(ReadError::InvalidFormat),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use curve25519_dalek::ristretto::CompressedRistretto;

    #[test]
    fn address_decode_rejects_unknown_tag() {
        // Build a list-Dict of two values: tag=99, dummy point.
        let mut bytes = Vec::new();
        write_list_prefix(&mut bytes, 2).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(99u64))).unwrap();
        write_value(
            &mut bytes,
            &Value::Point(Point::from_compressed(CompressedRistretto([0u8; 32]))),
        )
        .unwrap();
        let mut r = bytes.as_slice();
        let err = Address::decode(&mut r).expect_err("must error");
        assert!(matches!(err, ReadError::InvalidFormat));
    }

    #[test]
    fn address_decode_rejects_wrong_arity_for_message() {
        // tag = MessageTarget but only 3 entries instead of 5.
        let mut bytes = Vec::new();
        write_list_prefix(&mut bytes, 3).unwrap();
        write_value(
            &mut bytes,
            &Value::Int253(Int253::from(Address::TAG_MESSAGE_TARGET as u64)),
        )
        .unwrap();
        write_value(
            &mut bytes,
            &Value::String(String::from(vec![ActorID::TAG_HASH; 33])),
        )
        .unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        let mut r = bytes.as_slice();
        let err = Address::decode(&mut r).expect_err("must error");
        assert!(matches!(err, ReadError::InvalidFormat));
    }

    #[test]
    fn address_decode_rejects_negative_gas() {
        let addr = Address::MessageTarget {
            dst: ActorID::Hash([0u8; 32]),
            method: Int253::from(0u64),
            args: Dict::new(),
            gas: 0,
        };
        let mut bytes = addr.to_bytes().expect("encode");
        // Bytes ends with the encoded gas (Int253 from 0u64). Replace
        // it with a fresh -1 Int253. To keep the surgery simple, just
        // append a fresh list-Dict instead with the negative tail:
        bytes.clear();
        write_list_prefix(&mut bytes, 5).unwrap();
        write_value(
            &mut bytes,
            &Value::Int253(Int253::from(Address::TAG_MESSAGE_TARGET as u64)),
        )
        .unwrap();
        write_value(
            &mut bytes,
            &Value::String(String::from(ActorID::Hash([0u8; 32]).encode_to_vec())),
        )
        .unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        write_value(&mut bytes, &Value::Dict(Dict::new())).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(-1i64))).unwrap();
        let mut r = bytes.as_slice();
        let err = Address::decode(&mut r).expect_err("must error");
        assert!(matches!(err, ReadError::InvalidFormat));
    }

    #[test]
    fn address_decode_rejects_trailing_bytes_inside_dst() {
        // Encode the message target; tamper with the dst-string
        // bytes to add a trailing byte after the ActorID.
        let mut bytes = Vec::new();
        write_list_prefix(&mut bytes, 5).unwrap();
        write_value(
            &mut bytes,
            &Value::Int253(Int253::from(Address::TAG_MESSAGE_TARGET as u64)),
        )
        .unwrap();
        // dst string contains a valid Hash ActorID + 1 trailing byte.
        let mut dst_payload = ActorID::Hash([1u8; 32]).encode_to_vec();
        dst_payload.push(0xff);
        write_value(&mut bytes, &Value::String(String::from(dst_payload))).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        write_value(&mut bytes, &Value::Dict(Dict::new())).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        let mut r = bytes.as_slice();
        let err = Address::decode(&mut r).expect_err("must error");
        assert!(matches!(err, ReadError::InvalidFormat));
    }
}
