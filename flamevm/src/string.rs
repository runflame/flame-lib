//! Variable-length binary string; carries optional prover-side witness payloads.

use std::borrow::Cow;
use std::sync::Arc;

use crate::constraints::Commitment;
use crate::errors::VMError;
use crate::int253::Int253;
use crate::ops::Instruction;

/// Variable-length binary string with optional witness-bearing
/// variants. See module docs for the design.
///
/// `Clone` and `Debug` are implemented manually so the `Cell`
/// variant — whose inner `Cell` carries non-clonable, non-debuggable
/// payload values — can share ownership via `Arc` on clone (cheap
/// refcount bump, witnesses preserved) and print as the cell id on
/// debug. The cheap-Box variants (Commitment / Scalar / Predicate /
/// Script) clone in O(1) and keep their witnesses normally.
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
    /// Consumed by `op_run`, `op_switch`, `op_signcall` via
    /// [`String::to_instructions`] — verifier sees `Opaque(bytes)`
    /// and parses, prover keeps witnesses inline.
    Script(Vec<Instruction>),
    /// Prover-side cell with witness-bearing `Commitment::Open`
    /// quantities/flavors on its Token payloads. Encodes to the
    /// canonical cell bytes — verifier sees `Opaque(bytes)` and
    /// decodes via `Cell::decode` to closed commitments. Consumed
    /// by `op_input` via [`String::to_cell`].
    ///
    /// Held behind `Arc` so cloning the wrapping `String` (e.g.
    /// when `Run::next_instruction` clones the `PushStr` operand
    /// on every step) is a cheap refcount bump that preserves
    /// witnesses — `Cell` itself is non-Clonable because its
    /// payload may hold linear types.
    Cell(Arc<crate::cell::Cell>),
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
    /// (e.g. inner `alloc(Some(_))` calls) and will later be
    /// consumed by `run` / `switch` / `signcall`.
    pub fn script(instructions: Vec<Instruction>) -> String {
        String::Script(instructions)
    }

    /// Constructs a witness-bearing Cell-String. Used by the prover
    /// before `op_input` to push a cell whose Token payloads still
    /// carry `Commitment::Open` quantities/flavors. The verifier-side
    /// equivalent is `String::Opaque(cell.to_bytes())`.
    pub fn cell(c: crate::cell::Cell) -> String {
        String::Cell(Arc::new(c))
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
            String::Cell(c) => Cow::Owned(c.to_bytes()),
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
            String::Cell(c) => c.to_bytes(),
        }
    }

    /// Non-consuming variant of [`String::to_bytes`]. Always
    /// allocates, even for `Opaque`.
    pub fn to_bytes_vec(&self) -> Vec<u8> {
        self.bytes_view().into_owned()
    }

    /// Length in canonical wire bytes. For witness-bearing variants
    /// this is the encoded-form length (32 bytes for Commitment,
    /// Scalar, Predicate; compiled bytecode length for Script;
    /// serialized cell length for Cell).
    pub fn len(&self) -> usize {
        match self {
            String::Opaque(d) => d.len(),
            String::Commitment(_) | String::Scalar(_) | String::Predicate(_) => 32,
            String::Script(instrs) => compile_instructions(instrs).len(),
            String::Cell(c) => c.to_bytes().len(),
        }
    }

    /// True iff this String's canonical bytes are empty.
    pub fn is_empty(&self) -> bool {
        match self {
            String::Opaque(d) => d.is_empty(),
            String::Script(instrs) => instrs.is_empty(),
            // Commitment / Scalar / Predicate are 32 bytes; Cell has a
            // non-empty header → never empty.
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
    /// Used by `op_run`, `op_switch`, `op_signcall` to enter a
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

    /// Downcasts to a `Cell`. For `String::Cell(c)`, returns the
    /// witness-bearing cell directly (Token payloads keep their
    /// `Commitment::Open` quantities/flavors). For `Opaque`, decodes
    /// the canonical wire bytes via `Cell::decode` (yields
    /// `Commitment::Closed`). Hard-fails `MalformedCellEncoding` on
    /// malformed bytes, trailing data, or any non-decodable variant.
    ///
    /// Used by `op_input` — matches zkvm's `String::to_output` shape.
    ///
    /// For `String::Cell`, the cell is held in an `Arc` (so clones
    /// of the wrapping `String` share ownership and preserve
    /// witnesses). If this is the sole reference (the common case
    /// — pushed once, consumed by `op_input` once), `Arc::try_unwrap`
    /// returns the owned cell directly. If the Arc has been cloned
    /// (rare — `dup` on a witness-bearing pushed cell), we fall back
    /// to encoding the shared cell to canonical bytes and decoding a
    /// fresh closed-commitment copy — the witness is lost only in
    /// this multi-reference path, and only for this consumer.
    pub fn to_cell(self) -> Result<crate::cell::Cell, VMError> {
        match self {
            String::Cell(arc) => match Arc::try_unwrap(arc) {
                Ok(cell) => Ok(cell),
                // Shared Arc — happens in the common path because
                // `Run::next_instruction` clones the `PushStr` operand
                // (the original ref is pinned in `instructions[]` for
                // potential `loop` rewinds). Deep-clone the cell so
                // witnesses survive into op_input.
                Err(shared) => shared.try_clone_with_witnesses(),
            },
            String::Opaque(data) => {
                let mut reader: &[u8] = &data;
                let cell = crate::cell::Cell::decode(&mut reader)?;
                if !reader.is_empty() {
                    return Err(VMError::MalformedCellEncoding);
                }
                Ok(cell)
            }
            _ => Err(VMError::MalformedCellEncoding),
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

impl Clone for String {
    fn clone(&self) -> Self {
        match self {
            String::Opaque(d) => String::Opaque(d.clone()),
            String::Commitment(c) => String::Commitment(c.clone()),
            String::Scalar(s) => String::Scalar(s.clone()),
            String::Predicate(p) => String::Predicate(p.clone()),
            String::Script(i) => String::Script(i.clone()),
            // Arc bump — witnesses survive cloning (the underlying
            // Cell is shared, not deep-copied). Needed so the VM's
            // per-step instruction clone in `Run::next_instruction`
            // doesn't degrade a witness-bearing pushed cell.
            String::Cell(c) => String::Cell(Arc::clone(c)),
        }
    }
}

impl std::fmt::Debug for String {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            String::Opaque(d) => f.debug_tuple("Opaque").field(d).finish(),
            String::Commitment(c) => f.debug_tuple("Commitment").field(c).finish(),
            String::Scalar(s) => f.debug_tuple("Scalar").field(s).finish(),
            String::Predicate(p) => f.debug_tuple("Predicate").field(p).finish(),
            String::Script(i) => f.debug_tuple("Script").field(i).finish(),
            // Cell isn't Debug-derived; print its canonical id (in
            // hex) as a surrogate so test output stays readable.
            String::Cell(c) => {
                let id = c.id();
                let mut hex = std::string::String::with_capacity(64);
                for b in id.iter() {
                    use std::fmt::Write;
                    let _ = write!(hex, "{:02x}", b);
                }
                f.debug_struct("Cell").field("id", &hex).finish()
            }
        }
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
