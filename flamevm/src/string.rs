//! Variable-length binary string — the VM's universal byte-bag and
//! witness carrier.
//!
//! `String` is an enum with multiple "shapes" — the prover-side
//! variants carry witness data alongside the encoded bytes:
//!
//! - `Opaque(Vec<u8>)` — verifier's view; arbitrary byte data.
//! - `Commitment(Box<Commitment>)` — prover's view of a Pedersen
//!   commitment with witness. Encodes to its 32-byte compressed point.
//! - `Scalar(Box<Int253>)` — prover's view of a scalar value. Encodes
//!   to its 32-byte sign-magnitude representation.
//! - `Predicate(Box<Predicate>)` — prover's view of an unlock
//!   predicate. Encodes to its 32-byte opaque point.
//!
//! All variants encode to the same opaque bytes on the wire — the
//! verifier always sees `Opaque(bytes)`. Downcasts (`to_commitment`,
//! `to_scalar`, `to_predicate`) work on both forms: for witness-bearing
//! variants they extract the typed payload; for `Opaque` they parse it
//! from the bytes.
//!
//! ## Sharp edge: `as_bytes(&self)` and bit operations require `Opaque`
//!
//! [`String::as_bytes`] returns a borrowed slice. For witness-bearing
//! variants there is no inner byte buffer to borrow — callers must
//! use [`String::to_bytes`] (consuming, allocates if needed) or
//! [`String::to_bytes_vec`] (non-consuming, always allocates) instead.
//! Calling `as_bytes()` on a witness-bearing variant panics with a
//! clear message. In practice, witness-bearing Strings only enter the
//! stack via prover-side `Program::push_str(rich_variant)` and are
//! immediately consumed by `op_commit` / `op_scalar` / `op_predicate`
//! via downcasts — they never reach the bit-manipulation opcodes.
//! Hashing and bit ops on the stack always operate on Opaque Strings
//! produced by `pushstr` / `read_str` / `sha*` / `keccak256` / `merlin_read`.

use std::borrow::Cow;

use crate::constraints::Commitment;
use crate::errors::VMError;
use crate::int253::Int253;

/// Variable-length binary string with optional witness-bearing
/// variants. See module docs for the design.
#[derive(Clone, Debug)]
pub enum String {
    /// Plain byte buffer — the verifier's view.
    Opaque(Vec<u8>),
    /// Pedersen commitment witness; encodes to 32-byte point.
    Commitment(Box<Commitment>),
    /// Scalar witness (cleartext `Int253`); encodes to 32 bytes.
    Scalar(Box<Int253>),
    /// Predicate witness; encodes to 32-byte point.
    Predicate(Box<crate::cell::Predicate>),
}

impl String {
    // ── Construction ────────────────────────────────────────────

    /// Constructs a witness-bearing Commitment-String.
    pub fn commitment(c: Commitment) -> String {
        String::Commitment(Box::new(c))
    }

    /// Constructs a witness-bearing Scalar-String.
    pub fn scalar<T: Into<Int253>>(s: T) -> String {
        String::Scalar(Box::new(s.into()))
    }

    /// Constructs a witness-bearing Predicate-String.
    pub fn predicate(p: crate::cell::Predicate) -> String {
        String::Predicate(Box::new(p))
    }

    // ── Byte views ──────────────────────────────────────────────

    /// Returns a borrow of the inner bytes when this is `Opaque`.
    /// Panics for witness-bearing variants — callers that may handle
    /// either form should use [`String::to_bytes`] or
    /// [`String::bytes_view`] instead.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            String::Opaque(d) => d,
            _ => panic!(
                "String::as_bytes called on witness-bearing variant; use to_bytes() or bytes_view()"
            ),
        }
    }

    /// Returns a byte view — borrowed for `Opaque`, owned for
    /// witness-bearing variants. Safe to call on any variant.
    pub fn bytes_view(&self) -> Cow<'_, [u8]> {
        match self {
            String::Opaque(d) => Cow::Borrowed(d),
            String::Commitment(c) => {
                Cow::Owned(c.to_point().as_bytes().to_vec())
            }
            String::Scalar(s) => Cow::Owned(s.to_bytes().to_vec()),
            String::Predicate(p) => Cow::Owned(p.to_point().as_bytes().to_vec()),
        }
    }

    /// Consumes self and returns the inner bytes. For `Opaque`,
    /// returns the inner Vec without allocating. For witness-bearing
    /// variants, encodes to a fresh Vec.
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            String::Opaque(d) => d,
            String::Commitment(c) => c.to_point().as_bytes().to_vec(),
            String::Scalar(s) => s.to_bytes().to_vec(),
            String::Predicate(p) => p.to_point().as_bytes().to_vec(),
        }
    }

    /// Non-consuming variant of [`String::to_bytes`]. Always
    /// allocates, even for `Opaque`.
    pub fn to_bytes_vec(&self) -> Vec<u8> {
        self.bytes_view().into_owned()
    }

    /// Length in canonical wire bytes. For witness-bearing variants
    /// this is the encoded-form length (32 bytes for Commitment,
    /// Scalar, Predicate).
    pub fn len(&self) -> usize {
        match self {
            String::Opaque(d) => d.len(),
            String::Commitment(_) | String::Scalar(_) | String::Predicate(_) => 32,
        }
    }

    /// True iff this String's canonical bytes are empty.
    pub fn is_empty(&self) -> bool {
        match self {
            String::Opaque(d) => d.is_empty(),
            _ => false,
        }
    }

    /// Converts to an `Opaque` variant. No-op for `Opaque`; serializes
    /// for witness-bearing variants. Useful before bit operations
    /// that need a raw byte buffer.
    pub fn into_opaque(self) -> String {
        match self {
            String::Opaque(_) => self,
            other => String::Opaque(other.to_bytes()),
        }
    }

    // ── Downcasts ───────────────────────────────────────────────

    /// Downcasts to a `Commitment`. For `Opaque`, parses the bytes
    /// as a 32-byte compressed Ristretto point and wraps in
    /// `Commitment::Closed`. For `String::Commitment(c)`, returns
    /// the witness directly.
    pub fn to_commitment(self) -> Result<Commitment, VMError> {
        match self {
            String::Commitment(c) => Ok(*c),
            String::Opaque(data) => {
                if data.len() != 32 {
                    return Err(VMError::TypeNotString);
                }
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&data);
                Ok(Commitment::Closed(
                    curve25519_dalek::ristretto::CompressedRistretto(bytes),
                ))
            }
            _ => Err(VMError::TypeNotString),
        }
    }

    /// Downcasts to an `Int253`. For `Opaque`, parses the bytes as a
    /// canonical 32-byte sign-magnitude `Int253`. For
    /// `String::Scalar(i)`, returns the witness directly.
    pub fn to_scalar(self) -> Result<Int253, VMError> {
        match self {
            String::Scalar(i) => Ok(*i),
            String::Opaque(data) => {
                if data.len() != 32 {
                    return Err(VMError::InvalidInt253Encoding);
                }
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&data);
                Int253::from_bytes(bytes).ok_or(VMError::InvalidInt253Encoding)
            }
            _ => Err(VMError::InvalidInt253Encoding),
        }
    }

    /// Downcasts to a `Predicate`. For `Opaque`, parses the bytes as
    /// a 32-byte compressed Ristretto point and wraps in
    /// `Predicate::Opaque`. For `String::Predicate(p)`, returns the
    /// witness directly.
    pub fn to_predicate(self) -> Result<crate::cell::Predicate, VMError> {
        match self {
            String::Predicate(p) => Ok(*p),
            String::Opaque(data) => {
                if data.len() != 32 {
                    return Err(VMError::InvalidPoint);
                }
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&data);
                Ok(crate::cell::Predicate::Opaque(
                    curve25519_dalek::ristretto::CompressedRistretto(bytes),
                ))
            }
            _ => Err(VMError::InvalidPoint),
        }
    }

    // ── Byte-level operations ───────────────────────────────────
    //
    // These always operate on canonical bytes; for witness-bearing
    // variants the bytes are serialized first. The result is always
    // a new `Opaque(Vec<u8>)`.

    /// Returns `self || other`. Used by `0x46 append`.
    pub fn append(self, other: &String) -> String {
        let mut out = self.to_bytes();
        out.extend_from_slice(&other.bytes_view());
        String::Opaque(out)
    }

    /// Returns `self || bytes`. Used by `0x44 writebits`, `0x45
    /// writeint`, `0x47 writezeros`.
    pub fn append_bytes(self, bytes: &[u8]) -> String {
        let mut out = self.to_bytes();
        out.extend_from_slice(bytes);
        String::Opaque(out)
    }

    /// Splits off the first `n` bytes. Returns `(remainder, head)` on
    /// success, `None` if the string is too short. Used by `0x42 readstr`.
    pub fn split_at(self, n: usize) -> Option<(String, String)> {
        let bytes = self.to_bytes();
        if n > bytes.len() {
            return None;
        }
        let head = bytes[..n].to_vec();
        let tail = bytes[n..].to_vec();
        Some((String::Opaque(tail), String::Opaque(head)))
    }

    /// Returns a new String with every bit inverted. Used by `0x48 bitnot`.
    pub fn bit_not(self) -> String {
        let bytes = self.to_bytes();
        String::Opaque(bytes.iter().map(|b| !b).collect())
    }

    /// Bytewise OR. Returns `None` if operand lengths differ.
    pub fn bit_or(self, other: &String) -> Option<String> {
        let a = self.to_bytes();
        let b = other.bytes_view();
        if a.len() != b.len() {
            return None;
        }
        Some(String::Opaque(
            a.iter().zip(b.iter()).map(|(x, y)| x | y).collect(),
        ))
    }

    /// Bytewise AND. Returns `None` if operand lengths differ.
    pub fn bit_and(self, other: &String) -> Option<String> {
        let a = self.to_bytes();
        let b = other.bytes_view();
        if a.len() != b.len() {
            return None;
        }
        Some(String::Opaque(
            a.iter().zip(b.iter()).map(|(x, y)| x & y).collect(),
        ))
    }

    /// Bytewise XOR. Returns `None` if operand lengths differ.
    pub fn bit_xor(self, other: &String) -> Option<String> {
        let a = self.to_bytes();
        let b = other.bytes_view();
        if a.len() != b.len() {
            return None;
        }
        Some(String::Opaque(
            a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect(),
        ))
    }

    /// Shifts bits left by `n` (treating the string as a big-endian
    /// bigint: byte 0 holds the most significant bits). Returns
    /// `(shifted, removed)`.
    ///
    /// See `bit_at` / `set_bit` for the bit-numbering convention. Used
    /// by `0x4c shiftleft`.
    pub fn shift_left(self, n: usize) -> (String, String) {
        let inner = self.to_bytes();
        let total = 8 * inner.len();
        let mut shifted = vec![0u8; inner.len()];
        for i in 0..total {
            if let Some(src) = i.checked_add(n) {
                if src < total {
                    let bit = bit_at(&inner, src);
                    set_bit(&mut shifted, i, bit);
                }
            }
        }
        let removed_bytes = (n + 7) / 8;
        let pad = removed_bytes * 8 - n;
        let mut removed = vec![0u8; removed_bytes];
        for i in 0..n {
            if i < total {
                let bit = bit_at(&inner, i);
                set_bit(&mut removed, pad + i, bit);
            }
        }
        (String::Opaque(shifted), String::Opaque(removed))
    }

    /// Shifts bits right by `n`. Mirror of [`String::shift_left`]; used
    /// by `0x4d shiftright`.
    pub fn shift_right(self, n: usize) -> (String, String) {
        let inner = self.to_bytes();
        let total = 8 * inner.len();
        let mut shifted = vec![0u8; inner.len()];
        for i in 0..total {
            if let Some(src) = i.checked_sub(n) {
                let bit = bit_at(&inner, src);
                set_bit(&mut shifted, i, bit);
            }
        }
        let removed_bytes = (n + 7) / 8;
        let mut removed = vec![0u8; removed_bytes];
        for i in 0..n {
            if let Some(src) = (total + i).checked_sub(n) {
                if src < total {
                    let bit = bit_at(&inner, src);
                    set_bit(&mut removed, i, bit);
                }
            }
        }
        (String::Opaque(shifted), String::Opaque(removed))
    }
}

impl From<Vec<u8>> for String {
    fn from(v: Vec<u8>) -> Self {
        String::Opaque(v)
    }
}

// ── Internal bit helpers (MSB-first numbering) ───────────────────

fn bit_at(bytes: &[u8], pos: usize) -> u8 {
    let byte = pos / 8;
    let bit = 7 - (pos % 8);
    if byte >= bytes.len() {
        0
    } else {
        (bytes[byte] >> bit) & 1
    }
}

fn set_bit(bytes: &mut [u8], pos: usize, value: u8) {
    let byte = pos / 8;
    let bit = 7 - (pos % 8);
    if byte >= bytes.len() {
        return;
    }
    if value & 1 != 0 {
        bytes[byte] |= 1 << bit;
    } else {
        bytes[byte] &= !(1 << bit);
    }
}
