//! Routing addresses: spend predicates and actor message targets.

use readerwriter::{Reader, WriteError, Writer};

use crate::actor::{ActorID, MethodKey};
use crate::cell::Predicate;
use crate::crypto::Point;
use crate::dict::Dict;
use crate::encoding::{
    read_list_prefix, read_value, write_list_prefix, write_value,
};
use crate::errors::VMError;
use crate::int253::Int253;
use crate::string::String;
use crate::value::Value;

/// Routing target. See module docs for context.
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
        method: MethodKey,
        args: Dict,
        gas: u64,
    },
}

impl Address {
    /// Tag value for the [`Address::Predicate`] variant on the wire.
    pub const TAG_PREDICATE: u8 = 0x00;
    /// Tag value for the [`Address::MessageTarget`] variant on the wire.
    pub const TAG_MESSAGE_TARGET: u8 = 0x01;

    /// Writes the canonical wire form: a list-Dict whose first
    /// entry is the tag byte (as `Int253`) and whose remaining
    /// entries are the variant payload.
    ///
    /// Layout:
    /// - `Predicate`: `[tag, point]`.
    /// - `MessageTarget`: `[tag, dst_string, method_int, args_dict, gas_int]`.
    ///
    /// `dst` is wrapped as a `String` carrying the bytes of
    /// [`ActorID::encode`] so the outer list-Dict reader sees a
    /// uniform value-typed payload. The same convention applies
    /// downstream when scripts build addresses by hand.
    pub fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
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
                write_value(w, &Value::Int253(*method.as_int()))?;
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

    /// Reads the canonical wire form. Strict: rejects any deviation
    /// from the expected per-variant shape.
    pub fn decode(r: &mut impl Reader) -> Result<Self, VMError> {
        let count = read_list_prefix(r).map_err(|_| VMError::MalformedAddress)?;
        // First entry: tag byte as Int253.
        let tag_val =
            read_value(r).map_err(|_| VMError::MalformedAddress)?;
        let tag = match tag_val {
            Some(Value::Int253(i)) => {
                // We only accept tag values that fit in a single byte.
                let bytes = i.to_bytes();
                // Reject if anything beyond the low byte is set (so a
                // future signed/large tag can't sneak past).
                if bytes[1..].iter().any(|b| *b != 0) {
                    return Err(VMError::MalformedAddress);
                }
                bytes[0]
            }
            _ => return Err(VMError::MalformedAddress),
        };
        match (tag, count) {
            (Self::TAG_PREDICATE, 2) => {
                let p = match read_value(r) {
                    Ok(Some(Value::Point(p))) => Predicate::Opaque(p.inner),
                    _ => return Err(VMError::MalformedAddress),
                };
                Ok(Address::Predicate(p))
            }
            (Self::TAG_MESSAGE_TARGET, 5) => {
                // dst: String of ActorID bytes.
                let dst_bytes = match read_value(r) {
                    Ok(Some(Value::String(s))) => s.to_bytes(),
                    _ => return Err(VMError::MalformedAddress),
                };
                let mut dst_r = dst_bytes.as_slice();
                let dst = ActorID::decode(&mut dst_r)
                    .map_err(|_| VMError::MalformedAddress)?;
                if !dst_r.is_empty() {
                    return Err(VMError::MalformedAddress);
                }
                // method: Int253.
                let method = match read_value(r) {
                    Ok(Some(Value::Int253(i))) => MethodKey::from(i),
                    _ => return Err(VMError::MalformedAddress),
                };
                // args: Dict.
                let args = match read_value(r) {
                    Ok(Some(Value::Dict(d))) => d,
                    _ => return Err(VMError::MalformedAddress),
                };
                // gas: Int253 → u64 (must fit, must be non-negative).
                let gas = match read_value(r) {
                    Ok(Some(Value::Int253(i))) => {
                        if i.is_negative() {
                            return Err(VMError::MalformedAddress);
                        }
                        i.to_u64().ok_or(VMError::MalformedAddress)?
                    }
                    _ => return Err(VMError::MalformedAddress),
                };
                Ok(Address::MessageTarget {
                    dst,
                    method,
                    args,
                    gas,
                })
            }
            _ => Err(VMError::MalformedAddress),
        }
    }

    /// Convenience: encode to a fresh `Vec<u8>`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, WriteError> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }
}

/// Manual `Debug` — `Dict` payloads and the `Predicate` inner
/// variants don't compose with derive in all branches.
impl core::fmt::Debug for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Address::Predicate(p) => f
                .debug_tuple("Address::Predicate")
                .field(p.to_point().as_bytes())
                .finish(),
            Address::MessageTarget {
                dst, method, args, gas,
            } => f
                .debug_struct("Address::MessageTarget")
                .field("dst", dst)
                .field("method", method)
                .field("args.len", &args.len())
                .field("gas", gas)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use curve25519_dalek::ristretto::CompressedRistretto;

    fn fake_predicate() -> Predicate {
        Predicate::Opaque(CompressedRistretto([0x7e; 32]))
    }

    #[test]
    fn address_predicate_encode_decode_roundtrip() {
        let addr = Address::Predicate(fake_predicate());
        let bytes = addr.to_bytes().expect("encode");
        let mut r = bytes.as_slice();
        let back = Address::decode(&mut r).expect("decode");
        match back {
            Address::Predicate(p) => assert_eq!(p.to_point().as_bytes(), &[0x7e; 32]),
            _ => panic!("variant mismatch"),
        }
        assert!(r.is_empty(), "no trailing bytes");
    }

    #[test]
    fn address_message_target_encode_decode_roundtrip() {
        let mut args = Dict::new();
        args.insert(Int253::from(0u64), Value::Int253(Int253::from(7u64)));
        args.insert(Int253::from(1u64), Value::String(String::from(b"hi".to_vec())));

        let addr = Address::MessageTarget {
            dst: ActorID::Hash([0x11; 32]),
            method: MethodKey::from(3u64),
            args,
            gas: 10_000,
        };
        let bytes = addr.to_bytes().expect("encode");
        let mut r = bytes.as_slice();
        let back = Address::decode(&mut r).expect("decode");
        match back {
            Address::MessageTarget {
                dst, method, args, gas,
            } => {
                assert_eq!(dst, ActorID::Hash([0x11; 32]));
                assert_eq!(method, MethodKey::from(3u64));
                assert_eq!(args.len(), 2);
                assert_eq!(gas, 10_000);
            }
            _ => panic!("variant mismatch"),
        }
        assert!(r.is_empty(), "no trailing bytes");
    }

    #[test]
    fn address_message_target_with_constructor_dst() {
        let addr = Address::MessageTarget {
            dst: ActorID::Constructor(vec![0xde, 0xad]),
            method: MethodKey::from(0u64),
            args: Dict::new(),
            gas: 0,
        };
        let bytes = addr.to_bytes().expect("encode");
        let mut r = bytes.as_slice();
        let back = Address::decode(&mut r).expect("decode");
        match back {
            Address::MessageTarget { dst, .. } => {
                assert_eq!(dst, ActorID::Constructor(vec![0xde, 0xad]));
            }
            _ => panic!("variant mismatch"),
        }
    }

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
        assert!(matches!(err, VMError::MalformedAddress));
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
        assert!(matches!(err, VMError::MalformedAddress));
    }

    #[test]
    fn address_decode_rejects_negative_gas() {
        let addr = Address::MessageTarget {
            dst: ActorID::Hash([0u8; 32]),
            method: MethodKey::from(0u64),
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
            &Value::String(String::from(ActorID::Hash([0u8; 32]).to_bytes())),
        )
        .unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        write_value(&mut bytes, &Value::Dict(Dict::new())).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(-1i64))).unwrap();
        let mut r = bytes.as_slice();
        let err = Address::decode(&mut r).expect_err("must error");
        assert!(matches!(err, VMError::MalformedAddress));
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
        let mut dst_payload = ActorID::Hash([1u8; 32]).to_bytes();
        dst_payload.push(0xff);
        write_value(&mut bytes, &Value::String(String::from(dst_payload))).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        write_value(&mut bytes, &Value::Dict(Dict::new())).unwrap();
        write_value(&mut bytes, &Value::Int253(Int253::from(0u64))).unwrap();
        let mut r = bytes.as_slice();
        let err = Address::decode(&mut r).expect_err("must error");
        assert!(matches!(err, VMError::MalformedAddress));
    }
}
