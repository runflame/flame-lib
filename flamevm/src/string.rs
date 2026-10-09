//! Single-Cell binary string; carries optional prover-side witness payloads.

use crate::constraints::Commitment;
use crate::crypto::Point;
use crate::errors::VMError;
use crate::scalar::Scalar;

use crate::contract::{Contract, Predicate};
use crate::ops::Instruction;
use crate::script::{Script, ScriptBuilder};

/// Binary string of at most [`String::MAX_LEN`] bytes, with optional prover
/// witnesses. Host-built literals are checked at VM and encoding boundaries.
#[derive(Clone, Debug)]
pub enum String {
    /// Plain byte buffer — the verifier's view.
    Opaque(Vec<u8>),
    /// Prover-only typed data, boxed so the common `Opaque` string stays
    /// the size of a `Vec<u8>`.
    Witness(Box<StringWitness>),
}

/// Prover-side typed payload carried by [`String::Witness`]. Every variant
/// has the same canonical byte representation as an opaque String.
#[derive(Clone, Debug)]
pub enum StringWitness {
    /// Point witness (Opaque / Commitment / Predicate). Encodes to
    /// the canonical 32-byte compressed point regardless of variant;
    /// see [`Point`].
    Point(Point),
    /// Scalar witness; encodes to 32 canonical bytes.
    Scalar(Scalar),
    /// Prover-side sub-script: a decoded instruction stream with
    /// witness slots intact. Encodes to the compiled bytecode.
    /// Consumed by `signcall` — verifier sees `Opaque(bytes)` and parses,
    /// prover keeps witnesses inline. Taproot `open` loads code from Cells.
    Script(Vec<Instruction>),
    /// Prover-side contract with witness-bearing `Commitment::Open`
    /// quantities/flavors on its Token payloads. Encodes to the
    /// 32-byte Contract ID. The body is collected into the transaction Cell hierarchy;
    /// `input` resolves it there before applying this private witness overlay.
    Contract(Contract),
}

impl StringWitness {
    fn to_bytes_vec(&self) -> Vec<u8> {
        match self {
            Self::Point(p) => p.to_bytes().to_vec(),
            Self::Scalar(s) => s.to_bytes().to_vec(),
            Self::Script(instrs) => compile_instructions(instrs),
            Self::Contract(c) => c.id().to_vec(),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Point(_) | Self::Scalar(_) | Self::Contract(_) => 32,
            Self::Script(instrs) => instrs.iter().map(Instruction::encoded_size).sum(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Script(instrs) => instrs.is_empty(),
            // Point, Scalar, and Contract ID are 32 bytes.
            _ => false,
        }
    }
}

impl String {
    /// Temporary VM String limit: one payload-only Cell, with no continuation.
    pub const MAX_LEN: usize = cells::MAX_CELL_PAYLOAD;

    /// Validates host-built literals/witnesses before VM use or byte allocation.
    pub fn check_len(&self) -> Result<(), VMError> {
        Self::check_length(self.len())
    }

    pub(crate) fn check_length(len: usize) -> Result<(), VMError> {
        if len > Self::MAX_LEN {
            return Err(VMError::StringTooLong);
        }
        Ok(())
    }

    /// Checks bounded growth before serializing a witness or allocating bytes.
    pub(crate) fn appended_len(&self, extra: usize) -> Result<usize, VMError> {
        let len = self
            .len()
            .checked_add(extra)
            .ok_or(VMError::StringTooLong)?;
        Self::check_length(len)?;
        Ok(len)
    }

    // ── Construction ────────────────────────────────────────────

    /// Constructs a witness-bearing Point-String.
    pub fn point(p: Point) -> String {
        String::Witness(Box::new(StringWitness::Point(p)))
    }

    /// Convenience: wrap a `Commitment` as a point witness.
    pub fn commitment(c: Commitment) -> String {
        Self::point(Point::commitment(c))
    }

    /// Constructs a witness-bearing Scalar-String.
    pub fn scalar<T: Into<Scalar>>(s: T) -> String {
        String::Witness(Box::new(StringWitness::Scalar(s.into())))
    }

    /// Convenience: wrap a `Predicate` as a point witness.
    pub fn predicate(p: Predicate) -> String {
        Self::point(Point::predicate(p))
    }

    /// Constructs a witness-bearing Script-String. Used by the
    /// prover when pushing a sub-script that contains witnesses
    /// (e.g. inner `alloc(Some(_))` calls) and will later be
    /// consumed by `run` / `switch` / `signcall`.
    pub fn script(instructions: Vec<Instruction>) -> String {
        String::Witness(Box::new(StringWitness::Script(instructions)))
    }

    /// Constructs a witness-bearing Contract-String. Used by the prover
    /// before `op_input` to push a contract whose Token payloads still
    /// carry `Commitment::Open` quantities/flavors. The verifier-side
    /// equivalent is `String::Opaque(contract.id().to_vec())`, with the
    /// output Cell and required children supplied in the same transaction Cell hierarchy.
    pub fn contract(c: Contract) -> String {
        String::Witness(Box::new(StringWitness::Contract(c)))
    }

    // ── Byte views ──────────────────────────────────────────────

    /// Returns a borrow of the inner bytes when this is `Opaque`.
    pub fn as_opaque(&self) -> Option<&[u8]> {
        match self {
            String::Opaque(d) => Some(d),
            _ => None,
        }
    }

    /// Consumes self and returns the inner bytes. For `Opaque`,
    /// returns the inner Vec without allocating. For witness-bearing
    /// variants, encodes to a fresh Vec.
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            String::Opaque(d) => d,
            // Witness variants serialize the same whether owned or borrowed.
            other => other.to_bytes_vec(),
        }
    }

    /// Borrowing variant of [`String::to_bytes`] — returns the
    /// canonical bytes by value (always allocates, even for `Opaque`).
    /// Prefer [`String::to_bytes`] when you own the `String`, or
    /// [`String::as_opaque`] when you only need the `Opaque` case.
    pub fn to_bytes_vec(&self) -> Vec<u8> {
        match self {
            String::Opaque(d) => d.clone(),
            String::Witness(w) => w.to_bytes_vec(),
        }
    }

    /// Length in canonical bytes: 32 for Point/Scalar/Contract ID,
    /// compiled bytecode length for Script.
    pub fn len(&self) -> usize {
        match self {
            String::Opaque(d) => d.len(),
            String::Witness(w) => w.len(),
        }
    }

    /// True iff this String's canonical bytes are empty.
    pub fn is_empty(&self) -> bool {
        match self {
            String::Opaque(d) => d.is_empty(),
            String::Witness(w) => w.is_empty(),
        }
    }

    // ── Downcasts ───────────────────────────────────────────────

    /// Downcasts to a `Commitment`. `Point` routes through
    /// `Point::to_commitment` (preserves witness when present);
    /// `Opaque` parses 32 bytes as `Commitment::Closed`.
    pub fn to_commitment(self) -> Result<Commitment, VMError> {
        match self {
            String::Witness(w) => match *w {
                StringWitness::Point(p) => p.to_commitment(),
                _ => Err(VMError::TypeNotString),
            },
            String::Opaque(data) => {
                let bytes = array32(&data).ok_or(VMError::TypeNotString)?;
                Ok(Commitment::Closed(
                    curve25519_dalek::ristretto::CompressedRistretto(bytes),
                ))
            }
        }
    }

    /// Downcasts to a `Scalar`. For `Opaque`, parses a canonical
    /// 32-byte little-endian residue strictly below the group order. For
    /// `StringWitness::Scalar(i)`, returns the witness directly.
    pub fn to_scalar(self) -> Result<Scalar, VMError> {
        match self {
            String::Witness(w) => match *w {
                StringWitness::Scalar(i) => Ok(i),
                _ => Err(VMError::InvalidScalarEncoding),
            },
            String::Opaque(data) => {
                let bytes = array32(&data).ok_or(VMError::InvalidScalarEncoding)?;
                Scalar::from_bytes(bytes).ok_or(VMError::InvalidScalarEncoding)
            }
        }
    }

    /// Downcasts to a `Vec<Instruction>` — the runtime form the VM
    /// walks. `Script` returns witnesses inline (prover side);
    /// `Opaque` parses via `ScriptBuilder::parse` (verifier side); 32-byte
    /// point/scalar variants are not executable bytecode and error.
    ///
    /// Preserves prover witnesses when converting a short embedded script.
    /// Taproot branches are loaded separately through the Cell resolver.
    pub fn to_instructions(self) -> Result<Vec<Instruction>, VMError> {
        match self {
            String::Witness(w) => match *w {
                StringWitness::Script(instrs) => Ok(instrs),
                _ => Err(VMError::TypeNotString),
            },
            String::Opaque(data) => Ok(ScriptBuilder::parse(&data)?.into_instructions()),
        }
    }

    /// Like [`to_instructions`], but returns the executable [`Script`]:
    /// `Transparent` keeps prover witnesses inline; `Opaque` becomes raw
    /// bytecode the verifier decodes on demand (no parse).
    pub(crate) fn into_script(self) -> Result<Script, VMError> {
        match self {
            String::Witness(w) => match *w {
                StringWitness::Script(instrs) => Ok(Script::Transparent(instrs)),
                _ => Err(VMError::TypeNotString),
            },
            String::Opaque(data) => Ok(Script::Opaque(data)),
        }
    }

    /// Downcasts to a `Predicate`. `Point` routes through
    /// `Point::to_predicate` (preserves witness when present);
    /// `Opaque` parses 32 bytes as `Predicate::Opaque`.
    pub fn to_predicate(self) -> Result<Predicate, VMError> {
        match self {
            String::Witness(w) => match *w {
                StringWitness::Point(p) => p.to_predicate(),
                _ => Err(VMError::InvalidPoint),
            },
            String::Opaque(data) => {
                let bytes = array32(&data).ok_or(VMError::InvalidPoint)?;
                Ok(Predicate::opaque(
                    curve25519_dalek::ristretto::CompressedRistretto(bytes),
                ))
            }
        }
    }

    /// Downcasts to a `Contract`. For `StringWitness::Contract(c)`, returns the
    /// witness-bearing contract directly. An opaque ID cannot be decoded without
    /// the transaction's resolver; the VM owns that authenticated lookup.
    pub fn to_contract(self) -> Result<Contract, VMError> {
        match self {
            String::Witness(w) => match *w {
                StringWitness::Contract(contract) => Ok(contract),
                _ => Err(VMError::MalformedContractEncoding),
            },
            String::Opaque(_) => Err(VMError::MalformedContractEncoding),
        }
    }

    // ── Byte-level operations ───────────────────────────────────
    //
    // These always operate on canonical bytes; for witness-bearing
    // variants the bytes are serialized first. The result is always
    // a new `Opaque(Vec<u8>)`.

    /// Returns `self || bytes`. Used by `0x46 append`, `0x44 writebits`,
    /// `0x45 writeint`, `0x47 writezeros`.
    pub fn append_bytes(self, bytes: &[u8]) -> Result<String, VMError> {
        self.appended_len(bytes.len())?;
        let mut out = self.to_bytes();
        out.extend_from_slice(bytes);
        Ok(String::Opaque(out))
    }

    /// Returns `self` followed by `n` zero bytes without allocating a
    /// temporary zero buffer.
    pub fn append_zeros(self, n: usize) -> Result<String, VMError> {
        let len = self.appended_len(n)?;
        let mut out = self.to_bytes();
        out.resize(len, 0);
        Ok(String::Opaque(out))
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

    fn zip_bytes(self, other: &String, op: impl Fn(u8, u8) -> u8) -> Option<String> {
        let a = self.to_bytes();
        let b = other.to_bytes_vec();
        if a.len() != b.len() {
            return None;
        }
        Some(String::Opaque(
            a.iter().zip(b.iter()).map(|(x, y)| op(*x, *y)).collect(),
        ))
    }

    /// Bitwise OR. `None` if operand lengths differ.
    pub fn bit_or(self, other: &String) -> Option<String> {
        self.zip_bytes(other, |x, y| x | y)
    }

    /// Bitwise AND. `None` if operand lengths differ.
    pub fn bit_and(self, other: &String) -> Option<String> {
        self.zip_bytes(other, |x, y| x & y)
    }

    /// Bitwise XOR. `None` if operand lengths differ.
    pub fn bit_xor(self, other: &String) -> Option<String> {
        self.zip_bytes(other, |x, y| x ^ y)
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
        let removed_bytes = n.div_ceil(8);
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
        let removed_bytes = n.div_ceil(8);
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
// by `to_bytes_vec` and `len` for `StringWitness::Script`. ──────

pub(crate) fn compile_instructions(instrs: &[Instruction]) -> Vec<u8> {
    let mut out = Vec::new();
    for instr in instrs {
        instr.encode(&mut out);
    }
    out
}

pub(crate) fn array32(data: &[u8]) -> Option<[u8; 32]> {
    if data.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(data);
    Some(bytes)
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
