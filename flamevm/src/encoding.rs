//! Canonical compact encoding for flamevm values.
//!
//! Each value has exactly one wire byte sequence. The first byte is
//! a type+width tag; offset-based width classes carve disjoint
//! sub-ranges so the encoder has no choice and the decoder has
//! little to enforce.
//!
//! Tag namespace:
//!
//! ```text
//! 0..=58     Int253 positive immediate (value = tag)
//! 59..=62    Int253 +U8 / +U32 / +U64 / +FULL (offset-based widths)
//! 63..=67    Int253 -1 / -U8 / -U32 / -U64 / -FULL
//! 68..=127   String (immediate length 0..=58; 127 = VAR sub-varint)
//! 128..=187  List (immediate count; 187 = VAR sub-varint)
//! 188..=247  Dict  (immediate count; 247 = VAR sub-varint)
//! 248        Point          (32-byte compressed Ristretto)
//! 249..=253  Token / ClearToken / WideToken / Object / Merlin
//! 254        reserved
//! 255        extension      (sub-tag follows)
//! ```
//!
//! Sub-varint (used inside `STR_VAR` / `LIST_VAR` / `DICT_VAR`)
//! encodes a non-negative integer with one byte sequence per value:
//!
//! ```text
//! sub-tag 0  1 LE byte    value = b                  range 0..=255
//! sub-tag 1  2 LE bytes   value = 256 + w            range 256..=65_791
//! sub-tag 2  4 LE bytes   value = 65_792 + w         range up to ≈4.3e9
//! sub-tag 3  8 LE bytes   value = 4_295_032_608 + w  range up to ≈1.8e19
//! ```
//!
//! `INT_PFULL` / `INT_NFULL` reject values that fit in a narrower
//! width; `DICT_*` rejects keys `0..n-1` (use `LIST_*`).

use core::cmp::Ordering;
use core::convert::TryFrom;

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
pub use readerwriter::{ReadError, Reader, WriteError, Writer};

use crate::crypto::Point;
use crate::dict::Dict;
use crate::int253::Int253;
use crate::string::String;
use crate::value::Value;

// ── Tag constants ─────────────────────────────────────────────────

// Int253
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

// Int253 width-class bases (the value encoded with a zero payload).
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

pub(crate) fn write_subvarint(w: &mut impl Writer, n: u64) -> Result<(), WriteError> {
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

pub(crate) fn read_subvarint(r: &mut impl Reader) -> Result<u64, ReadError> {
    let tag = r.read_u8()?;
    match tag {
        SUBVARINT_U8 => Ok(r.read_u8()? as u64),
        SUBVARINT_U16 => {
            let mut buf = [0u8; 2];
            r.read(&mut buf)?;
            Ok(SUBVAR_U16_BASE + (u16::from_le_bytes(buf) as u64))
        }
        SUBVARINT_U32 => Ok(SUBVAR_U32_BASE + (r.read_u32()? as u64)),
        // Reject overflow: a payload near u64::MAX would wrap past the
        // base and alias a smaller value (non-canonical).
        SUBVARINT_U64 => SUBVAR_U64_BASE
            .checked_add(r.read_u64()?)
            .ok_or(ReadError::InvalidFormat),
        _ => Err(ReadError::InvalidFormat),
    }
}

// ── Int253 encoding ──────────────────────────────────────────────

/// Writes an `Int253` in compact canonical form.
pub fn write_int253(w: &mut impl Writer, int: &Int253) -> Result<(), WriteError> {
    if int.is_negative() {
        write_negative_int253(w, int)
    } else {
        write_positive_int253(w, int)
    }
}

fn write_positive_int253(w: &mut impl Writer, int: &Int253) -> Result<(), WriteError> {
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
    let pu64_top = Int253::from(PU64_BASE) + Int253::from(u64::MAX);
    if int.cmp(&pu64_top) != Ordering::Greater {
        let payload_int = *int - Int253::from(PU64_BASE);
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

fn write_negative_int253(w: &mut impl Writer, int: &Int253) -> Result<(), WriteError> {
    let abs = int.abs();
    if abs == Int253::ONE {
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

    let nu64_top_mag = Int253::from(NU64_BASE) + Int253::from(u64::MAX);
    if abs.cmp(&nu64_top_mag) != Ordering::Greater {
        let payload_int = abs - Int253::from(NU64_BASE);
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

/// Reads a compact-encoded `Int253`.
pub fn read_int253(r: &mut impl Reader) -> Result<Int253, ReadError> {
    let tag = r.read_u8()?;
    read_int253_with_tag(r, tag)
}

fn read_int253_with_tag(r: &mut impl Reader, tag: u8) -> Result<Int253, ReadError> {
    match tag {
        0..=INT_IMM_MAX => Ok(Int253::from(tag as u64)),
        INT_PU8 => {
            let b = r.read_u8()?;
            Ok(Int253::from(PU8_BASE + (b as u64)))
        }
        INT_PU32 => {
            let w = r.read_u32()? as u64;
            Ok(Int253::from(PU32_BASE + w))
        }
        INT_PU64 => {
            let w = r.read_u64()?;
            Ok(Int253::from(PU64_BASE) + Int253::from(w))
        }
        INT_PFULL => read_positive_full(r),
        INT_NEG1 => Ok(Int253::from(-1i64)),
        INT_NU8 => {
            let b = r.read_u8()?;
            Ok(Int253::from_parts(true, Scalar::from(NU8_BASE + (b as u64))))
        }
        INT_NU32 => {
            let w = r.read_u32()? as u64;
            Ok(Int253::from_parts(true, Scalar::from(NU32_BASE + w)))
        }
        INT_NU64 => {
            let w = r.read_u64()?;
            let mag = Int253::from(NU64_BASE) + Int253::from(w);
            Ok(-mag)
        }
        INT_NFULL => read_negative_full(r),
        _ => Err(ReadError::InvalidFormat),
    }
}

fn read_positive_full(r: &mut impl Reader) -> Result<Int253, ReadError> {
    let buf = r.read_u8x32()?;
    let int = Int253::from_bytes(buf).ok_or(ReadError::InvalidFormat)?;
    // Sign bit must be clear for a positive FULL.
    if int.is_negative() {
        return Err(ReadError::InvalidFormat);
    }
    // Canonicality: value must exceed the PU64 range.
    let pu64_top = Int253::from(PU64_BASE) + Int253::from(u64::MAX);
    if int.cmp(&pu64_top) != Ordering::Greater {
        return Err(ReadError::InvalidFormat);
    }
    Ok(int)
}

fn read_negative_full(r: &mut impl Reader) -> Result<Int253, ReadError> {
    let buf = r.read_u8x32()?;
    let int = Int253::from_bytes(buf).ok_or(ReadError::InvalidFormat)?;
    // Sign bit must be set for a negative FULL.
    if !int.is_negative() {
        return Err(ReadError::InvalidFormat);
    }
    // Canonicality: magnitude must exceed the NU64 range.
    let nu64_top_mag = Int253::from(NU64_BASE) + Int253::from(u64::MAX);
    if int.abs().cmp(&nu64_top_mag) != Ordering::Greater {
        return Err(ReadError::InvalidFormat);
    }
    Ok(int)
}

// ── String encoding ───────────────────────────────────────────────

/// Writes a `String` (byte-string) in canonical compact form.
///
/// For witness-bearing String variants (`Commitment`, `Scalar`,
/// `Predicate`), the bytes are derived from the variant's canonical
/// encoding (32-byte compressed point or sign-magnitude int) — the
/// wire form is byte-identical to what an Opaque String wrapping the
/// same bytes would produce. The witness data itself is discarded;
/// the prover ferries it via the in-memory Program (and pushes it
/// onto the stack as a witness-bearing String when needed).
pub fn write_string(w: &mut impl Writer, s: &String) -> Result<(), WriteError> {
    // Borrow the bytes for the common `Opaque` case; only witness
    // variants pay one allocation to canonicalize (no `Cow`).
    let owned;
    let bytes: &[u8] = match s.as_opaque() {
        Some(b) => b,
        None => {
            owned = s.to_bytes_vec();
            &owned
        }
    };
    let len = bytes.len() as u64;
    if len <= INT_IMM_MAX as u64 {
        w.write_u8(b"str.tag", STR_IMM_MIN + len as u8)?;
    } else {
        w.write_u8(b"str.tag", STR_VAR)?;
        write_subvarint(w, len - CONTAINER_VAR_BASE)?;
    }
    w.write(b"str.data", bytes)
}

/// Reads a length-prefixed byte string and returns its raw bytes,
/// erroring if the next tag isn't a `String` or the length is
/// malformed/out of bounds. Use this when a decoder expects a string
/// and wants the bytes directly, without routing through `Value`.
pub fn read_string(r: &mut impl Reader) -> Result<Vec<u8>, ReadError> {
    let tag = r.read_u8()?;
    Ok(read_string_with_tag(r, tag)?.to_bytes())
}

/// Reads a `String` body whose type tag was already read.
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

fn read_dict_count_with_tag(r: &mut impl Reader, tag: u8) -> Result<usize, ReadError> {
    let n: u64 = match tag {
        DICT_IMM_MIN..=DICT_IMM_MAX => (tag - DICT_IMM_MIN) as u64,
        DICT_VAR => CONTAINER_VAR_BASE + read_subvarint(r)?,
        _ => return Err(ReadError::InvalidFormat),
    };
    usize::try_from(n).map_err(|_| ReadError::InvalidFormat)
}

// ── Value encoding ────────────────────────────────────────────────

fn keys_are_sequential<'a>(keys: impl Iterator<Item = &'a Int253>) -> bool {
    keys.enumerate().all(|(i, k)| *k == Int253::from(i as u64))
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
        // Int253
        0..=INT_NFULL => {
            let int = read_int253_with_tag(r, tag)?;
            Ok(Some(Value::Int253(int)))
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
            let mut entries: Vec<(Int253, Value)> = Vec::with_capacity(count);
            let mut last_key: Option<Int253> = None;
            for _ in 0..count {
                let key = read_int253(r)?;
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
        // Token: 32-byte qty commitment point + 32-byte flv commitment
        // point. Decoded as a closed-commitment pair (the wire form
        // carries no witness). The encrypted-issue / borrow / cloak
        // opcodes that produce Tokens at runtime may still hold open
        // commitments; the wire round-trip simply re-uses the closed
        // form when re-encoded.
        TOKEN_TAG => {
            let qty_bytes = r.read_u8x32()?;
            let flv_bytes = r.read_u8x32()?;
            let qty = crate::Commitment::Closed(CompressedRistretto(qty_bytes));
            let flv = crate::Commitment::Closed(CompressedRistretto(flv_bytes));
            Ok(Some(Value::Token(crate::Token::new(qty, flv))))
        }
        // ClearToken (portable when non-negative): tag + cleartext qty
        // + flv as compact `Int253`s.
        CLEAR_TOKEN_TAG => {
            let qty = read_int253(r)?;
            let flv = read_int253(r)?;
            Ok(Some(Value::ClearToken(crate::ClearToken::new(qty, flv))))
        }
        // WideToken is non-portable (confidential, may be negative) and
        // never crosses the wire.
        WIDE_TOKEN_TAG => Err(ReadError::InvalidFormat),
        // Unimplemented portable types — payload size unknown, signal
        // to caller (kept for Phase-17 / future encodings).
        OBJECT_TAG | MERLIN_TAG => Ok(None),
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
            write_int253(w, k)?;
            write_value(w, v)?;
        }
    }
    Ok(())
}

/// Writes a `Value`. Portable types (incl. non-negative `ClearToken`)
/// serialize canonically; non-portable variants (`WideToken`, `Cell`,
/// `Merlin`, and the stack-only constraint-system types) return
/// `WriteError::InsufficientCapacity` — a "refuse to encode" signal.
/// Callers consult `Value::is_portable` before encoding.
pub fn write_value(w: &mut impl Writer, val: &Value) -> Result<(), WriteError> {
    match val {
        Value::Int253(i) => write_int253(w, i),
        Value::String(s) => write_string(w, s),
        Value::Dict(d) => write_dict(w, d),
        Value::Point(p) => {
            w.write_u8(b"point.tag", POINT_TAG)?;
            w.write(b"point.data", &p.to_bytes())
        }
        // Token (portable): tag + qty point (32 B) + flv point (32 B).
        Value::Token(t) => {
            w.write_u8(b"token.tag", TOKEN_TAG)?;
            w.write(b"token.qty", t.qty.to_point().as_bytes())?;
            w.write(b"token.flv", t.flv.to_point().as_bytes())
        }
        // ClearToken (portable when non-negative — the `is_portable`
        // gate upstream ensures only those reach here): tag + cleartext
        // qty + flv as compact `Int253`s.
        Value::ClearToken(t) => {
            w.write_u8(b"cleartoken.tag", CLEAR_TOKEN_TAG)?;
            write_int253(w, &t.qty)?;
            write_int253(w, &t.flv)
        }
        // Non-portable: widetokens / cells / merlins / CS-only types.
        // By design these never cross the wire. `InsufficientCapacity`
        // is the existing "refuse to encode" sentinel; higher-level
        // callers reject them before encoding via `Value::is_portable`.
        Value::WideToken(_)
        | Value::Cell(_)
        | Value::Merlin(_)
        | Value::Variable(_)
        | Value::Expression(_)
        | Value::Constraint(_)
        | Value::MultiscalarMul(_) => Err(WriteError::InsufficientCapacity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Int253 canonicality: decoder rejection ───────────────────

    #[test]
    fn int_pfull_decoder_rejects_low_value() {
        // PFULL must encode a value > PU64 range. A small positive value
        // in FULL form should be rejected by the decoder.
        let mut buf = Vec::new();
        buf.push(INT_PFULL);
        let small = Int253::from(42u64);
        buf.extend_from_slice(&small.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_int253(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_pfull_decoder_rejects_pu64_top_exact() {
        // Boundary: even the PU64 top, dressed in PFULL, must be rejected.
        let mut buf = Vec::new();
        buf.push(INT_PFULL);
        let top = Int253::from(PU64_BASE) + Int253::from(u64::MAX);
        buf.extend_from_slice(&top.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_int253(&mut r), Err(ReadError::InvalidFormat)));
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
        assert!(matches!(read_int253(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_nfull_decoder_rejects_low_magnitude() {
        let mut buf = Vec::new();
        buf.push(INT_NFULL);
        let small_neg = Int253::from(-42i64);
        buf.extend_from_slice(&small_neg.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_int253(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn int_nfull_decoder_rejects_positive_payload() {
        // Negative tag with sign bit clear in the payload must be rejected.
        let mut buf = Vec::new();
        buf.push(INT_NFULL);
        let pos_large = Int253::from(PU64_BASE) + Int253::from(u64::MAX) + Int253::ONE;
        buf.extend_from_slice(&pos_large.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_int253(&mut r), Err(ReadError::InvalidFormat)));
    }

    // ── Encoding canonicality at the Dict level ──────────────────

    #[test]
    fn write_dict_sequential_uses_list_encoding() {
        let d = Dict::from_values(vec![
            Value::Int253(Int253::from(10u64)),
            Value::Int253(Int253::from(20u64)),
        ]);
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        assert!(buf[0] >= LIST_IMM_MIN && buf[0] <= LIST_IMM_MAX);
    }

    #[test]
    fn write_dict_non_sequential_uses_dict_encoding() {
        let mut d = Dict::new();
        d.insert(Int253::from(10u64), Value::Int253(Int253::from(1u64)));
        d.insert(Int253::from(20u64), Value::Int253(Int253::from(2u64)));
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
        write_int253(&mut buf, &Int253::from(0u64)).unwrap();
        write_int253(&mut buf, &Int253::from(7u64)).unwrap();
        write_int253(&mut buf, &Int253::from(1u64)).unwrap();
        write_int253(&mut buf, &Int253::from(8u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_dict_rejects_out_of_order_keys() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_int253(&mut buf, &Int253::from(5u64)).unwrap();
        write_int253(&mut buf, &Int253::from(0u64)).unwrap();
        write_int253(&mut buf, &Int253::from(2u64)).unwrap();
        write_int253(&mut buf, &Int253::from(0u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_dict_rejects_duplicate_keys() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_int253(&mut buf, &Int253::from(7u64)).unwrap();
        write_int253(&mut buf, &Int253::from(0u64)).unwrap();
        write_int253(&mut buf, &Int253::from(7u64)).unwrap();
        write_int253(&mut buf, &Int253::from(0u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_unimplemented_returns_none() {
        // OBJECT_TAG / MERLIN_TAG still soft-fail decoding with `Ok(None)`
        // (their wire formats are pending). The token tags now have
        // definite semantics — see `token_decode_*` below.
        for tag in [OBJECT_TAG, MERLIN_TAG] {
            let buf = vec![tag];
            let mut r = buf.as_slice();
            assert!(read_value(&mut r).unwrap().is_none());
        }
    }

    #[test]
    fn read_value_widetoken_tag_rejects() {
        // WideToken is non-portable: tag 0xfb is always a wire error.
        let buf = vec![WIDE_TOKEN_TAG];
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn token_encode_decode_roundtrip() {
        // Build a Token with cleartext qty/flv → encode → decode →
        // re-encode → bytes equal. Decoded Token holds Closed
        // commitments; original holds Open ones, so we compare bytes
        // and structural shape rather than struct equality.
        let original = Value::Token(crate::Token::cleartext(
            Int253::from(123u64),
            Int253::from(7u64),
        ));
        let mut buf = Vec::new();
        write_value(&mut buf, &original).expect("encodes");
        // Wire shape: 1 tag byte + 32 qty bytes + 32 flv bytes.
        assert_eq!(buf.len(), 65);
        assert_eq!(buf[0], TOKEN_TAG);

        // Decode.
        let mut r = buf.as_slice();
        let decoded = read_value(&mut r).expect("decodes").expect("token tag");
        match &decoded {
            Value::Token(t) => {
                let expected_qty = crate::Commitment::unblinded(Int253::from(123u64));
                let expected_flv = crate::Commitment::unblinded(Int253::from(7u64));
                assert_eq!(t.qty.to_point(), expected_qty.to_point());
                assert_eq!(t.flv.to_point(), expected_flv.to_point());
            }
            _ => panic!("decoded value must be Token"),
        }
        assert!(r.is_empty(), "decoder consumed full input");

        // Re-encode and compare bytes (canonicality).
        let mut buf2 = Vec::new();
        write_value(&mut buf2, &decoded).expect("re-encodes");
        assert_eq!(buf, buf2);
    }

    #[test]
    fn token_decode_rejects_truncated_payload() {
        // 1 tag byte + 63 bytes (one short).
        let mut buf = vec![TOKEN_TAG];
        buf.extend_from_slice(&[0u8; 63]);
        let mut r = buf.as_slice();
        // Reader runs out of bytes on the second 32-byte read.
        assert!(matches!(
            read_value(&mut r),
            Err(ReadError::InsufficientBytes)
        ));
    }

    #[test]
    fn cleartoken_encode_decode_roundtrip() {
        let original = Value::ClearToken(crate::ClearToken::new(
            Int253::from(5u64),
            Int253::from(7u64),
        ));
        let mut buf = Vec::new();
        write_value(&mut buf, &original).expect("encodes");
        assert_eq!(buf[0], CLEAR_TOKEN_TAG);

        let mut r = buf.as_slice();
        let decoded = read_value(&mut r).expect("decodes").expect("cleartoken tag");
        match &decoded {
            Value::ClearToken(t) => {
                assert_eq!(t.qty(), Int253::from(5u64));
                assert_eq!(t.flv(), Int253::from(7u64));
            }
            _ => panic!("decoded value must be ClearToken"),
        }
        assert!(r.is_empty(), "decoder consumed full input");

        // Re-encode → byte-identical (canonicality).
        let mut buf2 = Vec::new();
        write_value(&mut buf2, &decoded).expect("re-encodes");
        assert_eq!(buf, buf2);
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
