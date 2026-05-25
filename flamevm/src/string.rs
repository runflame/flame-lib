//! Variable-length binary string; carries optional prover-side witness payloads.

use std::borrow::Cow;

use crate::constraints::Commitment;
use crate::errors::VMError;
use crate::int253::Int253;
use crate::ops::Instruction;

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
    /// Prover-side sub-script: a decoded instruction stream with
    /// witness slots intact. Encodes to the compiled bytecode.
    /// Consumed by `op_run`, `op_switch`, `op_signrun` via
    /// [`String::to_instructions`] — verifier sees `Opaque(bytes)`
    /// and parses, prover keeps witnesses inline.
    Script(Vec<Instruction>),
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

    /// Constructs a witness-bearing Script-String. Used by the
    /// prover when pushing a sub-script that contains witnesses
    /// (e.g. inner `alloc(Some(_))` / `input(Some(_))` calls) and
    /// will later be consumed by `run` / `switch` / `signrun`.
    pub fn script(instructions: Vec<Instruction>) -> String {
        String::Script(instructions)
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
            String::Script(instrs) => Cow::Owned(compile_instructions(instrs)),
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
            String::Script(instrs) => compile_instructions(&instrs),
        }
    }

    /// Non-consuming variant of [`String::to_bytes`]. Always
    /// allocates, even for `Opaque`.
    pub fn to_bytes_vec(&self) -> Vec<u8> {
        self.bytes_view().into_owned()
    }

    /// Length in canonical wire bytes. For witness-bearing variants
    /// this is the encoded-form length (32 bytes for Commitment,
    /// Scalar, Predicate; compiled bytecode length for Script).
    pub fn len(&self) -> usize {
        match self {
            String::Opaque(d) => d.len(),
            String::Commitment(_) | String::Scalar(_) | String::Predicate(_) => 32,
            String::Script(instrs) => compile_instructions(instrs).len(),
        }
    }

    /// True iff this String's canonical bytes are empty.
    pub fn is_empty(&self) -> bool {
        match self {
            String::Opaque(d) => d.is_empty(),
            String::Script(instrs) => instrs.is_empty(),
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

    /// Downcasts to a `Vec<Instruction>` — the runtime form the VM
    /// walks. For `Script(instrs)`, returns the witness-bearing
    /// instructions directly (prover side); for `Opaque(bytes)`,
    /// parses the bytes via `Program::parse` (verifier side, or
    /// for scripts that came from the wire); errors for other
    /// variants since they're 32-byte points/scalars, not
    /// executable bytecode.
    ///
    /// Used by `op_run`, `op_switch`, `op_signrun` to enter a
    /// sub-script — letting the prover keep witnesses inline
    /// across nested programs.
    pub fn to_instructions(self) -> Result<Vec<Instruction>, VMError> {
        match self {
            String::Script(instrs) => Ok(instrs),
            String::Opaque(data) => Ok(
                crate::program::Program::parse(&data)?.into_instructions(),
            ),
            _ => Err(VMError::TypeNotString),
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

    /// Bitwise OR. Returns `None` if operand lengths differ.
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

    /// Bitwise AND. Returns `None` if operand lengths differ.
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

    /// Bitwise XOR. Returns `None` if operand lengths differ.
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

// ── Internal: compile a Script-string's instruction stream to its
// canonical bytecode (the same bytes the verifier would see). Used
// by `bytes_view`, `to_bytes`, `len` for `String::Script`. ───────

fn compile_instructions(instrs: &[Instruction]) -> Vec<u8> {
    let mut out = Vec::new();
    for instr in instrs {
        instr.encode(&mut out);
    }
    out
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
