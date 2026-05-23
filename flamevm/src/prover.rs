//! `Prover` — the prover-side [`Delegate`] implementation.
//!
//! Wraps `bulletproofs::r1cs::Prover`: owns the witness scalars, walks
//! the VM in external context, then emits an `R1CSProof`. Mirrors
//! `zkvm::prover::Prover` in spirit — the constraint system is shared,
//! only the proof-machinery differs from the verifier's side.
//!
//! ## Witness flow
//!
//! The prover holds a FIFO queue of optional `Int253`s built from
//! [`Program::to_witnesses`]. Each `0x5c alloc` opcode pulls one
//! witness from the queue and binds it to a newly allocated R1CS
//! variable, so the constraint system can prove the script's
//! arithmetic against concrete values.
//!
//! `commit_variable` is reserved for the Phase-13 `commit` opcode and
//! currently returns `WitnessMissing` (Phase 11 doesn't yet wire a
//! rich-`String` carrier for open commitments).

use std::collections::VecDeque;

use bulletproofs::r1cs::{self, ConstraintSystem, R1CSProof};
use bulletproofs::{BulletproofGens, PedersenGens};
use curve25519_dalek::ristretto::CompressedRistretto;
use merlin::Transcript;

use crate::errors::VMError;
use crate::int253::Int253;
use crate::program::Program;
use crate::tx::TxHeader;
use crate::vm::{Delegate, DeferredSig, TxResult, VM};

/// Phase-11 R1CS proof builder. Wraps `bulletproofs::r1cs::Prover` and a
/// FIFO queue of alloc-witnesses the VM consumes in opcode-emission
/// order.
///
/// Lifetimes: the inner `r1cs::Prover` borrows from the `PedersenGens`
/// passed at construction. Callers typically build a
/// `PedersenGens::default()` on the stack in a scope that contains the
/// entire prove operation; the public [`Prover::prove`] entry point
/// handles that automatically.
pub struct Prover<'g> {
    cs: r1cs::Prover<'g, Transcript>,
    alloc_witnesses: VecDeque<Option<Int253>>,
    bp_gens: BulletproofGens,
}

impl<'g> Prover<'g> {
    /// Constructs a fresh prover. `pc_gens` must outlive the prover.
    /// Allocates a fresh `BulletproofGens` sized for the expected R1CS
    /// multipliers (Phase 11 uses a small bound; Phase 12 raises it).
    pub fn new(
        pc_gens: &'g PedersenGens,
        witnesses: VecDeque<Option<Int253>>,
    ) -> Self {
        let cs = r1cs::Prover::new(pc_gens, Transcript::new(b"flamevm.r1cs.v1"));
        Self {
            cs,
            alloc_witnesses: witnesses,
            // 64 generators × 16 parties matches the zkvm bound; plenty
            // for the trivial Phase-11 programs and gives Phase 12 room.
            bp_gens: BulletproofGens::new(64, 16),
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
    /// external context, then emits an R1CS proof.
    ///
    /// Returns the original bytecode (so the verifier has the canonical
    /// byte sequence to walk) plus the proof. Returns the `TxResult` /
    /// `DeferredSig`s too, in case the caller needs them — Phase 11
    /// callers usually don't.
    pub fn prove(
        pc_gens: &'g PedersenGens,
        program: Program,
        header: TxHeader,
        gas_limit: u64,
        mem_limit: u64,
    ) -> Result<(Vec<u8>, R1CSProof, TxResult, Vec<DeferredSig>), VMError> {
        let bytecode = program.to_bytecode();
        let witnesses = program.to_witnesses();
        let mut prover = Prover::new(pc_gens, witnesses);
        let (result, sigs) = VM::execute_external_keep_delegate(
            header,
            bytecode.clone(),
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

    fn next_alloc_witness(&mut self) -> Option<Int253> {
        // `pop_front` returns `Option<Option<Int253>>`. Flattening the
        // two layers: outer `None` means "queue ran out", inner `None`
        // means "intentionally unassigned witness". Phase 11 treats
        // both the same (no assignment); future phases distinguish via
        // the `WitnessMissing` error path.
        self.alloc_witnesses.pop_front().unwrap_or(None)
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        // The proof is produced by `into_proof` instead — `finalize`'s
        // signature can't return the proof bytes without changing the
        // trait. [`Prover::prove`] calls `into_proof` after
        // `VM::execute_external_keep_delegate` returns.
        Ok(())
    }
}
