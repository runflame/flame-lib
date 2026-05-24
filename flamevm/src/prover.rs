//! `Prover` — the prover-side [`Delegate`] implementation.
//!
//! Wraps `bulletproofs::r1cs::Prover`: owns the witness scalars (carried
//! inline in each [`Instruction::Alloc(Some(int))`]), walks the VM in
//! external context, then emits an `R1CSProof`. Mirrors
//! `zkvm::prover::Prover` in spirit — the constraint system is shared
//! with the verifier; only the proof-machinery differs.
//!
//! ## Witness flow
//!
//! Witnesses live inside `Instruction::Alloc(Option<Int253>)` and travel
//! with the prover's [`crate::Program`]. The VM, when stepping through a
//! `Run::Queue` populated from a Program, sees each Alloc with its
//! cleartext witness intact, and binds the witness to a newly allocated
//! R1CS variable. The verifier walks the same bytecode (via
//! `Run::Bytecode`), where every Alloc parses to `Alloc(None)` — the
//! variable is left unassigned and the constraint system fills it in
//! algebraically during proof verification.
//!
//! `commit_variable` is reserved for the Phase-13 `commit` opcode and
//! currently returns `WitnessMissing` (Phase 11 doesn't yet wire a
//! rich-`String` carrier for open commitments).
//!
//! [`Instruction::Alloc(Some(int))`]: crate::ops::Instruction::Alloc

use bulletproofs::r1cs::{self, R1CSProof};
use bulletproofs::{BulletproofGens, PedersenGens};
use curve25519_dalek::ristretto::CompressedRistretto;
use merlin::Transcript;

use crate::errors::VMError;
use crate::program::Program;
use crate::tx::TxHeader;
use crate::vm::{Delegate, DeferredSig, TxResult, VM};

/// Phase-11 R1CS proof builder. Wraps `bulletproofs::r1cs::Prover`;
/// owns the constraint system and the bulletproof generators.
///
/// Lifetimes: the inner `r1cs::Prover` borrows from the `PedersenGens`
/// passed at construction. Callers typically build a
/// `PedersenGens::default()` on the stack in a scope that contains the
/// entire prove operation; the public [`Prover::prove`] entry point
/// handles that automatically.
pub struct Prover<'g> {
    cs: r1cs::Prover<'g, Transcript>,
    bp_gens: BulletproofGens,
}

impl<'g> Prover<'g> {
    /// Constructs a fresh prover. `pc_gens` must outlive the prover.
    /// Allocates a fresh `BulletproofGens` sized for the expected R1CS
    /// multipliers (Phase 11 uses a small bound; Phase 12 raises it).
    pub fn new(pc_gens: &'g PedersenGens) -> Self {
        let cs = r1cs::Prover::new(pc_gens, Transcript::new(b"flamevm.r1cs.v1"));
        Self {
            cs,
            // 1024-gen single-party setup matches the zkvm test
            // configuration (`BulletproofGens::new(256, 1)` in
            // zkvm/tests/zkvm.rs is the lower bound; we go a bit
            // higher to leave headroom for Phase-13 `cloak`
            // multi-range proofs). Party capacity is 1 because R1CS
            // proofs are single-party.
            bp_gens: BulletproofGens::new(1024, 1),
        }
    }

    /// Drives the inner `r1cs::Prover` to emit the proof, consuming
    /// self. Called by [`Prover::prove`] after the VM script has
    /// finished cleanly.
    pub fn into_proof(self) -> Result<R1CSProof, VMError> {
        self.cs
            .prove(&self.bp_gens)
            .map_err(|_| VMError::R1CSProofConstruction)
    }

    /// Public Phase-11 entry point: runs `program` through the VM in
    /// external context (with witnesses attached to each Alloc
    /// Instruction), then emits an R1CS proof.
    ///
    /// Returns the canonical bytecode (so the verifier has the byte
    /// sequence to walk) plus the proof, the `TxResult`, and the
    /// `DeferredSig`s.
    pub fn prove(
        pc_gens: &'g PedersenGens,
        program: Program,
        header: TxHeader,
        gas_limit: u64,
        mem_limit: u64,
    ) -> Result<(Vec<u8>, R1CSProof, TxResult, Vec<DeferredSig>), VMError> {
        let bytecode = program.to_bytecode();
        let mut prover = Prover::new(pc_gens);
        let (result, sigs) = VM::run_external_program(
            header,
            program,
            gas_limit,
            mem_limit,
            &mut prover,
        )?;
        let proof = prover.into_proof()?;
        Ok((bytecode, proof, result, sigs))
    }
}

impl<'g> Delegate for Prover<'g> {
    type CS = r1cs::Prover<'g, Transcript>;

    fn cs(&mut self) -> &mut Self::CS {
        &mut self.cs
    }

    fn commit_variable(
        &mut self,
        _commitment: &CompressedRistretto,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError> {
        // Phase 11 doesn't yet exercise `commit_variable` (no `commit`
        // opcode wired). The interface stays here so Phase 13 can plug
        // in the open-commitment witness path.
        Err(VMError::WitnessMissing)
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        // The proof is produced by `into_proof` instead — `finalize`'s
        // signature can't return the proof bytes without changing the
        // trait. [`Prover::prove`] calls `into_proof` after the VM run
        // returns; `finalize` here is a no-op retained only so `Prover`
        // satisfies the `Delegate` trait.
        Ok(())
    }
}
