//! Compact encoding for flamevm types.
//!
//! ## Immediate value set
//!
//! Indices 0..=58 encode values from the set {0,1,...,55, 64, 128, 256}.
//! The first 56 indices (0..=55) are identity-mapped, then:
//!   index 56 => 64, index 57 => 128, index 58 => 256.
//!
//! ## Type tags (single-byte prefix)
//!
//! ```text
//! Ints:
//!   0..=58    immediate non-negative integer from the set above
//!   59        full 256-bit signed Integer (32 bytes follow)
//!   60        short -1
//!   61        varlen  8-bit unsigned (1 byte follows)
//!   62        varlen 16-bit unsigned LE (2 bytes follow)
//!   63        varlen 32-bit unsigned LE (4 bytes follow)
//!   64        varlen 64-bit unsigned LE (8 bytes follow)
//!   65        varlen 128-bit unsigned LE (16 bytes follow)
//!
//! Strings:
//!   66..=124  immediate-length string (len from set, then `len` bytes)
//!   125       compactint-prefixed string
//!
//! Lists (structs, list-style):
//!   126..=184 immediate-length list (count from set, then `count` items)
//!   185       compactint-prefixed list
//!
//! Dicts (structs, dict-style):
//!   186..=244 immediate-length dict (count from set, then `count` pairs)
//!   245       compactint-prefixed dict
//!
//! Fixed-size objects:
//!   246       Point (32 bytes)
//!   247..=255 Token, ClearToken, WideToken, Object, Merlin, ...
//! ```

use curve25519_dalek::ristretto::CompressedRistretto;
pub use readerwriter::{ReadError, Reader, WriteError, Writer};

use crate::crypto::Point;
use crate::dict::Dict;
use crate::integer::Integer;
use crate::string::String;
use crate::value::Value;

// ── Immediate value set ────────────────────────────────────────────
// {0,1,...,55, 64, 128, 256} — 59 values, indices 0..=58.

const NUM_IMMEDIATES: usize = 59;

/// Maps index 0..=58 to the immediate value.
const IMM_VALUES: [usize; NUM_IMMEDIATES] = {
    let mut t = [0usize; NUM_IMMEDIATES];
    let mut i = 0;
    while i < 56 {
        t[i] = i;
        i += 1;
    }
    t[56] = 64;
    t[57] = 128;
    t[58] = 256;
    t
};

/// Returns Some(index) if `val` is in the immediate set, None otherwise.
fn imm_index(val: usize) -> Option<u8> {
    if val <= 55 {
        Some(val as u8)
    } else if val == 64 {
        Some(56)
    } else if val == 128 {
        Some(57)
    } else if val == 256 {
        Some(58)
    } else {
        None
    }
}

/// Decodes an immediate index (0..=58) to the value.
fn imm_value(index: u8) -> usize {
    IMM_VALUES[index as usize]
}

// ── Tag constants ──────────────────────────────────────────────────

const IMM_MAX_INDEX: u8 = 58;

// Integers
const INT_FULL: u8 = 59;
const INT_NEG1: u8 = 60;
const INT_U8: u8 = 61;
const INT_U16: u8 = 62;
const INT_U32: u8 = 63;
const INT_U64: u8 = 64;
const INT_U128: u8 = 65;

// Strings
const STR_IMM_MIN: u8 = 66;
const STR_IMM_MAX: u8 = 124;
const STR_VAR: u8 = 125;

// Lists
const LIST_IMM_MIN: u8 = 126;
const LIST_IMM_MAX: u8 = 184;
const LIST_VAR: u8 = 185;

// Dicts
const DICT_IMM_MIN: u8 = 186;
const DICT_IMM_MAX: u8 = 244;
const DICT_VAR: u8 = 245;

// Fixed-size objects
const POINT_TAG: u8 = 246;
const TOKEN_TAG: u8 = 247;
const CLEAR_TOKEN_TAG: u8 = 248;
const WIDE_TOKEN_TAG: u8 = 249;
const OBJECT_TAG: u8 = 250;
const MERLIN_TAG: u8 = 251;

// ── Compactint helpers ─────────────────────────────────────────────
// Variable-length unsigned integer for length prefixes (after a VAR tag).
// Uses the same immediate set for indices 0..=58,
// then varlen tags for larger values.

const COMPACT_IMM_MAX: u8 = 58;
const COMPACT_U8: u8 = 59;
const COMPACT_U16: u8 = 60;
const COMPACT_U32: u8 = 61;
const COMPACT_U64: u8 = 62;

fn write_compactint(w: &mut impl Writer, n: usize) -> Result<(), WriteError> {
    if let Some(idx) = imm_index(n) {
        w.write_u8(b"compactint", idx)
    } else if n <= u8::MAX as usize {
        w.write_u8(b"compactint", COMPACT_U8)?;
        w.write_u8(b"compactint.u8", n as u8)
    } else if n <= u16::MAX as usize {
        w.write_u8(b"compactint", COMPACT_U16)?;
        w.write(b"compactint.u16", &(n as u16).to_le_bytes())
    } else if n <= u32::MAX as usize {
        w.write_u8(b"compactint", COMPACT_U32)?;
        w.write(b"compactint.u32", &(n as u32).to_le_bytes())
    } else {
        w.write_u8(b"compactint", COMPACT_U64)?;
        w.write_u64(b"compactint.u64", n as u64)
    }
}

fn read_compactint(r: &mut impl Reader) -> Result<usize, ReadError> {
    let tag = r.read_u8()?;
    match tag {
        0..=COMPACT_IMM_MAX => Ok(imm_value(tag)),
        COMPACT_U8 => Ok(r.read_u8()? as usize),
        COMPACT_U16 => {
            let mut buf = [0u8; 2];
            r.read(&mut buf)?;
            Ok(u16::from_le_bytes(buf) as usize)
        }
        COMPACT_U32 => Ok(r.read_u32()? as usize),
        COMPACT_U64 => Ok(r.read_u64()? as usize),
        _ => Err(ReadError::InvalidFormat),
    }
}

// ── Integer encoding ───────────────────────────────────────────────

/// Writes an `Integer` in compact form.
pub fn write_integer(w: &mut impl Writer, int: &Integer) -> Result<(), WriteError> {
    if int.is_negative() {
        if *int == Integer::from(-1i64) {
            return w.write_u8(b"int.tag", INT_NEG1);
        }
        w.write_u8(b"int.tag", INT_FULL)?;
        w.write(b"int.full", &int.to_bytes())
    } else {
        let bytes = int.as_bytes();
        let width = classify_width(bytes);
        match width {
            IntWidth::Imm(idx) => w.write_u8(b"int.tag", idx),
            IntWidth::U8(v) => {
                w.write_u8(b"int.tag", INT_U8)?;
                w.write_u8(b"int.u8", v)
            }
            IntWidth::U16(b2) => {
                w.write_u8(b"int.tag", INT_U16)?;
                w.write(b"int.u16", &b2)
            }
            IntWidth::U32(b4) => {
                w.write_u8(b"int.tag", INT_U32)?;
                w.write(b"int.u32", &b4)
            }
            IntWidth::U64(b8) => {
                w.write_u8(b"int.tag", INT_U64)?;
                w.write(b"int.u64", &b8)
            }
            IntWidth::U128(b16) => {
                w.write_u8(b"int.tag", INT_U128)?;
                w.write(b"int.u128", &b16)
            }
            IntWidth::Full => {
                w.write_u8(b"int.tag", INT_FULL)?;
                w.write(b"int.full", &int.to_bytes())
            }
        }
    }
}

/// Reads a compact-encoded `Integer`.
pub fn read_integer(r: &mut impl Reader) -> Result<Integer, ReadError> {
    let tag = r.read_u8()?;
    read_integer_with_tag(r, tag)
}

fn read_integer_with_tag(r: &mut impl Reader, tag: u8) -> Result<Integer, ReadError> {
    match tag {
        0..=IMM_MAX_INDEX => Ok(Integer::from(imm_value(tag) as u64)),
        INT_FULL => {
            let buf = r.read_u8x32()?;
            Integer::from_bytes(buf).ok_or(ReadError::InvalidFormat)
        }
        INT_NEG1 => Ok(Integer::from(-1i64)),
        INT_U8 => Ok(Integer::from(r.read_u8()? as u64)),
        INT_U16 => {
            let mut buf = [0u8; 2];
            r.read(&mut buf)?;
            Ok(Integer::from(u16::from_le_bytes(buf) as u64))
        }
        INT_U32 => Ok(Integer::from(r.read_u32()? as u64)),
        INT_U64 => Ok(Integer::from(r.read_u64()?)),
        INT_U128 => {
            let mut buf = [0u8; 32];
            r.read(&mut buf[..16])?;
            Integer::from_bytes(buf).ok_or(ReadError::InvalidFormat)
        }
        _ => Err(ReadError::InvalidFormat),
    }
}

// ── String encoding ────────────────────────────────────────────────

/// Writes a `String` (byte-string) in compact form.
pub fn write_string(w: &mut impl Writer, s: &String) -> Result<(), WriteError> {
    let len = s.as_bytes().len();
    if let Some(idx) = imm_index(len) {
        w.write_u8(b"str.tag", STR_IMM_MIN + idx)?;
    } else {
        w.write_u8(b"str.tag", STR_VAR)?;
        write_compactint(w, len)?;
    }
    w.write(b"str.data", s.as_bytes())
}

/// Reads a compact-encoded `String`.
pub fn read_string(r: &mut impl Reader) -> Result<String, ReadError> {
    let tag = r.read_u8()?;
    read_string_with_tag(r, tag)
}

fn read_string_with_tag(r: &mut impl Reader, tag: u8) -> Result<String, ReadError> {
    let len = match tag {
        STR_IMM_MIN..=STR_IMM_MAX => imm_value(tag - STR_IMM_MIN),
        STR_VAR => read_compactint(r)?,
        _ => return Err(ReadError::InvalidFormat),
    };
    let data = r.read_bytes(len)?;
    Ok(String::from(data))
}

// ── List / Dict length encoding ────────────────────────────────────

/// Writes the tag + count prefix for a list.
pub fn write_list_prefix(w: &mut impl Writer, count: usize) -> Result<(), WriteError> {
    if let Some(idx) = imm_index(count) {
        w.write_u8(b"list.tag", LIST_IMM_MIN + idx)
    } else {
        w.write_u8(b"list.tag", LIST_VAR)?;
        write_compactint(w, count)
    }
}

/// Reads the count from a list tag + optional compactint.
pub fn read_list_prefix(r: &mut impl Reader) -> Result<usize, ReadError> {
    let tag = r.read_u8()?;
    read_list_count_with_tag(r, tag)
}

fn read_list_count_with_tag(r: &mut impl Reader, tag: u8) -> Result<usize, ReadError> {
    match tag {
        LIST_IMM_MIN..=LIST_IMM_MAX => Ok(imm_value(tag - LIST_IMM_MIN)),
        LIST_VAR => read_compactint(r),
        _ => Err(ReadError::InvalidFormat),
    }
}

/// Writes the tag + count prefix for a dict.
pub fn write_dict_prefix(w: &mut impl Writer, count: usize) -> Result<(), WriteError> {
    if let Some(idx) = imm_index(count) {
        w.write_u8(b"dict.tag", DICT_IMM_MIN + idx)
    } else {
        w.write_u8(b"dict.tag", DICT_VAR)?;
        write_compactint(w, count)
    }
}

/// Reads the count from a dict tag + optional compactint.
pub fn read_dict_prefix(r: &mut impl Reader) -> Result<usize, ReadError> {
    let tag = r.read_u8()?;
    read_dict_count_with_tag(r, tag)
}

fn read_dict_count_with_tag(r: &mut impl Reader, tag: u8) -> Result<usize, ReadError> {
    match tag {
        DICT_IMM_MIN..=DICT_IMM_MAX => Ok(imm_value(tag - DICT_IMM_MIN)),
        DICT_VAR => read_compactint(r),
        _ => Err(ReadError::InvalidFormat),
    }
}

// ── Value encoding ─────────────────────────────────────────────────

/// Reads a `Value` from the stream.
/// Returns `Ok(None)` for recognized but unimplemented type tags
/// (tokens, objects, merlin, etc.).
/// Returns `Err` for malformed data.
pub fn read_value(r: &mut impl Reader) -> Result<Option<Value>, ReadError> {
    let tag = r.read_u8()?;
    match tag {
        // Integers
        0..=INT_U128 => {
            let int = read_integer_with_tag(r, tag)?;
            Ok(Some(Value::Int(int)))
        }

        // Strings
        STR_IMM_MIN..=STR_VAR => {
            let s = read_string_with_tag(r, tag)?;
            Ok(Some(Value::String(s)))
        }

        // List-style dict (sequential keys 0, 1, 2, ...)
        LIST_IMM_MIN..=LIST_VAR => {
            let count = read_list_count_with_tag(r, tag)?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                match read_value(r)? {
                    Some(v) => values.push(v),
                    None => return Ok(None),
                }
            }
            Ok(Some(Value::Dict(Dict::from_values(values))))
        }

        // Dict-style (explicit keys)
        DICT_IMM_MIN..=DICT_VAR => {
            let count = read_dict_count_with_tag(r, tag)?;
            let mut entries: Vec<(crate::Integer, Value)> = Vec::with_capacity(count);
            let mut last_key: Option<crate::Integer> = None;
            for _ in 0..count {
                let key = read_integer(r)?;
                // Reject duplicate or out-of-order keys to keep the wire encoding canonical.
                if let Some(prev) = &last_key {
                    if key.cmp(prev) != core::cmp::Ordering::Greater {
                        return Err(ReadError::InvalidFormat);
                    }
                }
                last_key = Some(key);
                match read_value(r)? {
                    Some(v) => entries.push((key, v)),
                    None => return Ok(None),
                }
            }
            // Safety: we just verified strictly ascending keys above.
            let dict = Dict::from_sorted_entries(entries)
                .map_err(|_| ReadError::InvalidFormat)?;
            Ok(Some(Value::Dict(dict)))
        }

        // Point
        POINT_TAG => {
            let buf = r.read_u8x32()?;
            Ok(Some(Value::Point(Point::from_compressed(
                CompressedRistretto(buf),
            ))))
        }

        // Unimplemented types
        TOKEN_TAG | CLEAR_TOKEN_TAG | WIDE_TOKEN_TAG | OBJECT_TAG | MERLIN_TAG => Ok(None),

        _ => Ok(None),
    }
}

/// Writes a `Dict`. Uses the compact list encoding (no keys) when
/// all keys are sequential 0, 1, 2, ...; otherwise uses the dict
/// encoding with explicit keys.
pub fn write_dict(w: &mut impl Writer, dict: &Dict) -> Result<(), WriteError> {
    let sequential = dict
        .entries()
        .iter()
        .enumerate()
        .all(|(i, (k, _))| *k == crate::Integer::from(i as u64));
    if sequential {
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

/// Writes a `Value`.
/// Returns `Err(WriteError)` for unimplemented types.
pub fn write_value(w: &mut impl Writer, val: &Value) -> Result<(), WriteError> {
    match val {
        Value::Int(i) => write_integer(w, i),
        Value::String(s) => write_string(w, s),
        Value::Dict(d) => write_dict(w, d),
        Value::Point(p) => {
            w.write_u8(b"point.tag", POINT_TAG)?;
            w.write(b"point.data", p.as_bytes())
        }
        // Unimplemented types — cannot encode.
        _ => Err(WriteError::InsufficientCapacity),
    }
}

// ── Helpers ────────────────────────────────────────────────────────

enum IntWidth {
    Imm(u8),          // immediate index
    U8(u8),
    U16([u8; 2]),
    U32([u8; 4]),
    U64([u8; 8]),
    U128([u8; 16]),
    Full,
}

/// Classifies a non-negative 32-byte scalar value into the smallest encoding.
fn classify_width(bytes: &[u8; 32]) -> IntWidth {
    // Find the highest non-zero byte.
    let mut top = 31;
    while top > 0 && bytes[top] == 0 {
        top -= 1;
    }

    // Try to read the value as a small integer and check the immediate set.
    if top <= 1 {
        let val = bytes[0] as usize | ((bytes[1] as usize) << 8);
        if let Some(idx) = imm_index(val) {
            return IntWidth::Imm(idx);
        }
    }

    match top {
        0 => IntWidth::U8(bytes[0]),
        1 => IntWidth::U16([bytes[0], bytes[1]]),
        2..=3 => {
            let mut b4 = [0u8; 4];
            b4[..=top].copy_from_slice(&bytes[..=top]);
            IntWidth::U32(b4)
        }
        4..=7 => {
            let mut b8 = [0u8; 8];
            b8[..=top].copy_from_slice(&bytes[..=top]);
            IntWidth::U64(b8)
        }
        8..=15 => {
            let mut b16 = [0u8; 16];
            b16[..=top].copy_from_slice(&bytes[..=top]);
            IntWidth::U128(b16)
        }
        _ => IntWidth::Full,
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

    // ── Immediate set tests ────────────────────────────────────────

    #[test]
    fn imm_set_identity() {
        for v in 0..=55usize {
            assert_eq!(imm_index(v), Some(v as u8));
            assert_eq!(imm_value(v as u8), v);
        }
    }

    #[test]
    fn imm_set_powers() {
        assert_eq!(imm_index(64), Some(56));
        assert_eq!(imm_index(128), Some(57));
        assert_eq!(imm_index(256), Some(58));
        assert_eq!(imm_value(56), 64);
        assert_eq!(imm_value(57), 128);
        assert_eq!(imm_value(58), 256);
    }

    #[test]
    fn imm_set_gaps() {
        // Values between 55 and 64 (exclusive) are NOT immediate.
        for v in [56usize, 57, 58, 59, 60, 61, 62, 63] {
            assert_eq!(imm_index(v), None);
        }
        // Values between 64 and 128 (exclusive) are NOT immediate.
        for v in [65usize, 100, 127] {
            assert_eq!(imm_index(v), None);
        }
        // Values between 128 and 256 (exclusive) are NOT immediate.
        assert_eq!(imm_index(200), None);
        assert_eq!(imm_index(255), None);
        // Values above 256 are NOT immediate.
        assert_eq!(imm_index(257), None);
        assert_eq!(imm_index(512), None);
    }

    // ── Integer encoding ───────────────────────────────────────────

    #[test]
    fn int_immediate_contiguous() {
        for v in 0u64..=55 {
            let i = Integer::from(v);
            let mut buf = Vec::new();
            write_integer(&mut buf, &i).unwrap();
            assert_eq!(buf.len(), 1, "value {} should be 1 byte", v);
            assert_eq!(buf[0], v as u8);
            assert_eq!(roundtrip_int(i), i);
        }
    }

    #[test]
    fn int_immediate_powers() {
        for (val, expected_tag) in [(64u64, 56u8), (128, 57), (256, 58)] {
            let i = Integer::from(val);
            let mut buf = Vec::new();
            write_integer(&mut buf, &i).unwrap();
            assert_eq!(buf.len(), 1, "value {} should be 1 byte", val);
            assert_eq!(buf[0], expected_tag);
            assert_eq!(roundtrip_int(i), i);
        }
    }

    #[test]
    fn int_non_immediate_uses_varlen() {
        // 56 is NOT immediate — should use u8 varlen.
        let i = Integer::from(56u64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U8);
        assert_eq!(buf.len(), 2);
        assert_eq!(roundtrip_int(i), i);

        // 63 is NOT immediate.
        let i = Integer::from(63u64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U8);
        assert_eq!(roundtrip_int(i), i);

        // 65 is NOT immediate.
        let i = Integer::from(65u64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U8);
        assert_eq!(roundtrip_int(i), i);

        // 257 is NOT immediate — should use u16 varlen.
        let i = Integer::from(257u64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U16);
        assert_eq!(buf.len(), 3);
        assert_eq!(roundtrip_int(i), i);
    }

    #[test]
    fn int_u32() {
        let i = Integer::from(70000u64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U32);
        assert_eq!(buf.len(), 5);
        assert_eq!(roundtrip_int(i), i);
    }

    #[test]
    fn int_u64() {
        let i = Integer::from(u64::MAX);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U64);
        assert_eq!(buf.len(), 9);
        assert_eq!(roundtrip_int(i), i);
    }

    #[test]
    fn int_u128() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xff;
        bytes[8] = 0x01;
        let i = Integer::from_bytes(bytes).unwrap();
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_U128);
        assert_eq!(buf.len(), 17);
        assert_eq!(roundtrip_int(i), i);
    }

    #[test]
    fn int_full_positive() {
        let mut bytes = [0u8; 32];
        bytes[16] = 0x01;
        let i = Integer::from_bytes(bytes).unwrap();
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_FULL);
        assert_eq!(buf.len(), 33);
        assert_eq!(roundtrip_int(i), i);
    }

    #[test]
    fn int_neg1() {
        let i = Integer::from(-1i64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf, vec![INT_NEG1]);
        assert_eq!(roundtrip_int(i), i);
    }

    #[test]
    fn int_negative_full() {
        let i = Integer::from(-42i64);
        let mut buf = Vec::new();
        write_integer(&mut buf, &i).unwrap();
        assert_eq!(buf[0], INT_FULL);
        assert_eq!(buf.len(), 33);
        assert_eq!(roundtrip_int(i), i);
    }

    // ── String encoding ────────────────────────────────────────────

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
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], STR_IMM_MIN); // index 0 => len 0
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    #[test]
    fn string_immediate_contiguous() {
        // Length 55 => immediate index 55.
        let s = String::from(vec![0xAB; 55]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf[0], STR_IMM_MIN + 55);
        assert_eq!(buf.len(), 1 + 55);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    #[test]
    fn string_immediate_powers() {
        for (len, expected_idx) in [(64, 56u8), (128, 57), (256, 58)] {
            let s = String::from(vec![0xCD; len]);
            let mut buf = Vec::new();
            write_string(&mut buf, &s).unwrap();
            assert_eq!(buf[0], STR_IMM_MIN + expected_idx);
            assert_eq!(buf.len(), 1 + len);
            assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
        }
    }

    #[test]
    fn string_non_immediate_uses_varlen() {
        // Length 56 is NOT in the immediate set.
        let s = String::from(vec![0xEF; 56]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf[0], STR_VAR);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());

        // Length 257 is NOT immediate.
        let s = String::from(vec![0xEF; 257]);
        let mut buf = Vec::new();
        write_string(&mut buf, &s).unwrap();
        assert_eq!(buf[0], STR_VAR);
        assert_eq!(roundtrip_string(&s).as_bytes(), s.as_bytes());
    }

    // ── List / Dict prefix ─────────────────────────────────────────

    #[test]
    fn list_prefix_immediate() {
        let mut buf = Vec::new();
        write_list_prefix(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![LIST_IMM_MIN]);

        buf.clear();
        write_list_prefix(&mut buf, 55).unwrap();
        assert_eq!(buf, vec![LIST_IMM_MIN + 55]);

        buf.clear();
        write_list_prefix(&mut buf, 256).unwrap();
        assert_eq!(buf, vec![LIST_IMM_MIN + 58]);
    }

    #[test]
    fn list_prefix_varlen() {
        // 56 is not immediate.
        let mut buf = Vec::new();
        write_list_prefix(&mut buf, 56).unwrap();
        assert_eq!(buf[0], LIST_VAR);
        let mut r = &buf[1..];
        assert_eq!(read_compactint(&mut r).unwrap(), 56);
    }

    #[test]
    fn list_prefix_roundtrip() {
        for count in [0usize, 1, 55, 56, 64, 128, 256, 257, 1000, 70000] {
            let mut buf = Vec::new();
            write_list_prefix(&mut buf, count).unwrap();
            let mut r = buf.as_slice();
            assert_eq!(read_list_prefix(&mut r).unwrap(), count);
        }
    }

    #[test]
    fn dict_prefix_roundtrip() {
        for count in [0usize, 1, 55, 56, 64, 128, 256, 257, 1000, 70000] {
            let mut buf = Vec::new();
            write_dict_prefix(&mut buf, count).unwrap();
            let mut r = buf.as_slice();
            assert_eq!(read_dict_prefix(&mut r).unwrap(), count);
        }
    }

    #[test]
    fn compactint_roundtrip() {
        for n in [
            0usize, 1, 55, 56, 64, 128, 255, 256, 257, 1000, 65535, 65536,
            0xFFFFFFFF, 0x1_0000_0000,
        ] {
            let mut buf = Vec::new();
            write_compactint(&mut buf, n).unwrap();
            let mut r = buf.as_slice();
            assert_eq!(read_compactint(&mut r).unwrap(), n, "failed for n={}", n);
        }
    }

    #[test]
    fn compactint_immediate_is_one_byte() {
        for val in [0usize, 1, 55, 64, 128, 256] {
            let mut buf = Vec::new();
            write_compactint(&mut buf, val).unwrap();
            assert_eq!(buf.len(), 1, "value {} should be 1-byte compactint", val);
        }
    }

    #[test]
    fn compactint_non_immediate_is_multi_byte() {
        for val in [56usize, 63, 65, 200, 257] {
            let mut buf = Vec::new();
            write_compactint(&mut buf, val).unwrap();
            assert!(buf.len() > 1, "value {} should be multi-byte compactint", val);
        }
    }

    // ── read_value tests ───────────────────────────────────────────

    #[test]
    fn read_value_int() {
        let mut buf = Vec::new();
        write_integer(&mut buf, &Integer::from(42u64)).unwrap();
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Int(i)) => assert_eq!(i, Integer::from(42u64)),
            other => panic!("expected Int, got {:?}", other.is_some()),
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
            other => panic!("expected String, got {:?}", other.is_some()),
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
            other => panic!("expected Point, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn read_value_list_style_dict() {
        // List encoding → Dict with keys 0, 1, 2, ...
        let mut buf = Vec::new();
        write_list_prefix(&mut buf, 2).unwrap();
        write_integer(&mut buf, &Integer::from(5u64)).unwrap();
        write_integer(&mut buf, &Integer::from(10u64)).unwrap();
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Dict(d)) => {
                assert_eq!(d.len(), 2);
                let (k0, v0) = &d.entries()[0];
                assert_eq!(*k0, Integer::from(0u64));
                match v0 { Value::Int(i) => assert_eq!(*i, Integer::from(5u64)), _ => panic!("expected Int") }
                let (k1, v1) = &d.entries()[1];
                assert_eq!(*k1, Integer::from(1u64));
                match v1 { Value::Int(i) => assert_eq!(*i, Integer::from(10u64)), _ => panic!("expected Int") }
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
                let (k, v) = &d.entries()[0];
                assert_eq!(*k, Integer::from(99u64));
                match v { Value::String(s) => assert_eq!(s.as_bytes(), b"hi"), _ => panic!("expected String") }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn write_dict_sequential_uses_list_encoding() {
        let d = Dict::from_values(vec![
            Value::Int(Integer::from(10u64)),
            Value::Int(Integer::from(20u64)),
        ]);
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        // Should use list prefix, not dict prefix.
        assert!(buf[0] >= LIST_IMM_MIN && buf[0] <= LIST_IMM_MAX);
        // Roundtrip.
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Dict(d2)) => {
                assert_eq!(d2.len(), 2);
                assert_eq!(d2.entries()[0].0, Integer::from(0u64));
                assert_eq!(d2.entries()[1].0, Integer::from(1u64));
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn write_dict_non_sequential_uses_dict_encoding() {
        let mut d = Dict::new();
        d.insert(Integer::from(10u64), Value::Int(Integer::from(1u64))).unwrap();
        d.insert(Integer::from(20u64), Value::Int(Integer::from(2u64))).unwrap();
        let mut buf = Vec::new();
        write_dict(&mut buf, &d).unwrap();
        // Should use dict prefix.
        assert!(buf[0] >= DICT_IMM_MIN && buf[0] <= DICT_IMM_MAX);
        // Roundtrip.
        let mut r = buf.as_slice();
        match read_value(&mut r).unwrap() {
            Some(Value::Dict(d2)) => {
                assert_eq!(d2.len(), 2);
                let (k, _) = &d2.entries()[0];
                assert_eq!(*k, Integer::from(10u64));
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn read_value_dict_rejects_out_of_order_keys() {
        // Manually craft a dict-style payload with keys [5, 2] — descending.
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
        // Manually craft a dict-style payload with key 7 twice.
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
    fn read_value_all_two_byte_combinations_no_panic() {
        for b0 in 0u16..=255 {
            for b1 in 0u16..=255 {
                let buf = [b0 as u8, b1 as u8];
                let mut r: &[u8] = &buf;
                // Must not panic — Ok or Err are both fine.
                let _ = read_value(&mut r);
            }
        }
    }
}
