//! Cells, predicates, and call-proofs.

use bulletproofs::PedersenGens;
use core::any::Any;
use core::fmt;
use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;
use readerwriter::{Decodable, Encodable, ExactSizeEncodable, ReadError, Reader, WriteError, Writer};

use crate::encoding::{read_list_prefix, read_value, write_list_prefix, write_value};
use crate::errors::VMError;
use crate::vm::Anchor;
use crate::{Point, String, Value};

/// 32-byte canonical identifier of a `Cell`. Computed via Merlin
/// transcript over the cell's canonical wire encoding (see `Cell::id`).
/// Stored in `TxEntry::Input` to commit a consumed cell's identity
/// without re-storing its payload.
pub type CellID = [u8; 32];

// ── Predicate ────────────────────────────────────────────────────

/// Prover-side metadata attached to a [`Predicate`]. The witness
/// helps construct call-proofs, signatures, and re-derive the
/// predicate's key on the prover side; it never crosses the wire.
///
/// Today the only impl is [`PredicateTree`] — the Taproot merkle
/// witness with internal key + program leaves. Other anticipated
/// witnesses (per zkvm's lead, commit 4a9ec80 in the slingshot
/// tree) include:
///
/// - **Raw private keys** for tests / build-and-sign flows.
/// - **Keytree derivation indices** so wallets can re-derive a
///   predicate's key from a seed + path.
/// - **Multikey / MuSig layouts** for 2-of-2 payment channels and
///   other multi-party signing protocols.
///
/// Each is added by `impl PredicateWitness for MyType` — no touch
/// to `Predicate` itself or its verifier-side call sites.
///
/// `Any` lets prover-side code downcast via
/// [`Predicate::witness_as`]. `Send + Sync` keeps `Predicate`
/// usable across threads (the txlog's `Output(Cell)` carries it).
/// `Debug` supports the manual `Debug` impl on `Predicate`.
pub trait PredicateWitness: Any + Send + Sync + fmt::Debug {
    /// Canonical 32-byte compressed Ristretto point this witness
    /// resolves to. Must equal the `point` field of the
    /// `Predicate` that holds this witness — checked at
    /// construction time.
    fn to_point(&self) -> CompressedRistretto;

    /// Clones the witness behind a fresh boxed trait object. Used
    /// by `<Predicate as Clone>::clone`.
    fn clone_witness(&self) -> Box<dyn PredicateWitness>;

    /// Bridge to `Any` so callers can downcast.
    fn as_any(&self) -> &dyn Any;
}

/// Unlock condition for a cell — a Taproot-compressed point
/// `P = X + H(X, M) · B` where `X` is the internal key and `M` is
/// the merkle root over the program tree.
///
/// The on-wire form is just `point` (32 bytes). `witness` is
/// optional prover-side metadata; it never serializes.
///
/// **Construction:**
///
/// - `Predicate::opaque(point)` — verifier-side; the wire-decoded
///   form, no witness attached.
/// - `Predicate::tree(tree)` — prover-side; wraps a
///   [`PredicateTree`] witness.
/// - `Predicate::with_witness(w)` — prover-side; attaches any
///   custom [`PredicateWitness`] (Multikey, keytree-derived key,
///   raw test scalar, …).
///
/// **Wire form is invariant across construction styles.** Calling
/// `.to_point()` on any of the above returns the same 32 bytes —
/// the constructors enforce this by deriving `point` from the
/// witness when one is provided.
pub struct Predicate {
    /// Canonical wire form. The only thing observable to
    /// verifiers; equal to `witness.to_point()` when `witness` is
    /// `Some` (enforced by the constructors).
    pub(crate) point: CompressedRistretto,

    /// Optional prover metadata. `None` is the verifier's view.
    /// Skipped by any serialization that targets the wire format.
    pub(crate) witness: Option<Box<dyn PredicateWitness>>,
}

impl Clone for Predicate {
    /// Clones the point; delegates witness cloning to [`PredicateWitness::clone_witness`].
    fn clone(&self) -> Self {
        Predicate {
            point: self.point,
            witness: self.witness.as_ref().map(|w| w.clone_witness()),
        }
    }
}

impl fmt::Debug for Predicate {
    /// Prints only the canonical point. The witness type is opaque
    /// to the formatter (could be anything implementing
    /// `PredicateWitness`); we don't try to render it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Predicate")
            .field(&self.point)
            .finish()
    }
}

/// One leaf in a `PredicateTree`'s merkle commitment. Every program leaf
/// is paired with a `Blinding` sibling so the position of any given
/// program inside its pair is uniformly random — observers walking a
/// merkle proof cannot tell program leaves apart from blinding leaves.
#[derive(Clone, Debug)]
pub enum PredicateLeaf {
    /// A script program that, if matched by a `CallProof`, unlocks the cell.
    Program(Vec<u8>),
    /// A 32-byte random sibling that hides its program partner's position.
    Blinding([u8; 32]),
}

/// Prover-side witness for a Taproot predicate.
///
/// The tree commits to one or more **programs** via a balanced merkle
/// root over `leaves`, then Taproot-tweaks the internal key by
/// `H(X, M)` to produce the opaque predicate point. Each program leaf
/// is paired with a `Blinding` sibling derived deterministically from
/// the `blinding_key` seed passed to `new`, so the on-tree position of
/// a program within its pair is uniformly random. The seed itself is
/// not retained — once the leaves are built, the seed is no longer
/// needed for [`point`](Self::point) or [`callproof_for`](Self::callproof_for).
///
/// `point` caches `X + H(X, M) · B` so subsequent reads are O(1).
/// `flamevm` reads it via `Predicate::to_point()` from the per-cell
/// txid hash, the txlog's `Send.refund_predicate` encoding, and the
/// `signtx`/`signcall` verification-key lookup — a hot path that
/// previously re-walked the merkle root and re-multiplied the
/// basepoint table on every call.
///
/// Fields are `pub(crate)` to enforce the construction invariants:
/// non-empty `leaves` exactly `2 × programs.len()` in length, an
/// `internal_key` that decompresses to a valid Ristretto point,
/// and `point` populated by `new` from the other two fields.
#[derive(Clone, Debug)]
pub struct PredicateTree {
    pub(crate) internal_key: CompressedRistretto,
    pub(crate) leaves: Vec<PredicateLeaf>,
    /// Cached Taproot-tweaked point `P = X + H(X, M) · B`.
    pub(crate) point: CompressedRistretto,
}

impl PredicateWitness for PredicateTree {
    fn to_point(&self) -> CompressedRistretto {
        self.point
    }
    fn clone_witness(&self) -> Box<dyn PredicateWitness> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Predicate {
    /// Verifier-style construction: wraps a wire-decoded point. No
    /// witness attached. The on-wire `point` is the only thing the
    /// verifier ever sees; constructing via `opaque` is what
    /// `op_input` and predicate decoding do.
    pub fn opaque(point: CompressedRistretto) -> Self {
        Predicate { point, witness: None }
    }

    /// Prover-style construction: attaches a typed witness. The
    /// predicate's `point` is derived from the witness so the two
    /// stay in lockstep.
    pub fn with_witness<W: PredicateWitness>(witness: W) -> Self {
        let point = witness.to_point();
        Predicate { point, witness: Some(Box::new(witness)) }
    }

    /// Convenience: attach a [`PredicateTree`] witness. Equivalent
    /// to `Predicate::with_witness(tree)`.
    pub fn tree(tree: PredicateTree) -> Self {
        Self::with_witness(tree)
    }

    /// Returns the verifier-visible compressed point. O(1): the
    /// constructors cache it.
    pub fn to_point(&self) -> CompressedRistretto {
        self.point
    }

    /// Strips any prover-side witness data, leaving only the opaque
    /// point. Used when sealing a cell into wire encoding.
    pub fn to_opaque(&self) -> Predicate {
        Predicate::opaque(self.point)
    }

    /// Borrows the witness as `&W` if one is attached and downcasts
    /// to the requested type. Returns `None` if no witness is
    /// attached or the witness is of a different type.
    ///
    /// Used by prover-side code that needs the concrete witness:
    /// e.g. `predicate.witness_as::<PredicateTree>()` to construct
    /// a `CallProof`.
    pub fn witness_as<W: PredicateWitness>(&self) -> Option<&W> {
        self.witness.as_ref()?.as_any().downcast_ref::<W>()
    }

    /// True iff a witness is attached. Cheap probe before a
    /// downcast when the caller doesn't know which witness type to
    /// expect.
    pub fn has_witness(&self) -> bool {
        self.witness.is_some()
    }

    /// The 32-byte verification key for `signtx` / `signcall`.
    /// Equal to the predicate's opaque point.
    pub fn verification_key(&self) -> CompressedRistretto {
        self.point
    }
}

/// 32-byte compressed Ristretto — the verifier's view. Any
/// prover-side witness is dropped on the wire.
impl Encodable for Predicate {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        w.write(b"predicate", self.point.as_bytes())
    }
}

impl ExactSizeEncodable for Predicate {
    fn encoded_size(&self) -> usize { 32 }
}

impl Decodable for Predicate {
    fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        let pt = r.read_u8x32()?;
        Ok(Predicate::opaque(CompressedRistretto(pt)))
    }
}

impl Predicate {

    /// The secondary Pedersen generator `B_blinding`, compressed.
    /// Suitable as an internal key when no key-path spend is desired:
    /// the discrete log of `B_blinding` w.r.t. the primary basepoint
    /// `B` is unknown by construction, so signing for the resulting
    /// tweaked predicate point is computationally infeasible.
    /// `PredicateTree::new(None, …)` substitutes this point.
    pub fn unspendable_key() -> CompressedRistretto {
        PedersenGens::default().B_blinding.compress()
    }

    /// Verifies a `CallProof` against this predicate. On success returns
    /// the unlocked program bytes (the leaf the proof opens). On failure
    /// (path mismatch, decompression failure, etc.) returns
    /// `VMError::CallProofMismatch` — a hard error: cell-open
    /// failures must not be recoverable.
    pub fn verify_callproof<'a>(
        &self,
        cp: &'a CallProof,
    ) -> Result<&'a [u8], VMError> {
        // Reconstruct the tweaked point P' = X + H(X, M)·B from the proof
        // and require it to equal the predicate's opaque point.
        let leaf = program_leaf_hash(&cp.program);
        let root = merkle_walk_up(leaf, &cp.neighbors, &cp.position)?;
        let h = taproot_tweak(&cp.internal_key, &root);
        let x_point = cp
            .internal_key
            .decompress()
            .ok_or(VMError::CallProofMismatch)?;
        let p_prime = x_point + RISTRETTO_BASEPOINT_TABLE * &h;
        if p_prime.compress() != self.to_point() {
            return Err(VMError::CallProofMismatch);
        }
        Ok(&cp.program)
    }
}

impl PredicateTree {
    /// Builds a validated tree.
    ///
    /// `internal_key = None` substitutes `Predicate::unspendable_key()`
    /// (the secondary Pedersen generator `B_blinding`), producing a
    /// program-only predicate that nobody can sign for — the only way to
    /// satisfy it is via a `CallProof` against one of the embedded leaves.
    ///
    /// `blinding_key` seeds a deterministic per-program blinding factor
    /// so the same `(internal_key, programs, blinding_key)` triple always
    /// produces the same opaque predicate point.
    ///
    /// Errors if `programs` is empty, or if a caller-supplied
    /// `internal_key` does not decompress to a valid Ristretto point.
    pub fn new(
        internal_key: Option<CompressedRistretto>,
        programs: Vec<Vec<u8>>,
        blinding_key: [u8; 32],
    ) -> Result<PredicateTree, VMError> {
        if programs.is_empty() {
            return Err(VMError::EmptyPredicateTree);
        }
        let internal_key = internal_key.unwrap_or_else(Predicate::unspendable_key);
        let x_point = internal_key
            .decompress()
            .ok_or(VMError::InvalidPoint)?;
        let leaves = create_merkle_leaves(&programs, &blinding_key);
        // Precompute the Taproot-tweaked point once at construction.
        // `Predicate::to_point()` returns this cached value in O(1);
        // hot paths (per-cell `Cell::id`, txid hashing, sig vk lookup)
        // would otherwise re-walk the merkle tree and re-multiply the
        // basepoint table on every call.
        let root = merkle_root_of_leaves(&leaves);
        let h = taproot_tweak(&internal_key, &root);
        let point = (x_point + RISTRETTO_BASEPOINT_TABLE * &h).compress();
        Ok(PredicateTree { internal_key, leaves, point })
    }

    /// Convenience: builds a tree with the unspendable internal key
    /// (`Predicate::unspendable_key`), so the predicate can only be
    /// satisfied via a `CallProof` against one of the embedded programs.
    /// Equivalent to `PredicateTree::new(None, programs, blinding_key)`.
    pub fn scripts_only(
        programs: Vec<Vec<u8>>,
        blinding_key: [u8; 32],
    ) -> Result<PredicateTree, VMError> {
        PredicateTree::new(None, programs, blinding_key)
    }

    /// Read-only accessor for the internal key.
    pub fn internal_key(&self) -> &CompressedRistretto {
        &self.internal_key
    }

    /// Read-only accessor for the leaves (both program and blinding).
    pub fn leaves(&self) -> &[PredicateLeaf] {
        &self.leaves
    }

    /// Iterator over the program leaves in original input order.
    pub fn programs(&self) -> impl Iterator<Item = &[u8]> {
        self.leaves.iter().filter_map(|l| match l {
            PredicateLeaf::Program(p) => Some(p.as_slice()),
            PredicateLeaf::Blinding(_) => None,
        })
    }

    /// Returns the cached Taproot-tweaked point
    /// `P = X + H(X, M) · B`. O(1) — `PredicateTree::new` computes
    /// it once at construction and stores it in `self.point`.
    ///
    /// The historic name `compute_point` is kept for backwards
    /// compatibility but no longer recomputes; new code should
    /// prefer `tree.point` or `Predicate::to_point()`.
    pub fn compute_point(&self) -> CompressedRistretto {
        self.point
    }

    /// Computes the merkle root over the leaves. For a single leaf the
    /// root is its leaf hash; otherwise the tree is balanced by repeatedly
    /// splitting at `next_power_of_two(n) / 2`.
    pub fn merkle_root(&self) -> [u8; 32] {
        merkle_root_of_leaves(&self.leaves)
    }

    /// Builds a `CallProof` that opens the `program_index`-th program leaf.
    /// Errors if `program_index` is beyond the number of programs.
    ///
    /// The returned proof's `neighbors` are leaf-to-root; `position` is a
    /// bit-packed string where bit `i` (LSB-first within byte) describes
    /// step `i` of the walk-up: `0` means "current hash on left / neighbor
    /// on right", `1` means "swap".
    pub fn callproof_for(&self, program_index: usize) -> Result<CallProof, VMError> {
        let leaf_index = self.program_leaf_index(program_index)?;
        let program = match &self.leaves[leaf_index] {
            PredicateLeaf::Program(p) => p.clone(),
            PredicateLeaf::Blinding(_) => unreachable!("program_leaf_index points at a Program"),
        };
        // Descend root-to-leaf, collecting siblings, then reverse so the
        // resulting list is leaf-to-root (the order `merkle_walk_up` wants).
        let mut neighbors = Vec::new();
        let mut bits = Vec::new();
        let mut sublist: &[PredicateLeaf] = &self.leaves;
        let mut subindex = leaf_index;
        while sublist.len() >= 2 {
            let k = sublist.len().next_power_of_two() / 2;
            if subindex >= k {
                neighbors.push(merkle_root_of_leaves(&sublist[..k]));
                bits.push(1);
                sublist = &sublist[k..];
                subindex -= k;
            } else {
                neighbors.push(merkle_root_of_leaves(&sublist[k..]));
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
            program,
        })
    }

    /// Maps a logical program index to its position among the leaves.
    /// Programs occupy pairs `(2k, 2k+1)` with the Program in either slot
    /// per the blinding-factor LSB; we probe slot `2k` first, fall back
    /// to `2k+1`.
    fn program_leaf_index(&self, program_index: usize) -> Result<usize, VMError> {
        let pair = program_index
            .checked_mul(2)
            .ok_or(VMError::ProgramIndexOutOfRange)?;
        if pair >= self.leaves.len() {
            return Err(VMError::ProgramIndexOutOfRange);
        }
        Ok(match &self.leaves[pair] {
            PredicateLeaf::Program(_) => pair,
            PredicateLeaf::Blinding(_) => pair + 1,
        })
    }
}

/// Deterministically generates the leaf list: for each program, a
/// 32-byte blinding factor is squeezed from a domain-separated transcript
/// keyed by `blinding_key` and bound to the entire program list. The
/// blinding factor's LSB picks whether the program sits on the left or
/// right of its blinding sibling.
fn create_merkle_leaves(progs: &[Vec<u8>], blinding_key: &[u8; 32]) -> Vec<PredicateLeaf> {
    let mut t = Transcript::new(b"flamevm.taproot.blinding");
    let n = progs.len() as u64;
    t.append_message(b"n", &n.to_le_bytes());
    t.append_message(b"key", blinding_key);
    for prog in progs {
        t.append_message(b"prog", prog);
    }
    let mut leaves = Vec::with_capacity(progs.len() * 2);
    for prog in progs {
        let mut blinding = [0u8; 32];
        t.challenge_bytes(b"blinding", &mut blinding);
        let blinding_leaf = PredicateLeaf::Blinding(blinding);
        let program_leaf = PredicateLeaf::Program(prog.clone());
        if blinding[0] & 1 == 0 {
            leaves.push(blinding_leaf);
            leaves.push(program_leaf);
        } else {
            leaves.push(program_leaf);
            leaves.push(blinding_leaf);
        }
    }
    leaves
}

/// Balanced merkle root over an ordered leaf list. Splits at
/// `next_power_of_two(n) / 2`; a singleton leaf hashes to its own root.
fn merkle_root_of_leaves(leaves: &[PredicateLeaf]) -> [u8; 32] {
    debug_assert!(!leaves.is_empty(), "merkle_root_of_leaves: empty list");
    if leaves.len() == 1 {
        leaf_hash(&leaves[0])
    } else {
        let k = leaves.len().next_power_of_two() / 2;
        let left = merkle_root_of_leaves(&leaves[..k]);
        let right = merkle_root_of_leaves(&leaves[k..]);
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

    /// Deep-clone preserving prover-side witnesses on Token payloads.
    ///
    /// Used by `String::to_cell` when the wrapping `Arc<Cell>` is
    /// shared. `Commitment::Open` is preserved so downstream `op_mix`
    /// finds the witness intact.
    ///
    /// Errors if any payload entry isn't a portable type (a contract
    /// violation — payload is filtered through `pop_n_portable` at
    /// construction).
    pub fn try_clone_with_witnesses(&self) -> Result<Cell, VMError> {
        let mut new_payload = Vec::with_capacity(self.payload.len());
        for v in &self.payload {
            new_payload.push(clone_portable_value(v)?);
        }
        Ok(Cell {
            predicate: self.predicate.clone(),
            anchor: self.anchor,
            payload: new_payload,
        })
    }

    /// Computes the canonical identity hash of this cell, via a Merlin
    /// transcript that absorbs the opaque predicate point, the anchor,
    /// and each payload value's **canonical wire encoding**.
    ///
    /// Anchor uniqueness across a tx (guaranteed by the ratchet chain
    /// from inputs forward) makes cell-ids unique without needing the
    /// payload to disambiguate; binding the payload bytes here is
    /// belt-and-suspenders to make `id()` a true commitment to the
    /// cell's contents — needed for protocol messages (signtx/signcall)
    /// and for the txlog Output entry.
    ///
    /// Panics if a payload value's type has no canonical encoder
    /// (all portable types — Int253, String, Dict, Point, Token —
    /// encode; non-portable types are rejected by `op_cell` /
    /// `op_output` before reaching here).
    pub fn id(&self) -> [u8; 32] {
        let mut t = Transcript::new(b"flamevm.cell.id");
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

    /// Canonical wire bytes — thin wrapper over `Encodable::encode_to_vec`
    /// for callers that want an owned `Vec<u8>` (e.g. `String::Cell`
    /// serialization). Cannot fail: `Vec<u8>` is an infallible writer
    /// and payload entries are guaranteed portable by construction.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// Validates that every payload value is portable. Pure VM-level
    /// check, distinct from parsing — `Decodable::decode` accepts any
    /// well-formed cell shape; this is the gate `op_input` applies on
    /// cells coming off the witness path.
    pub fn validate_portable(&self) -> Result<(), VMError> {
        for v in &self.payload {
            if !v.is_portable() {
                return Err(VMError::MalformedCellEncoding);
            }
        }
        Ok(())
    }
}

/// Canonical wire form: a list-style `Dict` with three entries —
/// predicate (`Point`), anchor (32-byte `String`), payload (nested
/// list-style `Dict` of values). The `Decodable` side accepts any
/// well-formed shape; portability of payload values is a separate
/// VM-level gate, see [`Cell::validate_portable`].
impl Encodable for Cell {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        write_list_prefix(w, 3)?;
        let pred_point = Point::from_compressed(self.predicate.to_point());
        write_value(w, &Value::Point(pred_point))?;
        write_value(w, &Value::String(String::from(self.anchor.0.to_vec())))?;
        write_list_prefix(w, self.payload.len())?;
        for v in &self.payload {
            write_value(w, v)?;
        }
        Ok(())
    }
}

/// Reads the canonical wire form. Pure parse — accepts any
/// well-formed cell shape regardless of payload portability;
/// callers that require portable payloads call
/// [`Cell::validate_portable`] after decoding (`op_input` does).
///
/// `ReadError::InvalidFormat` on:
/// - outer shape != list-Dict of exactly 3 entries,
/// - entry 0 not a `Point`,
/// - entry 1 not a 32-byte `String`,
/// - entry 2 not a list-Dict,
/// - any payload value missing or unparseable.
impl Decodable for Cell {
    fn decode(r: &mut impl Reader) -> Result<Cell, ReadError> {
        let outer_count = read_list_prefix(r).map_err(|_| ReadError::InvalidFormat)?;
        if outer_count != 3 {
            return Err(ReadError::InvalidFormat);
        }
        let predicate = match read_value(r) {
            Ok(Some(Value::Point(p))) => Predicate::opaque(p.to_compressed()),
            _ => return Err(ReadError::InvalidFormat),
        };
        let anchor = match read_value(r) {
            Ok(Some(Value::String(s))) => {
                if s.len() != 32 {
                    return Err(ReadError::InvalidFormat);
                }
                let mut a = [0u8; 32];
                a.copy_from_slice(s.as_bytes());
                Anchor(a)
            }
            _ => return Err(ReadError::InvalidFormat),
        };
        let payload_count =
            read_list_prefix(r).map_err(|_| ReadError::InvalidFormat)?;
        let mut payload = Vec::with_capacity(payload_count);
        for _ in 0..payload_count {
            match read_value(r) {
                Ok(Some(v)) => payload.push(v),
                _ => return Err(ReadError::InvalidFormat),
            }
        }
        Ok(Cell::new(predicate, anchor, payload))
    }
}

// ── Internal hashing helpers (all via Merlin) ────────────────────

/// `H(X, M)` — the Taproot tweak scalar.
fn taproot_tweak(internal_key: &CompressedRistretto, merkle_root: &[u8; 32]) -> Scalar {
    let mut t = Transcript::new(b"flamevm.taproot");
    t.append_message(b"key", internal_key.as_bytes());
    t.append_message(b"root", merkle_root);
    let mut buf = [0u8; 64];
    t.challenge_bytes(b"h", &mut buf);
    Scalar::from_bytes_mod_order_wide(&buf)
}

/// Domain-tagged leaf hash dispatching on the variant.
fn leaf_hash(leaf: &PredicateLeaf) -> [u8; 32] {
    match leaf {
        PredicateLeaf::Program(p) => program_leaf_hash(p),
        PredicateLeaf::Blinding(b) => blinding_leaf_hash(b),
    }
}

/// Merkle leaf hash for a program leaf — also what the verifier
/// computes from `CallProof::program` before walking up.
fn program_leaf_hash(program: &[u8]) -> [u8; 32] {
    let mut t = Transcript::new(b"flamevm.merkle.leaf");
    t.append_message(b"program", program);
    let mut h = [0u8; 32];
    t.challenge_bytes(b"hash", &mut h);
    h
}

/// Merkle leaf hash for a blinding leaf. Distinct domain from the
/// program leaf so a prover can't substitute one for the other.
fn blinding_leaf_hash(bytes: &[u8; 32]) -> [u8; 32] {
    let mut t = Transcript::new(b"flamevm.merkle.leaf");
    t.append_message(b"blinding", bytes);
    let mut h = [0u8; 32];
    t.challenge_bytes(b"hash", &mut h);
    h
}

/// Merkle node hash combining two child hashes.
fn merkle_node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut t = Transcript::new(b"flamevm.merkle.node");
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

/// Clone a Value that's known to be portable. Non-portable variants
/// (Merlin / Variable / Expression / Constraint / WideToken) are never
/// present in `Cell::payload` (construction filters via `pop_n_portable`)
/// and error here.
///
/// Distinct from `Value::try_clone`, which is the *stack-level*
/// linearity gate that rejects Token/Cell to prevent implicit
/// duplication of bearer values. Cell-payload cloning is below
/// the stack level — these values are still inside a cell, not
/// live on the stack — so the clone is allowed.
fn clone_portable_value(v: &Value) -> Result<Value, VMError> {
    use crate::Token;
    match v {
        Value::Int253(i) => Ok(Value::Int253(*i)),
        Value::String(s) => Ok(Value::String(s.clone())),
        Value::Dict(d) => Ok(Value::Dict(d.try_clone()?)),
        Value::Point(p) => Ok(Value::Point(p.clone())),
        Value::Token(t) => Ok(Value::Token(Token::new(t.qty.clone(), t.flv.clone()))),
        Value::ClearToken(ct) => Ok(Value::ClearToken(*ct)),
        Value::Cell(c) => Ok(Value::Cell(c.try_clone_with_witnesses()?)),
        // Non-portable types should never appear in a cell payload.
        _ => Err(VMError::NonPortableInOutput),
    }
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
