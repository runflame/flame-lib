//! Linear Contracts and Cell-backed Taproot predicates.

use bulletproofs::PedersenGens;
use cells::{
    BagOfCells, CellBuilder, CellDecode, CellEncode, CellError, CellID, CellRef, CellResolver,
    CellSlice, Trie,
};
use core::{any::Any, fmt};
use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;
use std::{convert::TryFrom, sync::Arc};

use crate::{errors::VMError, vm::Anchor, ScriptBuilder, Value};

/// The Cell ID of a Contract output: predicate, anchor, and one payload Value.
pub type ContractID = CellID;

/// Prover-side metadata; never part of a Predicate's 32-byte public encoding.
pub trait PredicateWitness: Any + Send + Sync + fmt::Debug {
    fn to_point(&self) -> CompressedRistretto;
    fn clone_witness(&self) -> Box<dyn PredicateWitness>;
    fn as_any(&self) -> &dyn Any;
}

/// Unlock condition P = X + H(X, root Cell ID) · B.
pub struct Predicate {
    pub(crate) point: CompressedRistretto,
    pub(crate) witness: Option<Box<dyn PredicateWitness>>,
}

impl Clone for Predicate {
    fn clone(&self) -> Self {
        Self {
            point: self.point,
            witness: self.witness.as_ref().map(|w| w.clone_witness()),
        }
    }
}

impl fmt::Debug for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Predicate").field(&self.point).finish()
    }
}

/// The sum tag distinguishes executable programs from random blinding data.
#[derive(Clone, Debug)]
pub enum PredicateLeaf {
    Program(Vec<u8>),
    Blinding([u8; 32]),
}

impl CellEncode for PredicateLeaf {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        match self {
            Self::Program(program) => {
                builder.store_u8(0)?.store_snake(program)?;
            }
            Self::Blinding(bytes) => {
                builder.store_u8(1)?.store_bytes(bytes)?;
            }
        }
        Ok(())
    }
}

/// Prover witness containing the resident program Trie and its blinding leaves.
///
/// Each program occupies one randomly selected slot in a pair of leaves. The
/// other slot contains a deterministic, secret-seeded blinding value. The root
/// Cell is the raw root of an eight-byte-key Trie, with no count envelope.
/// Its Cell ID commits directly to the paths and leaves in the Taproot tweak.
#[derive(Clone, Debug)]
pub struct PredicateTree {
    pub(crate) internal_key: CompressedRistretto,
    pub(crate) leaves: Vec<PredicateLeaf>,
    root: CellRef,
    pub(crate) point: CompressedRistretto,
    /// Optional prover programs in logical input order; never public encoding.
    scripts: Vec<ScriptBuilder>,
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
    pub fn opaque(point: CompressedRistretto) -> Self {
        Self {
            point,
            witness: None,
        }
    }

    pub fn with_witness<W: PredicateWitness>(witness: W) -> Self {
        Self {
            point: witness.to_point(),
            witness: Some(Box::new(witness)),
        }
    }

    pub fn tree(tree: PredicateTree) -> Self {
        Self::with_witness(tree)
    }
    pub fn to_point(&self) -> CompressedRistretto {
        self.point
    }
    pub fn to_opaque(&self) -> Self {
        Self::opaque(self.point)
    }

    pub fn witness_as<W: PredicateWitness>(&self) -> Option<&W> {
        self.witness.as_ref()?.as_any().downcast_ref::<W>()
    }

    pub fn verification_key(&self) -> CompressedRistretto {
        self.point
    }

    /// Unknown discrete log, disabling the key-path spend.
    pub fn unspendable_key() -> CompressedRistretto {
        PedersenGens::default().B_blinding.compress()
    }

    /// Authenticates a branch selector and loads its code through this execution's
    /// resolver. There are no separate sibling hashes or caller-supplied code.
    /// Missing path/continuation Cells are hard errors, never alternate branches.
    pub fn open_branch<R: CellResolver + ?Sized>(
        &self,
        proof: &TaprootProof,
        cells: &mut R,
        max_program_bytes: usize,
    ) -> Result<Vec<u8>, VMError> {
        let internal = proof
            .internal_key
            .decompress()
            .ok_or(VMError::TaprootProofMismatch)?;
        let tweak = taproot_tweak(&proof.internal_key, &proof.root);
        if (internal + RISTRETTO_BASEPOINT_TABLE * &tweak).compress() != self.point {
            return Err(VMError::TaprootProofMismatch);
        }
        let leaf = Trie::lookup(
            &CellRef::unresolved(proof.root),
            &proof.index.to_be_bytes(),
            cells,
        )?
        .ok_or(VMError::TaprootProofMismatch)?;
        let mut slice = CellSlice::new(&leaf);
        if slice.load_u8()? != 0 {
            return Err(VMError::TaprootProofMismatch);
        }
        let program = slice.load_snake(cells, max_program_bytes)?;
        slice.finish()?;
        Ok(program)
    }
}

impl CellEncode for Predicate {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_bytes(self.point.as_bytes())?;
        Ok(())
    }
}

impl CellDecode for Predicate {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self::opaque(CompressedRistretto(<[u8; 32]>::decode(
            slice, cells,
        )?)))
    }
}

impl PredicateTree {
    /// Builds a deterministic, blinded program Trie. None disables key spends.
    pub fn new(
        internal_key: Option<CompressedRistretto>,
        programs: Vec<Vec<u8>>,
        blinding_key: [u8; 32],
    ) -> Result<Self, VMError> {
        if programs.is_empty() {
            return Err(VMError::EmptyPredicateTree);
        }
        let internal_key = internal_key.unwrap_or_else(Predicate::unspendable_key);
        let internal = internal_key.decompress().ok_or(VMError::InvalidPoint)?;
        let leaves = create_blinded_leaves(&programs, &blinding_key);
        let mut trie = Trie::new(8)?;
        for (index, leaf) in leaves.iter().enumerate() {
            let index = u64::try_from(index).map_err(|_| VMError::ProgramIndexOutOfRange)?;
            trie.insert(&index.to_be_bytes(), leaf.to_cell()?, &mut ())?;
        }
        let root = trie.into_root().expect("programs are nonempty");
        let tweak = taproot_tweak(&internal_key, &root.id());
        let point = (internal + RISTRETTO_BASEPOINT_TABLE * &tweak).compress();
        Ok(Self {
            internal_key,
            leaves,
            root,
            point,
            scripts: Vec::new(),
        })
    }

    /// Builds the same public tree as [`Self::new`], retaining each program's
    /// private assignments and embedded public witnesses for the prover.
    /// [`ScriptBuilder::push_taproot_proof`] attaches only the selected program.
    pub fn from_scripts(
        internal_key: Option<CompressedRistretto>,
        programs: Vec<ScriptBuilder>,
        blinding_key: [u8; 32],
    ) -> Result<Self, VMError> {
        let mut tree = Self::new(
            internal_key,
            programs.iter().map(ScriptBuilder::to_bytecode).collect(),
            blinding_key,
        )?;
        tree.scripts = programs;
        Ok(tree)
    }

    pub(crate) fn script_witness(&self, program_index: usize) -> Option<&ScriptBuilder> {
        self.scripts.get(program_index)
    }

    pub fn scripts_only(programs: Vec<Vec<u8>>, blinding_key: [u8; 32]) -> Result<Self, VMError> {
        Self::new(None, programs, blinding_key)
    }
    pub fn internal_key(&self) -> &CompressedRistretto {
        &self.internal_key
    }
    pub fn leaves(&self) -> &[PredicateLeaf] {
        &self.leaves
    }

    pub fn programs(&self) -> impl Iterator<Item = &[u8]> {
        self.leaves.iter().filter_map(|leaf| match leaf {
            PredicateLeaf::Program(program) => Some(program.as_slice()),
            PredicateLeaf::Blinding(_) => None,
        })
    }
    pub fn root(&self) -> &CellRef {
        &self.root
    }
    pub fn root_id(&self) -> CellID {
        self.root.id()
    }

    /// Selects a program in original input order. The proof index is its actual
    /// blinded Trie position, not its logical program index.
    pub fn taproot_proof_for(&self, program_index: usize) -> Result<TaprootProof, VMError> {
        let pair = program_index
            .checked_mul(2)
            .ok_or(VMError::ProgramIndexOutOfRange)?;
        let first = self
            .leaves
            .get(pair)
            .ok_or(VMError::ProgramIndexOutOfRange)?;
        let index = pair + usize::from(matches!(first, PredicateLeaf::Blinding(_)));
        Ok(TaprootProof {
            internal_key: self.internal_key,
            root: self.root.id(),
            index: u64::try_from(index).map_err(|_| VMError::ProgramIndexOutOfRange)?,
        })
    }

    /// Records only Cells read to open this program, including snake overflow.
    /// Unused program/blinding bodies remain unloaded in the returned witness bag.
    pub fn witness_for(&self, program_index: usize) -> Result<(TaprootProof, BagOfCells), VMError> {
        let proof = self.taproot_proof_for(program_index)?;
        let mut recorder = BranchRecorder {
            root: &self.root,
            recorded: BagOfCells::new(),
        };
        Predicate::opaque(self.point).open_branch(&proof, &mut recorder, u32::MAX as usize)?;
        Ok((proof, recorder.recorded))
    }
}

struct BranchRecorder<'a> {
    root: &'a CellRef,
    recorded: BagOfCells,
}

impl CellResolver for BranchRecorder<'_> {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<cells::Cell>, CellError> {
        let source = if reference.id() == self.root.id() {
            self.root
        } else {
            reference
        };
        let cell = source
            .as_resident_arc()
            .cloned()
            .ok_or(CellError::MissingCell(source.id()))?;
        self.recorded.insert(Arc::clone(&cell))?;
        Ok(cell)
    }
}

fn create_blinded_leaves(programs: &[Vec<u8>], blinding_key: &[u8; 32]) -> Vec<PredicateLeaf> {
    let mut transcript = Transcript::new(b"flamevm.taproot.blinding");
    transcript.append_message(b"n", &(programs.len() as u64).to_le_bytes());
    transcript.append_message(b"key", blinding_key);
    for program in programs {
        transcript.append_message(b"prog", program);
    }
    let mut leaves = Vec::with_capacity(programs.len() * 2);
    for program in programs {
        let mut blinding = [0; 32];
        transcript.challenge_bytes(b"blinding", &mut blinding);
        let pair = if blinding[0] & 1 == 0 {
            [
                PredicateLeaf::Blinding(blinding),
                PredicateLeaf::Program(program.clone()),
            ]
        } else {
            [
                PredicateLeaf::Program(program.clone()),
                PredicateLeaf::Blinding(blinding),
            ]
        };
        leaves.extend(pair);
    }
    leaves
}

/// An authenticated root and leaf selector. Path Cells live in the transaction
/// BoC; this contains neither a parallel proof encoding nor a copy of the code.
#[derive(Clone, Debug)]
pub struct TaprootProof {
    pub internal_key: CompressedRistretto,
    pub root: CellID,
    pub index: u64,
}

impl CellEncode for TaprootProof {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store_bytes(self.internal_key.as_bytes())?
            .store_bytes(&self.root)?
            .store_u64(self.index)?;
        Ok(())
    }
}

impl CellDecode for TaprootProof {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self {
            internal_key: CompressedRistretto(<[u8; 32]>::decode(slice, cells)?),
            root: <[u8; 32]>::decode(slice, cells)?,
            index: slice.load_u64()?,
        })
    }
}

/// A linear Contract protecting a single portable payload Value.
#[derive(Clone, Debug)]
pub struct Contract {
    pub predicate: Predicate,
    pub anchor: Anchor,
    payload: Value,
}

impl Contract {
    /// Portability is enforced on entry into the Contract domain, not by codecs.
    pub fn new(predicate: Predicate, anchor: Anchor, payload: Value) -> Result<Self, VMError> {
        if !payload.is_portable() {
            return Err(VMError::NonPortableInOutput);
        }
        let contract = Self {
            predicate,
            anchor,
            payload,
        };
        // Admission is fallible even for portable values: excessive nesting or
        // payload length must never reach an infallible identity computation.
        contract.to_cell()?;
        Ok(contract)
    }
    pub fn payload(&self) -> &Value {
        &self.payload
    }
    pub fn into_payload(self) -> Value {
        self.payload
    }

    /// Restores a previously admitted Contract without scanning hidden Dicts.
    /// "Trusted" refers to the Dict counts/capability summaries checked when the
    /// output was created, not to its sender or a hash alone. The caller must
    /// bind this body to an accepted input commitment (including chain membership
    /// validation). Unlike ordinary `CellDecode`, hidden branches remain unloaded;
    /// accessed nodes and values still undergo their normal decoding checks.
    pub fn from_trusted_cell<R: CellResolver + ?Sized>(
        cell: &cells::Cell,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        let mut slice = CellSlice::new(cell);
        let predicate = Predicate::decode(&mut slice, resolver)?;
        let anchor = Anchor(<[u8; 32]>::decode(&mut slice, resolver)?);
        let payload = Value::decode_trusted(&mut slice, resolver)?;
        slice.finish()?;
        Self::new(predicate, anchor, payload).map_err(|_| CellError::InvalidFormat)
    }
    pub(crate) fn clone_gas(&self) -> u64 {
        1u64.saturating_add(self.payload.clone_gas())
    }

    /// Plain SHA256 identity of the canonical output Cell record.
    pub fn id(&self) -> ContractID {
        self.to_cell()
            .expect("portable Contract payload has a Cell encoding")
            .id()
    }
}

/// Output encoding only: a Contract is not itself a portable VM Value.
impl CellEncode for Contract {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder
            .store(&self.predicate)?
            .store_bytes(&self.anchor.0)?
            .store(&self.payload)?;
        Ok(())
    }
}

impl CellDecode for Contract {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        let predicate = Predicate::decode(slice, cells)?;
        let anchor = Anchor(<[u8; 32]>::decode(slice, cells)?);
        let payload = Value::decode(slice, cells)?;
        Self::new(predicate, anchor, payload).map_err(|_| CellError::InvalidFormat)
    }
}

/// Merlin is retained for proof binding, not Cell content addressing.
fn taproot_tweak(internal_key: &CompressedRistretto, root: &CellID) -> Scalar {
    let mut transcript = Transcript::new(b"flamevm.taproot");
    transcript.append_message(b"key", internal_key.as_bytes());
    transcript.append_message(b"root", root);
    let mut bytes = [0; 64];
    transcript.challenge_bytes(b"h", &mut bytes);
    Scalar::from_bytes_mod_order_wide(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_rejects_unencodable_depth_before_identity_is_used() {
        let mut value = Value::Scalar(crate::Scalar::ONE);
        for _ in 0..=crate::encoding::MAX_VALUE_DEPTH {
            value = Value::Dict(crate::Dict::from_values(vec![value]));
        }
        assert!(value.is_portable());
        assert!(matches!(
            Contract::new(
                Predicate::opaque(Predicate::unspendable_key()),
                Anchor([0; 32]),
                value
            ),
            Err(VMError::Cell(CellError::LimitExceeded))
        ));
    }

    #[test]
    fn contract_id_is_its_output_cell_id_and_payload_is_one_value() {
        let contract = Contract::new(
            Predicate::opaque(Predicate::unspendable_key()),
            Anchor([9; 32]),
            Value::Scalar(crate::Scalar::from(7u64)),
        )
        .unwrap();
        let cell = contract.to_cell().unwrap();
        assert_eq!(
            &cell.payload()[..32],
            contract.predicate.to_point().as_bytes()
        );
        assert_eq!(&cell.payload()[32..64], &[9; 32]);
        assert_eq!(contract.id(), cell.id());
        let decoded = Contract::from_cell(&cell, &mut ()).unwrap();
        assert_eq!(decoded.id(), contract.id());
        assert!(
            matches!(decoded.payload(), Value::Scalar(value) if *value == crate::Scalar::from(7u64))
        );
        assert!(matches!(
            Contract::new(
                Predicate::opaque(Predicate::unspendable_key()),
                Anchor([0; 32]),
                Value::Contract(Box::new(contract)),
            ),
            Err(VMError::NonPortableInOutput)
        ));
    }

    #[test]
    fn branch_witness_opens_only_the_selected_program_and_checks_commitments() {
        let programs = vec![vec![1], vec![2; 20_000], vec![3]];
        let tree = PredicateTree::scripts_only(programs.clone(), [7; 32]).unwrap();
        let mut trie = Trie::new(8).unwrap();
        for (index, leaf) in tree.leaves().iter().enumerate() {
            trie.insert(
                &(index as u64).to_be_bytes(),
                leaf.to_cell().unwrap(),
                &mut (),
            )
            .unwrap();
        }
        assert_eq!(
            tree.root_id(),
            trie.root_id().unwrap(),
            "no predicate count-wrapper Cell"
        );
        let predicate = Predicate::opaque(tree.point);
        let (proof, mut bag) = tree.witness_for(1).unwrap();
        assert_eq!(
            predicate.open_branch(&proof, &mut bag, 20_000).unwrap(),
            programs[1]
        );
        assert!(predicate.open_branch(&proof, &mut bag, 19_999).is_err());
        assert!(predicate
            .open_branch(&proof, &mut BagOfCells::new(), 20_000)
            .is_err());
        let other = tree.taproot_proof_for(0).unwrap();
        assert!(predicate.open_branch(&other, &mut bag, 20_000).is_err());
        let mut missing = proof.clone();
        missing.index = u64::MAX;
        assert!(matches!(
            predicate.open_branch(&missing, &mut bag, 20_000),
            Err(VMError::TaprootProofMismatch)
        ));
        let mut forged = proof.clone();
        forged.root[0] ^= 1;
        assert!(matches!(
            predicate.open_branch(&forged, &mut bag, 20_000),
            Err(VMError::TaprootProofMismatch)
        ));
        forged = proof;
        forged.index ^= 1;
        let mut all = BagOfCells::collect(tree.root.as_resident_arc().unwrap().clone()).unwrap();
        assert!(matches!(
            predicate.open_branch(&forged, &mut all, 20_000),
            Err(VMError::TaprootProofMismatch)
        ));
    }

    #[test]
    fn predicate_construction_is_deterministic_and_blinded() {
        let programs = vec![vec![1], vec![2], vec![3]];
        let a = PredicateTree::scripts_only(programs.clone(), [1; 32]).unwrap();
        let b = PredicateTree::scripts_only(programs.clone(), [1; 32]).unwrap();
        let c = PredicateTree::scripts_only(programs, [2; 32]).unwrap();
        assert_eq!(a.root_id(), b.root_id());
        assert_eq!(a.point, b.point);
        assert_ne!(a.root_id(), c.root_id());
        assert_ne!(a.point, c.point);
        assert_eq!(a.internal_key, Predicate::unspendable_key());
        assert!(matches!(
            a.taproot_proof_for(3),
            Err(VMError::ProgramIndexOutOfRange)
        ));
        let proof = a.taproot_proof_for(0).unwrap();
        let cell = proof.to_cell().unwrap();
        assert_eq!(cell.payload().len(), 72);
        assert!(cell.refs().is_empty());
        let decoded = TaprootProof::from_cell(&cell, &mut ()).unwrap();
        assert_eq!(decoded.root, proof.root);
        assert_eq!(decoded.index, proof.index);
    }
}
