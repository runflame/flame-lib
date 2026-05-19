//! Canonical compact encoding for flamevm types.
//!
//! Each logical value has exactly one wire byte sequence. The first
//! byte is a type+width tag; within each type, width classes carve
//! the value range into disjoint, offset-based sub-ranges so that the
//! encoder has no choice about which tag to use and the decoder has
//! nothing (or very little) to enforce.
//!
//! ## Tag namespace
//!
//! ```text
//! 0..=58     Int positive immediate (value = tag)
//! 59         Int +U8     (1-byte payload b; value = 59 + b;  range 59..=314)
//! 60         Int +U32    (4-byte payload w; value = 315 + w; range 315..≈4.3e9)
//! 61         Int +U64    (8-byte payload w; value = 4_294_967_611 + w)
//! 62         Int +FULL   (32-byte canonical scalar; value > U64 range)
//! 63         Int -1
//! 64         Int -U8     (value = -(2 + b);   range -2..=-257)
//! 65         Int -U32    (value = -(258 + w))
//! 66         Int -U64    (value = -(4_294_967_554 + w))
//! 67         Int -FULL   (32-byte sign-magnitude; magnitude > U64 range)
//! 68..=126   Str immediate length (length = tag - 68; range 0..=58)
//! 127        Str VAR     (sub-varint v; length = 59 + v)
//! 128..=186  List immediate count (count = tag - 128; range 0..=58)
//! 187        List VAR    (sub-varint v; count = 59 + v)
//! 188..=246  Dict immediate count (count = tag - 188; range 0..=58)
//! 247        Dict VAR    (sub-varint v; count = 59 + v)
//! 248        Point (32-byte compressed Ristretto)
//! 249..=253  Token, ClearToken, WideToken, Object, Merlin (sizes TBD)
//! 254        reserved
//! 255        extension (sub-tag follows)
//! ```
//!
//! ## Sub-varint (used inside `STR_VAR` / `LIST_VAR` / `DICT_VAR`)
//!
//! Encodes a non-negative integer with exactly one byte sequence per
//! value, by offset-based disjoint ranges:
//!
//! ```text
//! sub-tag 0  1 LE byte    value = b              range 0..=255
//! sub-tag 1  2 LE bytes   value = 256 + w        range 256..=65_791
//! sub-tag 2  4 LE bytes   value = 65_792 + w     range 65_792..≈4.3e9
//! sub-tag 3  8 LE bytes   value = 4_295_032_608 + w   range up to ≈1.8e19
//! ```
//!
//! ## Canonicality checks performed at decode time
//!
//! Most non-canonical encodings are impossible by construction. Two
//! cases still require explicit checks:
//!
//! 1. `INT_PFULL` / `INT_NFULL`: the 32-byte payload encodes the
//!    value as-is (no offset). The decoder rejects if the value
//!    could have been encoded in a narrower width class.
//! 2. `DICT_*` payload whose keys turned out to be `0..n-1` must be
//!    rejected — that dict has a shorter `LIST_*` encoding.

use core::cmp::Ordering;
use core::convert::TryFrom;

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
pub use readerwriter::{ReadError, Reader, WriteError, Writer};

use crate::crypto::Point;
use crate::dict::Dict;
use crate::integer::Integer;
use crate::string::String;
use crate::value::Value;

// ── Tag constants ─────────────────────────────────────────────────

// Integers
const INT_IMM_MAX: u8 = 58;
const INT_PU8: u8 = 59;
const INT_PU32: u8 = 60;
const INT_PU64: u8 = 61;
const INT_PFULL: u8 = 62;
const INT_NEG1: u8 = 63;
const INT_NU8: u8 = 64;
const INT_NU32: u8 = 65;
const INT_NU64: u8 = 66;
const INT_NFULL: u8 = 67;

// Strings
const STR_IMM_MIN: u8 = 68;
const STR_IMM_MAX: u8 = 126;
const STR_VAR: u8 = 127;

// Lists
const LIST_IMM_MIN: u8 = 128;
const LIST_IMM_MAX: u8 = 186;
const LIST_VAR: u8 = 187;

// Dicts
const DICT_IMM_MIN: u8 = 188;
const DICT_IMM_MAX: u8 = 246;
const DICT_VAR: u8 = 247;

// Fixed-size types
const POINT_TAG: u8 = 248;
const TOKEN_TAG: u8 = 249;
const CLEAR_TOKEN_TAG: u8 = 250;
const WIDE_TOKEN_TAG: u8 = 251;
const OBJECT_TAG: u8 = 252;
const MERLIN_TAG: u8 = 253;
// Reserved: 254
// Extension: 255

// Number of IMM slots (values 0..=58) shared across int/str/list/dict.
const IMM_COUNT: u8 = INT_IMM_MAX + 1; // 59

// Integer width-class bases (the value encoded with a zero payload).
const PU8_BASE: u64 = 59;
const PU32_BASE: u64 = 315;
const PU64_BASE: u64 = 4_294_967_611; // PU32_BASE + 2^32

const NU8_BASE: u64 = 2;
const NU32_BASE: u64 = 258;
const NU64_BASE: u64 = 4_294_967_554; // NU32_BASE + 2^32

// Tops of each width class (max value encodable, inclusive).
const PU8_TOP: u64 = 314;
const PU32_TOP: u64 = 4_294_967_610;
const NU8_TOP: u64 = 257;
const NU32_TOP: u64 = 4_294_967_553;

// Container length/count beyond IMM range = IMM_COUNT + sub-varint.
const CONTAINER_VAR_BASE: u64 = IMM_COUNT as u64;

// Sub-varint sub-tags
const SUBVARINT_U8: u8 = 0;
const SUBVARINT_U16: u8 = 1;
const SUBVARINT_U32: u8 = 2;
const SUBVARINT_U64: u8 = 3;

const SUBVAR_U16_BASE: u64 = 256;
const SUBVAR_U32_BASE: u64 = 65_792; // 256 + 65536
const SUBVAR_U64_BASE: u64 = 4_295_033_088; // SUBVAR_U32_BASE + 2^32

// ── Sub-varint ────────────────────────────────────────────────────

fn write_subvarint(w: &mut impl Writer, n: u64) -> Result<(), WriteError> {
    if n <= u8::MAX as u64 {
        w.write_u8(b"subvarint.tag", SUBVARINT_U8)?;
        w.write_u8(b"subvarint.u8", n as u8)
    } else if n <= SUBVAR_U16_BASE + (u16::MAX as u64) {
        w.write_u8(b"subvarint.tag", SUBVARINT_U16)?;
        let payload = (n - SUBVAR_U16_BASE) as u16;
        w.write(b"subvarint.u16", &payload.to_le_bytes())
    } else if n <= SUBVAR_U32_BASE + (u32::MAX as u64) {
        w.write_u8(b"subvarint.tag", SUBVARINT_U32)?;
        let payload = (n - SUBVAR_U32_BASE) as u32;
        w.write(b"subvarint.u32", &payload.to_le_bytes())
    } else {
        w.write_u8(b"subvarint.tag", SUBVARINT_U64)?;
        let payload = n - SUBVAR_U64_BASE;
        w.write_u64(b"subvarint.u64", payload)
    }
}

fn read_subvarint(r: &mut impl Reader) -> Result<u64, ReadError> {
    let tag = r.read_u8()?;
    match tag {
        SUBVARINT_U8 => Ok(r.read_u8()? as u64),
        SUBVARINT_U16 => {
            let mut buf = [0u8; 2];
            r.read(&mut buf)?;
            Ok(SUBVAR_U16_BASE + (u16::from_le_bytes(buf) as u64))
        }
        SUBVARINT_U32 => Ok(SUBVAR_U32_BASE + (r.read_u32()? as u64)),
        SUBVARINT_U64 => Ok(SUBVAR_U64_BASE + r.read_u64()?),
        _ => Err(ReadError::InvalidFormat),
    }
}

// ── Integer encoding ──────────────────────────────────────────────

/// Writes an `Integer` in compact canonical form.
pub fn write_integer(w: &mut impl Writer, int: &Integer) -> Result<(), WriteError> {
    if int.is_negative() {
        write_negative_integer(w, int)
    } else {
        write_positive_integer(w, int)
    }
}

fn write_positive_integer(w: &mut impl Writer, int: &Integer) -> Result<(), WriteError> {
    if let Some(v) = int.to_u64() {
        if v <= INT_IMM_MAX as u64 {
            return w.write_u8(b"int.tag", v as u8);
        }
        if v <= PU8_TOP {
            w.write_u8(b"int.tag", INT_PU8)?;
            return w.write_u8(b"int.u8", (v - PU8_BASE) as u8);
        }
        if v <= PU32_TOP {
            w.write_u8(b"int.tag", INT_PU32)?;
            let payload = (v - PU32_BASE) as u32;
            return w.write(b"int.u32", &payload.to_le_bytes());
        }
        // v > PU32_TOP and fits in u64 → PU64 range (v - PU64_BASE fits u64).
        w.write_u8(b"int.tag", INT_PU64)?;
        return w.write_u64(b"int.u64", v - PU64_BASE);
    }

    // Doesn't fit u64. Might still fit PU64 if value <= PU64_BASE + u64::MAX.
    let pu64_top = Integer::from(PU64_BASE) + Integer::from(u64::MAX);
    if int.cmp(&pu64_top) != Ordering::Greater {
        let payload_int = *int - Integer::from(PU64_BASE);
        let payload = payload_int
            .to_u64()
            .expect("invariant: int - PU64_BASE fits u64 when int <= pu64_top");
        w.write_u8(b"int.tag", INT_PU64)?;
        return w.write_u64(b"int.u64", payload);
    }

    // FULL: write the 32-byte canonical scalar as-is.
    w.write_u8(b"int.tag", INT_PFULL)?;
    w.write(b"int.full", &int.to_bytes())
}

fn write_negative_integer(w: &mut impl Writer, int: &Integer) -> Result<(), WriteError> {
    let abs = int.abs();
    if abs == Integer::one() {
        return w.write_u8(b"int.tag", INT_NEG1);
    }
    if let Some(mag) = abs.to_u64() {
        if mag <= NU8_TOP {
            w.write_u8(b"int.tag", INT_NU8)?;
            return w.write_u8(b"int.u8", (mag - NU8_BASE) as u8);
        }
        if mag <= NU32_TOP {
            w.write_u8(b"int.tag", INT_NU32)?;
            let payload = (mag - NU32_BASE) as u32;
            return w.write(b"int.u32", &payload.to_le_bytes());
        }
        w.write_u8(b"int.tag", INT_NU64)?;
        return w.write_u64(b"int.u64", mag - NU64_BASE);
    }

    let nu64_top_mag = Integer::from(NU64_BASE) + Integer::from(u64::MAX);
    if abs.cmp(&nu64_top_mag) != Ordering::Greater {
        let payload_int = abs - Integer::from(NU64_BASE);
        let payload = payload_int
            .to_u64()
            .expect("invariant: magnitude - NU64_BASE fits u64 when abs <= nu64_top_mag");
        w.write_u8(b"int.tag", INT_NU64)?;
        return w.write_u64(b"int.u64", payload);
    }

    // NFULL: write 32-byte sign-magnitude as-is.
    w.write_u8(b"int.tag", INT_NFULL)?;
    w.write(b"int.full", &int.to_bytes())
}

/// Reads a compact-encoded `Integer`.
pub fn read_integer(r: &mut impl Reader) -> Result<Integer, ReadError> {
    let tag = r.read_u8()?;
    read_integer_with_tag(r, tag)
}

fn read_integer_with_tag(r: &mut impl Reader, tag: u8) -> Result<Integer, ReadError> {
    match tag {
        0..=INT_IMM_MAX => Ok(Integer::from(tag as u64)),
        INT_PU8 => {
            let b = r.read_u8()?;
            Ok(Integer::from(PU8_BASE + (b as u64)))
        }
        INT_PU32 => {
            let w = r.read_u32()? as u64;
            Ok(Integer::from(PU32_BASE + w))
        }
        INT_PU64 => {
            let w = r.read_u64()?;
            Ok(Integer::from(PU64_BASE) + Integer::from(w))
        }
        INT_PFULL => read_positive_full(r),
        INT_NEG1 => Ok(Integer::from(-1i64)),
        INT_NU8 => {
            let b = r.read_u8()?;
            Ok(Integer::from_parts(true, Scalar::from(NU8_BASE + (b as u64))))
        }
        INT_NU32 => {
            let w = r.read_u32()? as u64;
            Ok(Integer::from_parts(true, Scalar::from(NU32_BASE + w)))
        }
        INT_NU64 => {
            let w = r.read_u64()?;
            let mag = Integer::from(NU64_BASE) + Integer::from(w);
            Ok(-mag)
        }
        INT_NFULL => read_negative_full(r),
        _ => Err(ReadError::InvalidFormat),
    }
}

fn read_positive_full(r: &mut impl Reader) -> Result<Integer, ReadError> {
    let buf = r.read_u8x32()?;
    let int = Integer::from_bytes(buf).ok_or(ReadError::InvalidFormat)?;
    // Sign bit must be clear for a positive FULL.
    if int.is_negative() {
        return Err(ReadError::InvalidFormat);
    }
    // Canonicality: value must exceed the PU64 range.
    let pu64_top = Integer::from(PU64_BASE) + Integer::from(u64::MAX);
    if int.cmp(&pu64_top) != Ordering::Greater {
        return Err(ReadError::InvalidFormat);
    }
    Ok(int)
}

fn read_negative_full(r: &mut impl Reader) -> Result<Integer, ReadError> {
    let buf = r.read_u8x32()?;
    let int = Integer::from_bytes(buf).ok_or(ReadError::InvalidFormat)?;
    // Sign bit must be set for a negative FULL.
    if !int.is_negative() {
        return Err(ReadError::InvalidFormat);
    }
    // Canonicality: magnitude must exceed the NU64 range.
    let nu64_top_mag = Integer::from(NU64_BASE) + Integer::from(u64::MAX);
    if int.abs().cmp(&nu64_top_mag) != Ordering::Greater {
        return Err(ReadError::InvalidFormat);
    }
    Ok(int)
}

// ── String encoding ───────────────────────────────────────────────

/// Writes a `String` (byte-string) in canonical compact form.
pub fn write_string(w: &mut impl Writer, s: &String) -> Result<(), WriteError> {
    let len = s.as_bytes().len() as u64;
    if len <= INT_IMM_MAX as u64 {
        w.write_u8(b"str.tag", STR_IMM_MIN + len as u8)?;
    } else {
        w.write_u8(b"str.tag", STR_VAR)?;
        write_subvarint(w, len - CONTAINER_VAR_BASE)?;
    }
    w.write(b"str.data", s.as_bytes())
}

/// Reads a compact-encoded `String`.
pub fn read_string(r: &mut impl Reader) -> Result<String, ReadError> {
    let tag = r.read_u8()?;
    read_string_with_tag(r, tag)
}

fn read_string_with_tag(r: &mut impl Reader, tag: u8) -> Result<String, ReadError> {
    let len: u64 = match tag {
        STR_IMM_MIN..=STR_IMM_MAX => (tag - STR_IMM_MIN) as u64,
        STR_VAR => CONTAINER_VAR_BASE + read_subvarint(r)?,
        _ => return Err(ReadError::InvalidFormat),
    };
    let len = usize::try_from(len).map_err(|_| ReadError::InvalidFormat)?;
    // Bound against remaining input before delegating to the reader. This is
    // defense-in-depth: the default `read_bytes` impl already pre-checks
    // bounds, but custom `Reader` impls may not — and a malicious length
    // shouldn't be able to trigger a giant allocation regardless.
    if len > r.remaining_bytes() {
        return Err(ReadError::InvalidFormat);
    }
    let data = r.read_bytes(len)?;
    Ok(String::from(data))
}

// ── List / Dict length encoding ───────────────────────────────────

/// Writes the tag + count prefix for a list.
pub fn write_list_prefix(w: &mut impl Writer, count: usize) -> Result<(), WriteError> {
    let n = count as u64;
    if n <= INT_IMM_MAX as u64 {
        w.write_u8(b"list.tag", LIST_IMM_MIN + n as u8)
    } else {
        w.write_u8(b"list.tag", LIST_VAR)?;
        write_subvarint(w, n - CONTAINER_VAR_BASE)
    }
}

/// Reads the count from a list tag (with optional sub-varint).
pub fn read_list_prefix(r: &mut impl Reader) -> Result<usize, ReadError> {
    let tag = r.read_u8()?;
    read_list_count_with_tag(r, tag)
}

fn read_list_count_with_tag(r: &mut impl Reader, tag: u8) -> Result<usize, ReadError> {
    let n: u64 = match tag {
        LIST_IMM_MIN..=LIST_IMM_MAX => (tag - LIST_IMM_MIN) as u64,
        LIST_VAR => CONTAINER_VAR_BASE + read_subvarint(r)?,
        _ => return Err(ReadError::InvalidFormat),
    };
    usize::try_from(n).map_err(|_| ReadError::InvalidFormat)
}

/// Writes the tag + count prefix for a dict.
pub fn write_dict_prefix(w: &mut impl Writer, count: usize) -> Result<(), WriteError> {
    let n = count as u64;
    if n <= INT_IMM_MAX as u64 {
        w.write_u8(b"dict.tag", DICT_IMM_MIN + n as u8)
    } else {
        w.write_u8(b"dict.tag", DICT_VAR)?;
        write_subvarint(w, n - CONTAINER_VAR_BASE)
    }
}

/// Reads the count from a dict tag (with optional sub-varint).
pub fn read_dict_prefix(r: &mut impl Reader) -> Result<usize, ReadError> {
    let tag = r.read_u8()?;
    read_dict_count_with_tag(r, tag)
}

fn read_dict_count_with_tag(r: &mut impl Reader, tag: u8) -> Result<usize, ReadError> {
    let n: u64 = match tag {
        DICT_IMM_MIN..=DICT_IMM_MAX => (tag - DICT_IMM_MIN) as u64,
        DICT_VAR => CONTAINER_VAR_BASE + read_subvarint(r)?,
        _ => return Err(ReadError::InvalidFormat),
    };
    usize::try_from(n).map_err(|_| ReadError::InvalidFormat)
}

// ── Value encoding ────────────────────────────────────────────────

fn keys_are_sequential<'a>(keys: impl Iterator<Item = &'a Integer>) -> bool {
    keys.enumerate().all(|(i, k)| *k == Integer::from(i as u64))
}

/// Maximum nesting depth for `read_value`. Each list/dict entry counts
/// as one level. Bounds stack use and prevents DoS via deeply nested input.
const MAX_DEPTH: u32 = 64;

/// Reads a `Value`.
/// Returns `Ok(None)` for recognized but unimplemented type tags
/// (tokens, objects, merlin, etc.).
/// Returns `Err` for malformed data, non-canonical encodings, or
/// resource-exhausting input (excessive nesting, oversized counts).
pub fn read_value(r: &mut impl Reader) -> Result<Option<Value>, ReadError> {
    read_value_with_depth(r, 0)
}

fn read_value_with_depth(
    r: &mut impl Reader,
    depth: u32,
) -> Result<Option<Value>, ReadError> {
    if depth >= MAX_DEPTH {
        return Err(ReadError::InvalidFormat);
    }
    let tag = r.read_u8()?;
    match tag {
        // Integers
        0..=INT_NFULL => {
            let int = read_integer_with_tag(r, tag)?;
            Ok(Some(Value::Int(int)))
        }
        // Strings
        STR_IMM_MIN..=STR_VAR => {
            let s = read_string_with_tag(r, tag)?;
            Ok(Some(Value::String(s)))
        }
        // List-style dict (sequential keys 0..n-1)
        LIST_IMM_MIN..=LIST_VAR => {
            let count = read_list_count_with_tag(r, tag)?;
            // Each list element is at least 1 byte; reject counts that
            // can't possibly fit in the remaining input.
            if count > r.remaining_bytes() {
                return Err(ReadError::InvalidFormat);
            }
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                match read_value_with_depth(r, depth + 1)? {
                    Some(v) => values.push(v),
                    None => return Ok(None),
                }
            }
            Ok(Some(Value::Dict(Dict::from_values(values))))
        }
        // Dict-style (explicit keys)
        DICT_IMM_MIN..=DICT_VAR => {
            let count = read_dict_count_with_tag(r, tag)?;
            // Each dict entry is at least 2 bytes (key tag + value tag).
            // Division avoids overflow on attacker-supplied counts.
            if count > r.remaining_bytes() / 2 {
                return Err(ReadError::InvalidFormat);
            }
            let mut entries: Vec<(Integer, Value)> = Vec::with_capacity(count);
            let mut last_key: Option<Integer> = None;
            for _ in 0..count {
                let key = read_integer(r)?;
                // Reject duplicate or out-of-order keys (canonicality).
                if let Some(prev) = &last_key {
                    if key.cmp(prev) != Ordering::Greater {
                        return Err(ReadError::InvalidFormat);
                    }
                }
                last_key = Some(key);
                match read_value_with_depth(r, depth + 1)? {
                    Some(v) => entries.push((key, v)),
                    None => return Ok(None),
                }
            }
            // Canonicality: reject dict-style payloads whose keys ended up as
            // 0..n-1 — they have a shorter list-style encoding.
            if keys_are_sequential(entries.iter().map(|(k, _)| k)) {
                return Err(ReadError::InvalidFormat);
            }
            Ok(Some(Value::Dict(Dict::from_entries_unchecked(entries))))
        }
        // Point
        POINT_TAG => {
            let buf = r.read_u8x32()?;
            Ok(Some(Value::Point(Point::from_compressed(
                CompressedRistretto(buf),
            ))))
        }
        // Unimplemented types — payload size unknown, signal to caller.
        TOKEN_TAG | CLEAR_TOKEN_TAG | WIDE_TOKEN_TAG | OBJECT_TAG | MERLIN_TAG => Ok(None),
        // Reserved and extension tags are not yet defined.
        _ => Err(ReadError::InvalidFormat),
    }
}

/// Writes a `Dict`. Uses the list-style encoding when keys are
/// sequential 0, 1, 2, ...; otherwise uses the dict-style encoding.
pub fn write_dict(w: &mut impl Writer, dict: &Dict) -> Result<(), WriteError> {
    if keys_are_sequential(dict.entries().map(|(k, _)| k)) {
        write_list_prefix(w, dict.len())?;
        for (_, v) in dict.entries() {
            write_value(w, v)?;
        }
    } else {
        write_dict_prefix(w, dict.len())?;
        for (k, v) in dict.entries() {
            write_integer(w, k)?;
            write_value(w, v)?;
        }
    }
    Ok(())
}

/// Writes a `Value`. Returns `Err(WriteError)` for unimplemented types.
pub fn write_value(w: &mut impl Writer, val: &Value) -> Result<(), WriteError> {
    match val {
        Value::Int(i) => write_integer(w, i),
        Value::String(s) => write_string(w, s),
        Value::Dict(d) => write_dict(w, d),
        Value::Point(p) => {
            w.write_u8(b"point.tag", POINT_TAG)?;
            w.write(b"point.data", p.as_bytes())
        }
        _ => Err(WriteError::InsufficientCapacity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_int(int: Integer) -> Integer {
        let mut buf = Vec::new();
        write_integer(&mut buf, &int).unwrap();
        let mut r = buf.as_slice();
        read_integer(&mut r).unwrap()
    }

    fn encode_int(int: Integer) -> Vec<u8> {
        let mut buf = Vec::new();
        write_integer(&mut buf, &int).unwrap();
        buf
    }

    // ── Integer encoding: width-class selection ───────────────────

    #[test]
    fn int_immediate_contiguous() {
        for v in 0u64..=58 {
            let buf = encode_int(Integer::from(v));
            assert_eq!(buf.len(), 1, "value {} should be 1 byte", v);
            assert_eq!(buf[0], v as u8);
            assert_eq!(roundtrip_int(Integer::from(v)), Integer::from(v));
        }
    }

    #[test]
    fn int_pu8_range() {
        // Just above immediate (59), middle (180), top (314).
        for (val, payload) in [(59u64, 0u8), (180, 121), (314, 255)] {
            let buf = encode_int(Integer::from(val));
            assert_eq!(buf, vec![INT_PU8, payload], "value {}", val);
            assert_eq!(roundtrip_int(Integer::from(val)), Integer::from(val));
        }
    }

    #[test]
    fn int_pu32_range() {
        for val in [315u64, 1_000, 70_000, 4_294_967_610] {
            let buf = encode_int(Integer::from(val));
            assert_eq!(buf[0], INT_PU32);
            assert_eq!(buf.len(), 5);
            assert_eq!(roundtrip_int(Integer::from(val)), Integer::from(val));
        }
        // Tag-payload bijection at base.
        let buf = encode_int(Integer::from(315u64));
        assert_eq!(&buf[1..], &0u32.to_le_bytes());
    }

    #[test]
    fn int_pu64_range() {
        for val in [4_294_967_611u64, 1u64 << 40, u64::MAX] {
            let buf = encode_int(Integer::from(val));
            assert_eq!(buf[0], INT_PU64);
            assert_eq!(buf.len(), 9);
            assert_eq!(roundtrip_int(Integer::from(val)), Integer::from(val));
        }
        // Base value encodes payload 0.
        let buf = encode_int(Integer::from(PU64_BASE));
        assert_eq!(&buf[1..], &0u64.to_le_bytes());
    }

    #[test]
    fn int_pfull_just_above_pu64_top() {
        // pu64_top = PU64_BASE + u64::MAX. Value just above that goes to PFULL.
        let pu64_top = Integer::from(PU64_BASE) + Integer::from(u64::MAX);
        let above = pu64_top + Integer::one();
        let buf = encode_int(above);
        assert_eq!(buf[0], INT_PFULL);
        assert_eq!(buf.len(), 33);
        assert_eq!(roundtrip_int(above), above);
    }

    #[test]
    fn int_pu64_top_does_not_use_pfull() {
        // The boundary value (= pu64_top) must use PU64, not PFULL.
        let pu64_top = Integer::from(PU64_BASE) + Integer::from(u64::MAX);
        let buf = encode_int(pu64_top);
        assert_eq!(buf[0], INT_PU64);
        assert_eq!(roundtrip_int(pu64_top), pu64_top);
    }

    #[test]
    fn int_neg1() {
        let buf = encode_int(Integer::from(-1i64));
        assert_eq!(buf, vec![INT_NEG1]);
        assert_eq!(roundtrip_int(Integer::from(-1i64)), Integer::from(-1i64));
    }

    #[test]
    fn int_nu8_range() {
        for (val, payload) in [(-2i64, 0u8), (-100, 98), (-257, 255)] {
            let buf = encode_int(Integer::from(val));
            assert_eq!(buf, vec![INT_NU8, payload], "value {}", val);
            assert_eq!(roundtrip_int(Integer::from(val)), Integer::from(val));
        }
    }

    #[test]
    fn int_nu32_range() {
        for val in [-258i64, -1_000_000, -4_294_967_553] {
            let buf = encode_int(Integer::from(val));
            assert_eq!(buf[0], INT_NU32);
            assert_eq!(buf.len(), 5);
            assert_eq!(roundtrip_int(Integer::from(val)), Integer::from(val));
        }
    }

    #[test]
    fn int_nu64_range() {
        // Just above NU32_TOP, and at u64::MAX magnitude.
        let mag_at_base = Integer::from(NU64_BASE);
        let val_at_base = -mag_at_base;
        let buf = encode_int(val_at_base);
        assert_eq!(buf[0], INT_NU64);
        assert_eq!(&buf[1..], &0u64.to_le_bytes());
        assert_eq!(roundtrip_int(val_at_base), val_at_base);

        let mag_max = Integer::from(NU64_BASE) + Integer::from(u64::MAX);
        let val_max = -mag_max;
        let buf = encode_int(val_max);
        assert_eq!(buf[0], INT_NU64);
        assert_eq!(buf.len(), 9);
        assert_eq!(roundtrip_int(val_max), val_max);
    }

    #[test]
    fn int_nfull_just_above_nu64_top() {
        let nu64_top_mag = Integer::from(NU64_BASE) + Integer::from(u64::MAX);
        let mag_above = nu64_top_mag + Integer::one();
        let value = -mag_above;
        let buf = encode_int(value);
        assert_eq!(buf[0], INT_NFULL);
        assert_eq!(buf.len(), 33);
        assert_eq!(roundtrip_int(value), value);
    }

    // ── Integer canonicality: decoder rejection ───────────────────

    #[test]
    fn int_pfull_decoder_rejects_low_value() {
        // PFULL must encode a value > PU64 range. A small positive value
        // in FULL form should be rejected by the decoder.
        let mut buf = Vec::new();
        buf.push(INT_PFULL);
        let small = Integer::from(42u64);
        buf.extend_from_slice(&small.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_integer(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_pfull_decoder_rejects_pu64_top_exact() {
        // Boundary: even the PU64 top, dressed in PFULL, must be rejected.
        let mut buf = Vec::new();
        buf.push(INT_PFULL);
        let top = Integer::from(PU64_BASE) + Integer::from(u64::MAX);
        buf.extend_from_slice(&top.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_integer(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_pfull_decoder_rejects_negative_in_positive_tag() {
        // Setting bit 255 inside a PFULL payload must be rejected.
        let mut buf = Vec::new();
        buf.push(INT_PFULL);
        let mut bytes = [0u8; 32];
        // High bit set, lower bytes encode a magnitude > u64.
        bytes[31] = 0x80 | 0x01;
        buf.extend_from_slice(&bytes);
        let mut r = buf.as_slice();
        assert!(matches!(read_integer(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_nfull_decoder_rejects_low_magnitude() {
        let mut buf = Vec::new();
        buf.push(INT_NFULL);
        let small_neg = Integer::from(-42i64);
        buf.extend_from_slice(&small_neg.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_integer(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_nfull_decoder_rejects_positive_payload() {
        // Negative tag with sign bit clear in the payload must be rejected.
        let mut buf = Vec::new();
        buf.push(INT_NFULL);
        let pos_large = Integer::from(PU64_BASE) + Integer::from(u64::MAX) + Integer::one();
        buf.extend_from_slice(&pos_large.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_integer(&mut r), Err(ReadError::InvalidFormat)));
    }

    // ── String encoding ──────────────────────────────────────────

    fn roundtrip_string(s: &String) -> String {
        let mut buf = Vec::new();
        write_string(&mut buf, s).unwrap();
        let mut r = buf.as_slice();
        read_string(&mut r).unwrap()
    }

    #[test]
    fn string_empty() {
        let s = String::from(vec![]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf, vec![STR_IMM_MIN]);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    #[test]
    fn string_immediate_contiguous() {
        // length 58 is the last immediate
        let s = String::from(vec![0xAB; 58]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf[0], STR_IMM_MIN + 58);
        assert_eq!(buf.len(), 1 + 58);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    #[test]
    fn string_var_just_above_immediate() {
        // Length 59 → STR_VAR + sub-varint(0) + 59 bytes.
        let s = String::from(vec![0xCD; 59]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf[0], STR_VAR);
        assert_eq!(buf[1], SUBVARINT_U8);
        assert_eq!(buf[2], 0);
        assert_eq!(buf.len(), 3 + 59);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    #[test]
    fn string_var_larger() {
        let s = String::from(vec![0xEF; 1000]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf[0], STR_VAR);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    // ── List / Dict prefix ────────────────────────────────────────

    #[test]
    fn list_prefix_immediate() {
        let mut buf = Vec::new();
        write_list_prefix(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![LIST_IMM_MIN]);

        buf.clear();
        write_list_prefix(&mut buf, 58).unwrap();
        assert_eq!(buf, vec![LIST_IMM_MIN + 58]);
    }

    #[test]
    fn list_prefix_var() {
        // count 59 uses LIST_VAR with sub-varint payload 0
        let mut buf = Vec::new();
        write_list_prefix(&mut buf, 59).unwrap();
        assert_eq!(buf, vec![LIST_VAR, SUBVARINT_U8, 0]);

        // larger count
        buf.clear();
        write_list_prefix(&mut buf, 1000).unwrap();
        assert_eq!(buf[0], LIST_VAR);
        let mut r = &buf[1..];
        let v = read_subvarint(&mut r).unwrap();
        assert_eq!(v, 1000 - CONTAINER_VAR_BASE);
    }

    #[test]
    fn list_prefix_roundtrip() {
        for count in [0usize, 1, 58, 59, 60, 100, 314, 315, 1000, 70_000] {
            let mut buf = Vec::new();
            write_list_prefix(&mut buf, count).unwrap();
            let mut r = buf.as_slice();
            assert_eq!(read_list_prefix(&mut r).unwrap(), count);
        }
    }

    #[test]
    fn dict_prefix_roundtrip() {
        for count in [0usize, 1, 58, 59, 100, 1000, 70_000] {
            let mut buf = Vec::new();
            write_dict_prefix(&mut buf, count).unwrap();
            let mut r = buf.as_slice();
            assert_eq!(read_dict_prefix(&mut r).unwrap(), count);
        }
    }

    // ── Sub-varint ────────────────────────────────────────────────

    #[test]
    fn subvarint_roundtrip() {
        for n in [
            0u64, 1, 255, 256, 257, 65_791, 65_792, 65_793, 4_295_033_087,
            4_295_033_088, u64::MAX,
        ] {
            let mut buf = Vec::new();
            write_subvarint(&mut buf, n).unwrap();
            let mut r = buf.as_slice();
            assert_eq!(read_subvarint(&mut r).unwrap(), n, "failed for n={}", n);
        }
    }

    #[test]
    fn subvarint_widths_disjoint() {
        // Width boundaries: each tag's range starts where the previous ended.
        let cases = [
            (0u64, SUBVARINT_U8, 2usize),
            (255, SUBVARINT_U8, 2),
            (256, SUBVARINT_U16, 3),
            (65_791, SUBVARINT_U16, 3),
            (65_792, SUBVARINT_U32, 5),
            (4_295_033_087, SUBVARINT_U32, 5),
            (4_295_033_088, SUBVARINT_U64, 9),
        ];
        for (n, expected_tag, expected_len) in cases {
            let mut buf = Vec::new();
            write_subvarint(&mut buf, n).unwrap();
            assert_eq!(buf[0], expected_tag, "n={}", n);
            assert_eq!(buf.len(), expected_len, "n={}", n);
        }
    }

    // ── read_value: types ─────────────────────────────────────────

    #[test]
    fn read_value_int() {
        let mut buf = Vec::new();
        write_integer(&mut buf, &Integer::from(42u64)).unwrap();
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Int(i)) => assert_eq!(i, Integer::from(42u64)),
            _ => panic!("expected Int"),
        }
    }

    #[test]
    fn read_value_string() {
        let mut buf = Vec::new();
        let s = String::from(vec![1, 2, 3]);
        write_string(&mut buf, &s).unwrap();
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::String(s2)) => assert_eq!(s2.as_bytes(), &[1, 2, 3]),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn read_value_point() {
        let mut buf = Vec::new();
        buf.push(POINT_TAG);
        buf.extend_from_slice(&[0xAA; 32]);
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Point(p)) => assert_eq!(p.as_bytes(), &[0xAA; 32]),
            _ => panic!("expected Point"),
        }
    }

    #[test]
    fn read_value_list_style_dict() {
        let mut buf = Vec::new();
        write_list_prefix(&mut buf, 2).unwrap();
        write_integer(&mut buf, &Integer::from(5u64)).unwrap();
        write_integer(&mut buf, &Integer::from(10u64)).unwrap();
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Dict(d)) => {
                assert_eq!(d.len(), 2);
                let keys: Vec<_> = d.entries().map(|(k, _)| *k).collect();
                assert_eq!(keys, vec![Integer::from(0u64), Integer::from(1u64)]);
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn read_value_dict_style() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 1).unwrap();
        write_integer(&mut buf, &Integer::from(99u64)).unwrap();
        write_string(&mut buf, &String::from(b"hi".to_vec())).unwrap();
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Dict(d)) => {
                assert_eq!(d.len(), 1);
                let (k, v) = d.entries().next().unwrap();
                assert_eq!(*k, Integer::from(99u64));
                match v {
                    Value::String(s) => assert_eq!(s.as_bytes(), b"hi"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    // ── Encoding canonicality at the Dict level ──────────────────

    #[test]
    fn write_dict_sequential_uses_list_encoding() {
        let d = Dict::from_values(vec![
            Value::Int(Integer::from(10u64)),
            Value::Int(Integer::from(20u64)),
        ]);
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        assert!(buf[0] >= LIST_IMM_MIN && buf[0] <= LIST_IMM_MAX);
    }

    #[test]
    fn write_dict_non_sequential_uses_dict_encoding() {
        let mut d = Dict::new();
        d.insert(Integer::from(10u64), Value::Int(Integer::from(1u64)));
        d.insert(Integer::from(20u64), Value::Int(Integer::from(2u64)));
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        assert!(buf[0] >= DICT_IMM_MIN && buf[0] <= DICT_IMM_MAX);
    }

    #[test]
    fn read_value_dict_rejects_sequential_keys_in_dict_form() {
        // Manually craft a dict-style payload with keys [0, 1] —
        // they should have been list-style.
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_integer(&mut buf, &Integer::from(0u64)).unwrap();
        write_integer(&mut buf, &Integer::from(7u64)).unwrap();
        write_integer(&mut buf, &Integer::from(1u64)).unwrap();
        write_integer(&mut buf, &Integer::from(8u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_dict_rejects_out_of_order_keys() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_integer(&mut buf, &Integer::from(5u64)).unwrap();
        write_integer(&mut buf, &Integer::from(0u64)).unwrap();
        write_integer(&mut buf, &Integer::from(2u64)).unwrap();
        write_integer(&mut buf, &Integer::from(0u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_dict_rejects_duplicate_keys() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_integer(&mut buf, &Integer::from(7u64)).unwrap();
        write_integer(&mut buf, &Integer::from(0u64)).unwrap();
        write_integer(&mut buf, &Integer::from(7u64)).unwrap();
        write_integer(&mut buf, &Integer::from(0u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_unimplemented_returns_none() {
        for tag in [TOKEN_TAG, CLEAR_TOKEN_TAG, WIDE_TOKEN_TAG, OBJECT_TAG, MERLIN_TAG] {
            let buf = vec![tag];
            let mut r = buf.as_slice();
            assert!(read_value(&mut r).unwrap().is_none());
        }
    }

    #[test]
    fn read_value_reserved_tags_reject() {
        // 254 (reserved) and 255 (extension) currently reject.
        for tag in [254u8, 255] {
            let buf = vec![tag];
            let mut r: &[u8] = &buf;
            assert!(read_value(&mut r).is_err());
        }
    }

    #[test]
    fn read_value_rejects_excessive_nesting() {
        // 70 levels of single-element lists, then an immediate int.
        // The decoder must reject before recursing past MAX_DEPTH.
        let mut buf = Vec::new();
        for _ in 0..70 {
            buf.push(LIST_IMM_MIN + 1);
        }
        buf.push(0u8); // innermost int
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_rejects_oversized_string_len() {
        // STR_VAR claiming 1 billion bytes, with no actual data.
        let mut buf = Vec::new();
        buf.push(STR_VAR);
        write_subvarint(&mut buf, 1_000_000_000 - CONTAINER_VAR_BASE).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_rejects_oversized_list_count() {
        // LIST_VAR claiming 1 billion entries, with no payload.
        let mut buf = Vec::new();
        buf.push(LIST_VAR);
        write_subvarint(&mut buf, 1_000_000_000 - CONTAINER_VAR_BASE).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_rejects_oversized_dict_count() {
        // DICT_VAR claiming 1 billion entries, with no payload.
        let mut buf = Vec::new();
        buf.push(DICT_VAR);
        write_subvarint(&mut buf, 1_000_000_000 - CONTAINER_VAR_BASE).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_all_two_byte_combinations_no_panic() {
        for b0 in 0u16..=255 {
            for b1 in 0u16..=255 {
                let buf = [b0 as u8, b1 as u8];
                let mut r: &[u8] = &buf;
                let _ = read_value(&mut r);
            }
        }
    }
}
