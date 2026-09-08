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
//! 0..=58     Scalar immediate (value = tag)
//! 59..=61    Scalar near-zero U8 / U32 / U64 (offset-based widths)
//! 62         Scalar FULL (canonical 32-byte little-endian residue)
//! 63..=66    Scalar near-order -1 / -U8 / -U32 / -U64
//! 67         reserved
//! 68..=127   String (immediate length 0..=58; 127 = VAR sub-varint)
//! 128..=187  List (immediate count; 187 = VAR sub-varint)
//! 188..=247  Dict  (immediate count; 247 = VAR sub-varint)
//! 248        Point          (32-byte compressed Ristretto)
//! 249..=250  Token / ClearToken
//! 251..=254  reserved
//! 255        reserved extension prefix
//! ```
//!
//! Sub-varint (used inside `STR_VAR` / `LIST_VAR` / `DICT_VAR`)
//! encodes a non-negative integer with one byte sequence per value:
//!
//! ```text
//! sub-tag 0  1 LE byte    value = b                  range 0..=255
//! sub-tag 1  2 LE bytes   value = 256 + w            range 256..=65_791
//! sub-tag 2  4 LE bytes   value = 65_792 + w         range up to ≈4.3e9
//! sub-tag 3  8 LE bytes   value = 4_295_033_088 + w  range up to `u64::MAX`
//! ```
//!
//! `INT_FULL` rejects values that fit a narrower near-zero or near-order
//! width; `DICT_*` rejects keys `0..n-1` (use `LIST_*`).

use core::cmp::Ordering;
use core::convert::TryFrom;

use curve25519_dalek::ristretto::CompressedRistretto;
pub use readerwriter::{ReadError, Reader, WriteError, Writer};

use crate::constraints::Commitment;
use crate::crypto::Point;
use crate::dict::Dict;
use crate::scalar::Scalar;
use crate::string::String;
use crate::token::{ClearToken, Token};
use crate::value::Value;

/// Failure to encode a VM [`Value`]. Writer failures are exclusively
/// capacity failures; `UnsupportedType` means the value has no byte format.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ValueEncodeError {
    Writer(WriteError),
    UnsupportedType,
}

impl From<WriteError> for ValueEncodeError {
    fn from(error: WriteError) -> Self {
        Self::Writer(error)
    }
}

// ── Tag constants ─────────────────────────────────────────────────

// Scalar
const INT_IMM_MAX: u8 = 58;
const INT_PU8: u8 = 59;
const INT_PU32: u8 = 60;
const INT_PU64: u8 = 61;
const INT_FULL: u8 = 62;
const INT_NEG1: u8 = 63;
const INT_NU8: u8 = 64;
const INT_NU32: u8 = 65;
const INT_NU64: u8 = 66;
// 67 is reserved.

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
const RESERVED_VALUE_TAG_MIN: u8 = 251;
const RESERVED_VALUE_TAG_MAX: u8 = 254;
// Reserved extension prefix: 255

// Number of IMM slots (values 0..=58) shared across int/str/list/dict.
const IMM_COUNT: u8 = INT_IMM_MAX + 1; // 59

// Scalar width-class bases (the value encoded with a zero payload).
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

// ── Scalar encoding ──────────────────────────────────────────────

/// Writes a `Scalar` in compact canonical form.
pub fn write_scalar(w: &mut impl Writer, int: &Scalar) -> Result<(), WriteError> {
    let near_order = -*int;
    let near_order_top = Scalar::from(NU64_BASE) + Scalar::from(u64::MAX);
    if near_order != Scalar::ZERO && near_order <= near_order_top {
        write_near_order_scalar(w, &near_order)
    } else {
        write_near_zero_or_full_scalar(w, int)
    }
}

fn write_near_zero_or_full_scalar(w: &mut impl Writer, int: &Scalar) -> Result<(), WriteError> {
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
    let pu64_top = Scalar::from(PU64_BASE) + Scalar::from(u64::MAX);
    if int.cmp(&pu64_top) != Ordering::Greater {
        let payload_int = *int - Scalar::from(PU64_BASE);
        let payload = payload_int
            .to_u64()
            .expect("invariant: int - PU64_BASE fits u64 when int <= pu64_top");
        w.write_u8(b"int.tag", INT_PU64)?;
        return w.write_u64(b"int.u64", payload);
    }

    // FULL: write the 32-byte canonical scalar as-is.
    w.write_u8(b"int.tag", INT_FULL)?;
    w.write(b"int.full", &int.to_bytes())
}

/// Writes the modular negation of a nonzero, compact near-order distance.
fn write_near_order_scalar(w: &mut impl Writer, distance: &Scalar) -> Result<(), WriteError> {
    if *distance == Scalar::ONE {
        return w.write_u8(b"int.tag", INT_NEG1);
    }
    if let Some(mag) = distance.to_u64() {
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

    let payload = (*distance - Scalar::from(NU64_BASE))
        .to_u64()
        .expect("compact near-order distance minus NU64_BASE fits u64");
    w.write_u8(b"int.tag", INT_NU64)?;
    w.write_u64(b"int.u64", payload)
}

/// Reads a compact-encoded `Scalar`.
pub fn read_scalar(r: &mut impl Reader) -> Result<Scalar, ReadError> {
    let tag = r.read_u8()?;
    read_scalar_with_tag(r, tag)
}

fn read_scalar_with_tag(r: &mut impl Reader, tag: u8) -> Result<Scalar, ReadError> {
    match tag {
        0..=INT_IMM_MAX => Ok(Scalar::from(tag as u64)),
        INT_PU8 => {
            let b = r.read_u8()?;
            Ok(Scalar::from(PU8_BASE + (b as u64)))
        }
        INT_PU32 => {
            let w = r.read_u32()? as u64;
            Ok(Scalar::from(PU32_BASE + w))
        }
        INT_PU64 => {
            let w = r.read_u64()?;
            Ok(Scalar::from(PU64_BASE) + Scalar::from(w))
        }
        INT_FULL => read_full_scalar(r),
        INT_NEG1 => Ok(Scalar::from(-1i64)),
        INT_NU8 => {
            let b = r.read_u8()?;
            Ok(-Scalar::from(NU8_BASE + (b as u64)))
        }
        INT_NU32 => {
            let w = r.read_u32()? as u64;
            Ok(-Scalar::from(NU32_BASE + w))
        }
        INT_NU64 => {
            let w = r.read_u64()?;
            let mag = Scalar::from(NU64_BASE) + Scalar::from(w);
            Ok(-mag)
        }
        _ => Err(ReadError::InvalidFormat),
    }
}

/// FULL is canonical only outside both compact endpoint ranges.
fn read_full_scalar(r: &mut impl Reader) -> Result<Scalar, ReadError> {
    let buf = r.read_u8x32()?;
    let int = Scalar::from_bytes(buf).ok_or(ReadError::InvalidFormat)?;
    let near_zero_top = Scalar::from(PU64_BASE) + Scalar::from(u64::MAX);
    let near_order_top = Scalar::from(NU64_BASE) + Scalar::from(u64::MAX);
    if int <= near_zero_top || -int <= near_order_top {
        return Err(ReadError::InvalidFormat);
    }
    Ok(int)
}

// ── String encoding ───────────────────────────────────────────────

/// Writes a `String` (byte-string) in canonical compact form.
///
/// For witness-bearing String variants (`Commitment`, `Scalar`,
/// `Predicate`), the bytes are derived from the variant's canonical
/// encoding (32-byte compressed point or canonical scalar) — the
/// wire form is byte-identical to what an Opaque String wrapping the
/// same bytes would produce. The witness data itself is discarded;
/// the prover ferries it via the in-memory ScriptBuilder (and pushes it
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

fn keys_are_sequential<'a>(keys: impl Iterator<Item = &'a Scalar>) -> bool {
    keys.enumerate().all(|(i, k)| *k == Scalar::from(i as u64))
}

/// Maximum nesting depth for `read_value`. Each list/dict entry counts
/// as one level. Bounds stack use and prevents DoS via deeply nested input.
const MAX_DEPTH: u32 = 64;

/// Reads a `Value`.
/// Token and ClearToken decode; all unassigned and extension tags are invalid.
/// Returns `Err` for malformed data, non-canonical encodings, or
/// resource-exhausting input (excessive nesting, oversized counts).
pub fn read_value(r: &mut impl Reader) -> Result<Option<Value>, ReadError> {
    read_value_with_depth(r, 0)
}

fn read_value_with_depth(r: &mut impl Reader, depth: u32) -> Result<Option<Value>, ReadError> {
    if depth >= MAX_DEPTH {
        return Err(ReadError::InvalidFormat);
    }
    let tag = r.read_u8()?;
    match tag {
        // Scalar
        0..=INT_NU64 => {
            let int = read_scalar_with_tag(r, tag)?;
            Ok(Some(Value::Scalar(int)))
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
            let mut entries: Vec<(Scalar, Value)> = Vec::with_capacity(count);
            let mut last_key: Option<Scalar> = None;
            for _ in 0..count {
                let key = read_scalar(r)?;
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
            let qty = Commitment::Closed(CompressedRistretto(qty_bytes));
            let flv = Commitment::Closed(CompressedRistretto(flv_bytes));
            Ok(Some(Value::Token(Token::new(qty, flv))))
        }
        // ClearToken: tag + cleartext qty + flv as compact `Scalar`s.
        // Portability is a domain-boundary rule, not a decoding rule.
        CLEAR_TOKEN_TAG => {
            let qty = read_scalar(r)?;
            let flv = read_scalar(r)?;
            Ok(Some(Value::ClearToken(ClearToken::new(qty, flv))))
        }
        // Unassigned tags have no payload shape and cannot be skipped.
        RESERVED_VALUE_TAG_MIN..=RESERVED_VALUE_TAG_MAX => Err(ReadError::InvalidFormat),
        // Extension tags are not yet defined.
        _ => Err(ReadError::InvalidFormat),
    }
}

/// Writes a `Dict`. Uses the list-style encoding when keys are
/// sequential 0, 1, 2, ...; otherwise uses the dict-style encoding.
pub(crate) fn write_dict(w: &mut impl Writer, dict: &Dict) -> Result<(), ValueEncodeError> {
    if keys_are_sequential(dict.entries().map(|(k, _)| k)) {
        write_list_prefix(w, dict.len())?;
        for (_, v) in dict.entries() {
            write_value(w, v)?;
        }
    } else {
        write_dict_prefix(w, dict.len())?;
        for (k, v) in dict.entries() {
            write_scalar(w, k)?;
            write_value(w, v)?;
        }
    }
    Ok(())
}

/// Writes a canonically encodable `Value` without applying domain-level
/// portability policy. Centered-debit ClearTokens and representable non-portable
/// Dicts are accepted; VM-only variants with no byte format return
/// [`ValueEncodeError::UnsupportedType`].
pub(crate) fn write_value(w: &mut impl Writer, val: &Value) -> Result<(), ValueEncodeError> {
    match val {
        Value::Scalar(i) => Ok(write_scalar(w, i)?),
        Value::String(s) => Ok(write_string(w, s)?),
        Value::Dict(d) => write_dict(w, d),
        Value::Point(p) => {
            w.write_u8(b"point.tag", POINT_TAG)?;
            Ok(w.write(b"point.data", &p.to_bytes())?)
        }
        // Token (portable): tag + qty point (32 B) + flv point (32 B).
        Value::Token(t) => {
            w.write_u8(b"token.tag", TOKEN_TAG)?;
            w.write(b"token.qty", t.qty.to_point().as_bytes())?;
            Ok(w.write(b"token.flv", t.flv.to_point().as_bytes())?)
        }
        // ClearToken: tag + cleartext qty + flv as compact `Scalar`s.
        Value::ClearToken(t) => {
            w.write_u8(b"cleartoken.tag", CLEAR_TOKEN_TAG)?;
            write_scalar(w, &t.qty)?;
            Ok(write_scalar(w, &t.flv)?)
        }
        Value::WideToken(_)
        | Value::Contract(_)
        | Value::Merlin(_)
        | Value::Variable(_)
        | Value::Expression(_)
        | Value::Constraint(_)
        | Value::MultiscalarMul(_) => Err(ValueEncodeError::UnsupportedType),
    }
}

/// Writes a value already admitted to a domain whose invariant guarantees a
/// canonical representation. This does not perform a portability check.
pub(crate) fn write_admitted_value(w: &mut impl Writer, val: &Value) -> Result<(), WriteError> {
    match write_value(w, val) {
        Ok(()) => Ok(()),
        Err(ValueEncodeError::Writer(error)) => Err(error),
        Err(ValueEncodeError::UnsupportedType) => {
            unreachable!("domain-admitted value has no canonical encoding")
        }
    }
}

#[cfg(test)]
mod tests {
    use core::convert::TryInto;

    use super::*;
    use crate::Merlin;

    fn bytes(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let s = core::str::from_utf8(pair).expect("ASCII hex");
                u8::from_str_radix(s, 16).expect("valid hex")
            })
            .collect()
    }

    fn assert_int_wire(value: Scalar, expected_hex: &str) {
        let mut encoded = Vec::new();
        write_scalar(&mut encoded, &value).expect("Vec has capacity");
        assert_eq!(encoded, bytes(expected_hex));
        let mut input = encoded.as_slice();
        assert_eq!(read_scalar(&mut input).expect("canonical vector"), value);
        assert!(input.is_empty());
    }

    #[test]
    fn golden_subvarint_width_boundaries() {
        for (value, expected) in [
            (0, "0000"),
            (255, "00ff"),
            (256, "010000"),
            (65_791, "01ffff"),
            (65_792, "0200000000"),
            (4_295_033_087, "02ffffffff"),
            (4_295_033_088, "030000000000000000"),
            (u64::MAX, "03fffefefffeffffff"),
        ] {
            let mut encoded = Vec::new();
            write_subvarint(&mut encoded, value).expect("Vec has capacity");
            assert_eq!(encoded, bytes(expected));
            let mut input = encoded.as_slice();
            assert_eq!(read_subvarint(&mut input).expect("canonical vector"), value);
            assert!(input.is_empty());
        }
    }

    #[test]
    fn golden_container_prefix_boundary() {
        for (kind, count, expected) in [
            (0, 58, "7e"),
            (0, 59, "7f0000"),
            (1, 58, "ba"),
            (1, 59, "bb0000"),
            (2, 58, "f6"),
            (2, 59, "f70000"),
        ] {
            let mut encoded = Vec::new();
            match kind {
                0 => {
                    let value = String::from(vec![0u8; count]);
                    write_string(&mut encoded, &value).expect("Vec has capacity");
                    encoded.truncate(encoded.len() - count);
                }
                1 => write_list_prefix(&mut encoded, count).expect("Vec has capacity"),
                _ => write_dict_prefix(&mut encoded, count).expect("Vec has capacity"),
            }
            assert_eq!(encoded, bytes(expected));
        }
    }

    #[test]
    fn golden_scalar_width_boundaries() {
        let near_zero_full = Scalar::from(PU64_BASE as u128 + u64::MAX as u128 + 1);
        let near_order_full = -Scalar::from(NU64_BASE as u128 + u64::MAX as u128 + 1);
        for (value, expected) in [
            (Scalar::ZERO, "00"),
            (Scalar::from(58u64), "3a"),
            (Scalar::from(59u64), "3b00"),
            (Scalar::from(314u64), "3bff"),
            (Scalar::from(315u64), "3c00000000"),
            (Scalar::from(PU32_TOP), "3cffffffff"),
            (Scalar::from(PU64_BASE), "3d0000000000000000"),
            (
                Scalar::from(PU64_BASE) + Scalar::from(u64::MAX),
                "3dffffffffffffffff",
            ),
            (
                near_zero_full,
                "3e3b01000001000000010000000000000000000000000000000000000000000000",
            ),
            (Scalar::from(-1i64), "3f"),
            (Scalar::from(-2i64), "4000"),
            (Scalar::from(-257i64), "40ff"),
            (Scalar::from(-258i64), "4100000000"),
            (-Scalar::from(NU32_TOP), "41ffffffff"),
            (-Scalar::from(NU64_BASE), "420000000000000000"),
            (
                -(Scalar::from(NU64_BASE) + Scalar::from(u64::MAX)),
                "42ffffffffffffffff",
            ),
            (
                near_order_full,
                "3eebd2f55c19631258d59cf7a2def9de1400000000000000000000000000000010",
            ),
        ] {
            assert_int_wire(value, expected);
        }
    }

    #[test]
    fn golden_supported_value_tags() {
        let values = [
            (Value::Scalar(Scalar::from(7u64)), "07".to_owned()),
            (
                Value::String(String::from(vec![0xaa, 0xbb])),
                "46aabb".to_owned(),
            ),
            (
                Value::Dict(Dict::from_values(vec![
                    Value::Scalar(Scalar::ONE),
                    Value::Scalar(Scalar::from(2u64)),
                ])),
                "820102".to_owned(),
            ),
            (
                {
                    let mut dict = Dict::new();
                    dict.insert(Scalar::from(2u64), Value::Scalar(Scalar::from(3u64)));
                    Value::Dict(dict)
                },
                "bd0203".to_owned(),
            ),
            (
                Value::Point(Point::from_compressed(CompressedRistretto([0x11; 32]))),
                format!("f8{}", "11".repeat(32)),
            ),
            (
                Value::Token(Token::new(
                    Commitment::Closed(CompressedRistretto([0x22; 32])),
                    Commitment::Closed(CompressedRistretto([0x33; 32])),
                )),
                format!("f9{}{}", "22".repeat(32), "33".repeat(32)),
            ),
            (
                Value::ClearToken(ClearToken::new(Scalar::from(-1i64), Scalar::ZERO)),
                "fa3f00".to_owned(),
            ),
        ];
        for (value, expected) in values {
            let mut encoded = Vec::new();
            write_value(&mut encoded, &value).expect("supported value encodes");
            assert_eq!(encoded, bytes(&expected));
            let mut input = encoded.as_slice();
            assert!(read_value(&mut input).expect("canonical vector").is_some());
            assert!(input.is_empty());
        }
    }

    // ── Scalar canonicality: decoder rejection ───────────────────

    #[test]
    fn scalar_full_decoder_rejects_low_value() {
        // FULL must encode a value > PU64 range. A small residue
        // in FULL form should be rejected by the decoder.
        let mut buf = Vec::new();
        buf.push(INT_FULL);
        let small = Scalar::from(42u64);
        buf.extend_from_slice(&small.to_bytes());
        let mut r = buf.as_slice();
        assert!(matches!(read_scalar(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn scalar_full_decoder_rejects_compact_endpoint_boundaries() {
        // Both endpoint ranges must use their shorter width tags.
        for value in [
            Scalar::from(PU64_BASE) + Scalar::from(u64::MAX),
            -(Scalar::from(NU64_BASE) + Scalar::from(u64::MAX)),
            -Scalar::ONE,
        ] {
            let mut buf = vec![INT_FULL];
            buf.extend_from_slice(&value.to_bytes());
            assert!(matches!(
                read_scalar(&mut buf.as_slice()),
                Err(ReadError::InvalidFormat)
            ));
        }
    }

    #[test]
    fn scalar_full_decoder_rejects_noncanonical_residues() {
        let modulus = bytes("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
        for payload in [modulus, vec![0xff; 32]] {
            let mut buf = vec![INT_FULL];
            buf.extend_from_slice(&payload);
            assert!(matches!(
                read_scalar(&mut buf.as_slice()),
                Err(ReadError::InvalidFormat)
            ));
        }
    }

    #[test]
    fn scalar_full_roundtrips_across_centered_boundary() {
        let half_order = Scalar::from_bytes(
            bytes("f6e97a2e8d31092c6bce7b51ef7c6f0a00000000000000000000000000000008")
                .try_into()
                .unwrap(),
        )
        .unwrap();
        for value in [half_order, half_order + Scalar::ONE] {
            let mut encoded = Vec::new();
            write_scalar(&mut encoded, &value).unwrap();
            assert_eq!(encoded[0], INT_FULL);
            assert_eq!(&encoded[1..], value.as_bytes());
            assert_eq!(read_scalar(&mut encoded.as_slice()).unwrap(), value);
        }
    }

    #[test]
    fn reserved_scalar_tag_is_rejected() {
        let buf = [67];
        let mut r = buf.as_slice();
        assert!(matches!(read_scalar(&mut r), Err(ReadError::InvalidFormat)));
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    // ── Encoding canonicality at the Dict level ──────────────────

    #[test]
    fn write_dict_sequential_uses_list_encoding() {
        let d = Dict::from_values(vec![
            Value::Scalar(Scalar::from(10u64)),
            Value::Scalar(Scalar::from(20u64)),
        ]);
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        assert!(buf[0] >= LIST_IMM_MIN && buf[0] <= LIST_IMM_MAX);
    }

    #[test]
    fn write_dict_non_sequential_uses_dict_encoding() {
        let mut d = Dict::new();
        d.insert(Scalar::from(10u64), Value::Scalar(Scalar::from(1u64)));
        d.insert(Scalar::from(20u64), Value::Scalar(Scalar::from(2u64)));
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        assert!(buf[0] >= DICT_IMM_MIN && buf[0] <= DICT_IMM_MAX);
    }

    #[test]
    fn write_dict_ignores_sticky_portability_metadata() {
        let mut d = Dict::new();
        d.insert(Scalar::ZERO, Value::Merlin(Merlin::new(b"test")));
        d.remove(&Scalar::ZERO);
        assert!(d.is_empty());
        assert!(!d.is_portable());
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        assert_eq!(buf, vec![LIST_IMM_MIN]);
    }

    #[test]
    fn read_value_dict_rejects_sequential_keys_in_dict_form() {
        // Manually craft a dict-style payload with keys [0, 1] —
        // they should have been list-style.
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_scalar(&mut buf, &Scalar::from(0u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(7u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(1u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(8u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_dict_rejects_out_of_order_keys() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_scalar(&mut buf, &Scalar::from(5u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(0u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(2u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(0u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_dict_rejects_duplicate_keys() {
        let mut buf = Vec::new();
        write_dict_prefix(&mut buf, 2).unwrap();
        write_scalar(&mut buf, &Scalar::from(7u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(0u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(7u64)).unwrap();
        write_scalar(&mut buf, &Scalar::from(0u64)).unwrap();
        let mut r = buf.as_slice();
        assert!(matches!(read_value(&mut r), Err(ReadError::InvalidFormat)));
    }

    #[test]
    fn read_value_unimplemented_tags_reject() {
        for tag in RESERVED_VALUE_TAG_MIN..=255 {
            let bytes = [tag];
            let mut input = bytes.as_slice();
            assert!(matches!(
                read_value(&mut input),
                Err(ReadError::InvalidFormat)
            ));
        }
    }

    #[test]
    fn centered_debit_cleartoken_encoding_is_domain_neutral() {
        let value = Value::ClearToken(ClearToken::new(Scalar::from(-1i64), Scalar::ZERO));
        let mut encoded = Vec::new();
        write_value(&mut encoded, &value).unwrap();

        let mut r = encoded.as_slice();
        let decoded = read_value(&mut r).unwrap().unwrap();
        assert!(r.is_empty());
        assert!(!decoded.is_portable());
        match decoded {
            Value::ClearToken(token) => {
                assert_eq!(token.qty(), Scalar::from(-1i64));
                assert_eq!(token.flv(), Scalar::ZERO);
            }
            other => panic!("expected ClearToken, got {:?}", other),
        }
    }

    #[test]
    fn nested_nonportable_dict_roundtrips_for_diagnostics() {
        let mut inner = Dict::new();
        inner.insert(
            Scalar::ZERO,
            Value::ClearToken(ClearToken::new(Scalar::from(-1i64), Scalar::ZERO)),
        );
        let mut outer = Dict::new();
        outer.insert(Scalar::ZERO, Value::Dict(inner));

        let mut encoded = Vec::new();
        write_value(&mut encoded, &Value::Dict(outer)).unwrap();
        let decoded = read_value(&mut encoded.as_slice()).unwrap().unwrap();
        assert!(!decoded.is_portable());
    }

    #[test]
    fn unsupported_value_is_not_reported_as_writer_capacity() {
        let mut encoded = Vec::new();
        assert_eq!(
            write_value(&mut encoded, &Value::Merlin(Merlin::new(b"test"))),
            Err(ValueEncodeError::UnsupportedType)
        );
        assert!(encoded.is_empty());
    }

    #[test]
    fn token_encode_decode_roundtrip() {
        // Build a Token with cleartext qty/flv → encode → decode →
        // re-encode → bytes equal. Decoded Token holds Closed
        // commitments; original holds Open ones, so we compare bytes
        // and structural shape rather than struct equality.
        let original = Value::Token(
            Token::cleartext(Scalar::from(123u64), Scalar::from(7u64))
                .expect("test quantity is in range"),
        );
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
                let expected_qty = Commitment::unblinded(Scalar::from(123u64));
                let expected_flv = Commitment::unblinded(Scalar::from(7u64));
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
        let original = Value::ClearToken(ClearToken::new(Scalar::from(5u64), Scalar::from(7u64)));
        let mut buf = Vec::new();
        write_value(&mut buf, &original).expect("encodes");
        assert_eq!(buf[0], CLEAR_TOKEN_TAG);

        let mut r = buf.as_slice();
        let decoded = read_value(&mut r)
            .expect("decodes")
            .expect("cleartoken tag");
        match &decoded {
            Value::ClearToken(t) => {
                assert_eq!(t.qty(), Scalar::from(5u64));
                assert_eq!(t.flv(), Scalar::from(7u64));
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
