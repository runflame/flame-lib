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
#[derive(Clone, Debug)]
pub struct PredicateTree {
    /// Internal key `X`. The full predicate point is `P = X + H(X, M)·B`.
    pub internal_key: CompressedRistretto,
    /// Programs at the leaves of the merkle tree. The tree is balanced
    /// in canonical order. For Phase 9 we ship a single-leaf flavor
    /// (`programs.len() == 1`) and extend to general trees later.
    pub programs: Vec<Vec<u8>>,
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
    /// Computes the predicate's opaque point `P = X + H(X, M)·B`.
    pub fn compute_point(&self) -> CompressedRistretto {
        let root = self.merkle_root();
        let h = taproot_tweak(&self.internal_key, &root);
        let x_point = self
            .internal_key
            .decompress()
            .expect("PredicateTree built from a valid Ristretto point");
        (x_point + &h * &RISTRETTO_BASEPOINT_TABLE).compress()
    }

    /// Computes the merkle root over the leaf programs. For a single-leaf
    /// tree, the root is the leaf hash itself.
    pub fn merkle_root(&self) -> [u8; 32] {
        assert!(!self.programs.is_empty(), "PredicateTree must have ≥1 leaf");
        // Phase 9: single-leaf tree only. General balanced-tree merklization
        // will land alongside multi-program predicates in a later pass.
        assert_eq!(self.programs.len(), 1, "Phase 9: single-leaf trees only");
        merkle_leaf_hash(&self.programs[0])
    }

    /// Builds a `CallProof` that opens the `program_index`-th leaf.
    /// Phase 9: only `program_index == 0` is supported.
    pub fn callproof_for(&self, program_index: usize) -> CallProof {
        assert_eq!(program_index, 0, "Phase 9: single-leaf trees only");
        CallProof {
            internal_key: self.internal_key,
            neighbors: Vec::new(),
            position: Vec::new(),
            program: self.programs[0].clone(),
        }
    }
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

    /// Computes the canonical identity hash of this cell. Used as the
    /// source for the next anchor (via `to_anchor`) and for txlog
    /// commitments.
    ///
    /// The hash is bound to: opaque predicate point, anchor, and an
    /// ordered hash chain over payload type-codes + bytes. For Phase 9
    /// we use a Merlin transcript over the cell fields — payload
    /// values' canonical encodings will be added when full encoding
    /// support lands.
    pub fn id(&self) -> [u8; 32] {
        let mut t = Transcript::new(b"flamevm.cell.id.v1");
        t.append_message(b"predicate", self.predicate.to_point().as_bytes());
        t.append_message(b"anchor", &self.anchor.0);
        // Payload-bind via length prefix + per-item commit. Per-item
        // commitment uses `Value::type_code` + a placeholder for the
        // value's canonical bytes. Full canonical encoding is wired in
        // alongside the cell wire-format work in a follow-up phase.
        let len = self.payload.len() as u64;
        t.append_message(b"payload.len", &len.to_le_bytes());
        for v in &self.payload {
            t.append_message(b"payload.tag", &[v.type_code()]);
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
/// position bitstring (bit i = 0 → neighbor on right, 1 → left).
fn merkle_walk_up(
    mut hash: [u8; 32],
    neighbors: &[[u8; 32]],
    position: &[u8],
) -> Result<[u8; 32], VMError> {
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
