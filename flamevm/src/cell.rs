//! Cells — the unit of locked, portable value in FlameVM.
//!
//! A `Cell` is a linear value that wraps a portable payload under a
//! Taproot-compressed `Predicate`. Cells exist in three places:
//!
//! - **On the stack** during VM execution, as a `Value::Cell` handle.
//! - **In the UTXO accumulator** (Utreexo), as a wire-encoded blob.
//! - **In the txlog** as the result of an `output` effect.
//!
//! ## Linear semantics
//!
//! Cells are non-copyable and non-droppable: they can be created (`cell`,
//! `output`), opened (`open`, `signrun`, `signtx`), or sealed into the
//! Utreexo (`output`). Holding a cell on the stack outside one of these
//! flows is an error at frame exit (StackNotClean).
//!
//! ## Wire encoding
//!
//! On the wire, a cell is a list-style `Dict` with three entries:
//!
//! ```text
//! Cell = list-Dict {
//!   0: Point   (opaque predicate point P)
//!   1: String  (32-byte anchor)
//!   2: Dict    (list-style; payload items in order)
//! }
//! ```
//!
//! The opaque predicate is what consensus sees — the Taproot-compressed
//! point P. Prover-side witnesses (the internal key, the merkle tree) are
//! stripped before encoding.

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;

use crate::errors::VMError;
use crate::vm::Anchor;
use crate::Value;

// ── Predicate ────────────────────────────────────────────────────

/// Unlock condition for a cell. All FlameVM predicates are
/// Taproot-compressed: `P = X + H(X, M) · B` where `X` is the internal
/// key and `M` is the merkle root over a tree of programs.
///
/// The `Opaque` variant is what verifiers see — just the 32-byte
/// compressed point `P`. Prover-side variants (initially just `Tree`)
/// carry the witness data needed to construct `CallProof`s or sign for
/// the predicate. Additional witness variants may be added later as the
/// prover API matures.
#[derive(Clone, Debug)]
pub enum Predicate {
    /// Verifier-visible compressed point. The only variant that crosses
    /// the wire.
    Opaque(CompressedRistretto),

    /// Prover-witness: the internal key plus the merkle tree of unlock
    /// programs. Carries enough information to construct `CallProof`s
    /// and to sign for the predicate.
    Tree(PredicateTree),
}

/// Prover-side witness for a Taproot predicate.
///
/// The tree commits to one or more **programs** via a balanced merkle
/// root, then Taproot-tweaks the internal key by `H(X, M)` to produce the
/// opaque predicate point. Construction validates both `internal_key`
/// (must decompress) and `programs` (must be non-empty); fields are
/// `pub(crate)` to enforce that invariant.
#[derive(Clone, Debug)]
pub struct PredicateTree {
    pub(crate) internal_key: CompressedRistretto,
    pub(crate) programs: Vec<Vec<u8>>,
}

impl Predicate {
    /// Returns the verifier-visible compressed point. For `Tree`, this
    /// computes `P = X + H(X, M) · B`.
    pub fn to_point(&self) -> CompressedRistretto {
        match self {
            Predicate::Opaque(p) => *p,
            Predicate::Tree(t) => t.compute_point(),
        }
    }

    /// Strips any prover-side witness data, leaving only the opaque
    /// point. Used when sealing a cell into wire encoding.
    pub fn to_opaque(&self) -> Predicate {
        Predicate::Opaque(self.to_point())
    }

    /// The 32-byte verification key for `signtx` / `signrun`.
    /// Equal to the predicate's opaque point.
    pub fn verification_key(&self) -> CompressedRistretto {
        self.to_point()
    }

    /// Verifies a `CallProof` against this predicate. On success returns
    /// the unlocked program bytes (the leaf the proof opens). On failure
    /// (path mismatch, decompression failure, etc.) returns
    /// `VMError::CallProofMismatch` — hard error per Phase 9 model.
    pub fn verify_callproof<'a>(
        &self,
        cp: &'a CallProof,
    ) -> Result<&'a [u8], VMError> {
        // 1. Compute the merkle root from program + neighbors + position.
        let leaf = merkle_leaf_hash(&cp.program);
        let root = merkle_walk_up(leaf, &cp.neighbors, &cp.position)?;
        // 2. Compute h = H(X, M) and the expected point P' = X + h·B.
        let h = taproot_tweak(&cp.internal_key, &root);
        let x_point = cp
            .internal_key
            .decompress()
            .ok_or(VMError::CallProofMismatch)?;
        let p_prime = x_point + &h * &RISTRETTO_BASEPOINT_TABLE;
        // 3. Compare to the predicate's opaque point.
        if p_prime.compress() != self.to_point() {
            return Err(VMError::CallProofMismatch);
        }
        Ok(&cp.program)
    }
}

impl PredicateTree {
    /// Builds a validated tree. Errors if `programs` is empty or if
    /// `internal_key` does not decompress to a valid Ristretto point.
    pub fn new(
        internal_key: CompressedRistretto,
        programs: Vec<Vec<u8>>,
    ) -> Result<PredicateTree, VMError> {
        if programs.is_empty() {
            return Err(VMError::EmptyPredicateTree);
        }
        if internal_key.decompress().is_none() {
            return Err(VMError::InvalidPoint);
        }
        Ok(PredicateTree { internal_key, programs })
    }

    /// Read-only accessor for the internal key.
    pub fn internal_key(&self) -> &CompressedRistretto {
        &self.internal_key
    }

    /// Read-only accessor for the leaf programs.
    pub fn programs(&self) -> &[Vec<u8>] {
        &self.programs
    }

    /// Computes the predicate's opaque point `P = X + H(X, M)·B`.
    pub fn compute_point(&self) -> CompressedRistretto {
        let root = self.merkle_root();
        let h = taproot_tweak(&self.internal_key, &root);
        let x_point = self
            .internal_key
            .decompress()
            .expect("PredicateTree::new validated the internal key");
        (x_point + &h * &RISTRETTO_BASEPOINT_TABLE).compress()
    }

    /// Computes the merkle root over the leaf programs. For a single
    /// program, the root is the leaf hash itself. For more, the tree
    /// is balanced by repeatedly splitting at
    /// `next_power_of_two(n) / 2` — the same convention as zkvm's
    /// `merkle::MerkleTree`.
    pub fn merkle_root(&self) -> [u8; 32] {
        merkle_root_of_programs(&self.programs)
    }

    /// Builds a `CallProof` that opens the `program_index`-th leaf.
    /// Errors if `program_index >= programs.len()`.
    ///
    /// The returned proof's `neighbors` are leaf-to-root; `position`
    /// is a bit-packed string where bit `i` (LSB-first within byte)
    /// describes step `i` of the walk-up: `0` means "current hash on
    /// left / neighbor on right", `1` means "swap".
    pub fn callproof_for(&self, program_index: usize) -> Result<CallProof, VMError> {
        if program_index >= self.programs.len() {
            return Err(VMError::ProgramIndexOutOfRange);
        }
        // We walk root-to-leaf during construction (descending into halves)
        // but `merkle_walk_up` consumes neighbors leaf-to-root. Push in
        // descent order, then reverse — O(n) once vs. O(n) per insert(0).
        let mut neighbors = Vec::new();
        let mut bits = Vec::new();
        let mut sublist: &[Vec<u8>] = &self.programs;
        let mut subindex = program_index;
        while sublist.len() >= 2 {
            let k = sublist.len().next_power_of_two() / 2;
            if subindex >= k {
                // Current is in the right half; sibling is left subtree.
                neighbors.push(merkle_root_of_programs(&sublist[..k]));
                bits.push(1);
                sublist = &sublist[k..];
                subindex -= k;
            } else {
                neighbors.push(merkle_root_of_programs(&sublist[k..]));
                bits.push(0);
                sublist = &sublist[..k];
            }
        }
        neighbors.reverse();
        bits.reverse();
        Ok(CallProof {
            internal_key: self.internal_key,
            neighbors,
            position: pack_position_bits(&bits),
            program: self.programs[program_index].clone(),
        })
    }
}

/// Balanced merkle root over an ordered list of programs. Splits at
/// `next_power_of_two(n) / 2`. For `n = 1` the leaf hash itself is the
/// root.
fn merkle_root_of_programs(programs: &[Vec<u8>]) -> [u8; 32] {
    debug_assert!(!programs.is_empty(), "merkle_root_of_programs: empty list");
    if programs.len() == 1 {
        merkle_leaf_hash(&programs[0])
    } else {
        let k = programs.len().next_power_of_two() / 2;
        let left = merkle_root_of_programs(&programs[..k]);
        let right = merkle_root_of_programs(&programs[k..]);
        merkle_node_hash(&left, &right)
    }
}

/// Packs a slice of `0`/`1` bit values into bytes, LSB-first within
/// each byte. Trailing high bits in the last byte are zero-padded.
fn pack_position_bits(bits: &[u8]) -> Vec<u8> {
    let byte_count = (bits.len() + 7) / 8;
    let mut out = vec![0u8; byte_count];
    for (i, &b) in bits.iter().enumerate() {
        if b & 1 != 0 {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

// ── CallProof ────────────────────────────────────────────────────

/// Taproot path proof + the leaf program being unlocked.
///
/// Stack-encoded as four separate strings that the `open` opcode pops
/// (top to bottom: program, position, neighbors-list, internal_key) and
/// hands here as a struct. The `neighbors` list is a list-style Dict of
/// 32-byte strings; the `position` is a bit-packed string where bit `i`
/// indicates the side (0 = left, 1 = right) of the i-th neighbor.
#[derive(Clone, Debug)]
pub struct CallProof {
    /// Internal key `X` of the Taproot construction.
    pub internal_key: CompressedRistretto,
    /// Sibling hashes along the merkle path, leaf-to-root.
    pub neighbors: Vec<[u8; 32]>,
    /// Position bits: bit `i` is `0` if the i-th neighbor is on the
    /// right of the running hash, `1` if on the left.
    pub position: Vec<u8>,
    /// The leaf program being unlocked.
    pub program: Vec<u8>,
}

// ── Cell ─────────────────────────────────────────────────────────

/// A linear-typed cell carrying a payload under an unlock predicate.
///
/// `Cell` is intentionally not `Clone` or `Debug`: `Value` is non-clonable
/// (linear types in its variants), so a Cell containing Values can't be
/// derived for these. Cells move; they don't copy.
pub struct Cell {
    /// Unlock predicate. Always opaque when the cell crosses the wire;
    /// may carry prover-witness when the cell is freshly built in-VM.
    pub predicate: Predicate,
    /// 32-byte anchor — derived by ratcheting from the prior anchor.
    pub anchor: Anchor,
    /// Payload values; each must be portable (enforced at construction).
    pub payload: Vec<Value>,
}

impl Cell {
    /// Constructs a cell. Caller is responsible for portability and
    /// anchor uniqueness; this constructor doesn't re-check.
    pub fn new(predicate: Predicate, anchor: Anchor, payload: Vec<Value>) -> Self {
        Cell { predicate, anchor, payload }
    }

    /// Computes the canonical identity hash of this cell, via a Merlin
    /// transcript that absorbs the opaque predicate point, the anchor,
    /// and each payload value's **canonical wire encoding**.
    ///
    /// Anchor uniqueness across a tx (guaranteed by the ratchet chain
    /// from inputs forward) makes cell-ids unique without needing the
    /// payload to disambiguate; binding the payload bytes here is
    /// belt-and-suspenders to make `id()` a true commitment to the
    /// cell's contents — needed for protocol messages (signtx/signrun)
    /// and for the txlog Output entry.
    ///
    /// Panics if a payload value's type has no canonical encoder.
    /// All portable types (Int253, String, Dict, Point) encode today;
    /// Token / ClearToken / WideToken encoders land in Phase 13.
    pub fn id(&self) -> [u8; 32] {
        let mut t = Transcript::new(b"flamevm.cell.id.v1");
        t.append_message(b"predicate", self.predicate.to_point().as_bytes());
        t.append_message(b"anchor", &self.anchor.0);
        let len = self.payload.len() as u64;
        t.append_message(b"payload.len", &len.to_le_bytes());
        // Bind each payload item's canonical wire bytes. Re-use a single
        // buffer across items; clear between writes.
        let mut buf = Vec::new();
        for v in &self.payload {
            buf.clear();
            crate::encoding::write_value(&mut buf, v)
                .expect("portable payload value must have a canonical encoder");
            t.append_message(b"payload.item", &buf);
        }
        let mut h = [0u8; 32];
        t.challenge_bytes(b"id", &mut h);
        h
    }

    /// Derives the cell's contribution to the anchor chain. Equivalent
    /// to `Anchor(self.id()).ratchet()`.
    pub fn to_anchor(&self) -> Anchor {
        Anchor(self.id()).ratchet()
    }
}

// ── Internal hashing helpers (all via Merlin) ────────────────────

/// `H(X, M)` — the Taproot tweak scalar.
fn taproot_tweak(internal_key: &CompressedRistretto, merkle_root: &[u8; 32]) -> Scalar {
    let mut t = Transcript::new(b"flamevm.taproot.v1");
    t.append_message(b"key", internal_key.as_bytes());
    t.append_message(b"root", merkle_root);
    let mut buf = [0u8; 64];
    t.challenge_bytes(b"h", &mut buf);
    Scalar::from_bytes_mod_order_wide(&buf)
}

/// Merkle leaf hash for a program.
fn merkle_leaf_hash(program: &[u8]) -> [u8; 32] {
    let mut t = Transcript::new(b"flamevm.merkle.leaf.v1");
    t.append_message(b"program", program);
    let mut h = [0u8; 32];
    t.challenge_bytes(b"hash", &mut h);
    h
}

/// Merkle node hash combining two child hashes.
fn merkle_node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut t = Transcript::new(b"flamevm.merkle.node.v1");
    t.append_message(b"left", left);
    t.append_message(b"right", right);
    let mut h = [0u8; 32];
    t.challenge_bytes(b"hash", &mut h);
    h
}

/// Walks up the merkle path from a leaf hash using neighbors and a
/// position bitstring (bit i: `0` → current on left / neighbor on right,
/// `1` → swap; LSB-first within byte). The position bitstring must
/// cover all neighbors — more position bytes than required (up to byte
/// alignment) is fine; fewer is a `MalformedCallProof`.
fn merkle_walk_up(
    mut hash: [u8; 32],
    neighbors: &[[u8; 32]],
    position: &[u8],
) -> Result<[u8; 32], VMError> {
    if neighbors.len() > position.len().saturating_mul(8) {
        return Err(VMError::MalformedCallProof);
    }
    for (i, neighbor) in neighbors.iter().enumerate() {
        let bit = get_bit(position, i);
        hash = if bit == 0 {
            merkle_node_hash(&hash, neighbor)
        } else {
            merkle_node_hash(neighbor, &hash)
        };
    }
    Ok(hash)
}

fn get_bit(bits: &[u8], i: usize) -> u8 {
    let byte = i / 8;
    let off = i % 8;
    if byte >= bits.len() {
        0
    } else {
        (bits[byte] >> off) & 1
    }
}
