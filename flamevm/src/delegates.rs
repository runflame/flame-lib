//! Real `Prover` and `Verifier` [`Delegate`] implementations — Phase 11.
//!
//! ## Architecture (zkvm-derived)
//!
//! Bulletproofs' `r1cs::Prover` and `r1cs::Verifier` are two different
//! constraint-system *clients*: the prover owns the witness scalars and
//! ultimately calls `prove(&bp_gens)` to emit an `R1CSProof`, while the
//! verifier holds the same shape of constraint system but only the
//! public points and ultimately calls `verify(&proof, …)`. FlameVM's
//! `Delegate` trait paramterizes the VM over either client.
//!
//! The prover/verifier asymmetry surfaces in two places:
//!
//! - `commit_variable(commitment)`: the prover unpacks the open
//!   `Commitment` for its witness and calls `cs.commit(value,
//!   blinding)`; the verifier just calls `cs.commit(point)` because it
//!   only has the closed commitment.
//! - `next_alloc_witness()`: each `alloc` opcode pulls one witness from
//!   the prover's queue (built from `Program::to_witnesses`). The
//!   verifier's queue is always empty and returns `None` — the R1CS
//!   variable is allocated without an assignment.
//!
//! Both finalize by handing the deferred signatures back; Phase 11
//! does not yet batch-verify them — that lands in Phase 14.

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

// ── Prover ─────────────────────────────────────────────────────────

/// Phase-11 R1CS proof builder. Wraps `bulletproofs::r1cs::Prover` and a
/// FIFO queue of alloc-witnesses the VM consumes in opcode-emission
/// order.
///
/// Lifetimes: the inner `r1cs::Prover` borrows from the `PedersenGens`
/// passed at construction. The current Phase-11 API holds the
/// `PedersenGens` and the `BulletproofGens` inside the prover struct
/// so callers don't need to thread them; future phases (mix / decrypt)
/// may need a richer constructor.
pub struct Prover<'g> {
    cs: r1cs::Prover<'g, Transcript>,
    alloc_witnesses: VecDeque<Option<Int253>>,
    bp_gens: BulletproofGens,
}

impl<'g> Prover<'g> {
    /// Constructs a fresh prover. `pc_gens` must outlive the prover —
    /// callers typically build a `PedersenGens::default()` on the stack
    /// in a scope that contains the entire prove operation. Allocates a
    /// fresh `BulletproofGens` sized for the expected R1CS multipliers
    /// (Phase 11 uses a small bound; Phase 12 raises it).
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
    /// self. Called by `Prover::prove` after the VM script has
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
        // `pop_front` returns Option<Option<Int253>>. Flattening the two
        // layers: outer None means "queue ran out", inner None means
        // "intentionally unassigned witness". Phase 11 treats both the
        // same (no assignment); future phases distinguish via the
        // `WitnessMissing` error path.
        self.alloc_witnesses.pop_front().unwrap_or(None)
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        // The proof is produced by `into_proof` instead — `finalize`'s
        // signature can't return the proof bytes without changing the
        // trait. The public `Prover::prove` entry point in `lib.rs`
        // calls `into_proof` after `VM::execute_external` returns.
        Ok(())
    }
}

// ── Verifier ───────────────────────────────────────────────────────

/// Phase-11 R1CS proof verifier. Wraps `bulletproofs::r1cs::Verifier`
/// and an empty witness queue — the verifier never sees prover
/// witnesses.
pub struct Verifier {
    cs: r1cs::Verifier<Transcript>,
    bp_gens: BulletproofGens,
}

impl Verifier {
    /// Constructs a fresh verifier. The transcript label matches the
    /// prover's exactly — divergence here would silently invalidate
    /// every proof.
    pub fn new() -> Self {
        let cs = r1cs::Verifier::new(Transcript::new(b"flamevm.r1cs.v1"));
        Self {
            cs,
            bp_gens: BulletproofGens::new(64, 16),
        }
    }

    /// Verifies the provided proof against the constraint system the
    /// VM accumulated, consuming `self`. Errors `InvalidR1CSProof` on
    /// any failure (tampered proof, mismatched CS, …).
    pub fn verify_proof(
        self,
        proof: &R1CSProof,
        pc_gens: &PedersenGens,
    ) -> Result<(), VMError> {
        self.cs
            .verify(proof, pc_gens, &self.bp_gens)
            .map_err(|_| VMError::InvalidR1CSProof)
    }

    /// Public Phase-11 entry point: runs `bytecode` through the VM in
    /// external context, then verifies the R1CS proof.
    pub fn verify(
        pc_gens: &PedersenGens,
        bytecode: Vec<u8>,
        proof: &R1CSProof,
        header: TxHeader,
        gas_limit: u64,
        mem_limit: u64,
    ) -> Result<(TxResult, Vec<DeferredSig>), VMError> {
        let mut verifier = Verifier::new();
        let (result, sigs) = VM::execute_external_keep_delegate(
            header,
            bytecode,
            gas_limit,
            mem_limit,
            &mut verifier,
        )?;
        verifier.verify_proof(proof, pc_gens)?;
        Ok((result, sigs))
    }
}

impl Default for Verifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Delegate for Verifier {
    type CS = r1cs::Verifier<Transcript>;

    fn cs(&mut self) -> &mut Self::CS {
        &mut self.cs
    }

    fn commit_variable(
        &mut self,
        _commitment: &CompressedRistretto,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError> {
        // Same Phase-11 stub as Prover — the `commit` opcode lands
        // alongside the rich `String` enum in Phase 13.
        Err(VMError::WitnessMissing)
    }

    fn next_alloc_witness(&mut self) -> Option<Int253> {
        None // Verifier never has witness.
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        // Proof verification happens via `verify_proof` after the VM
        // exits cleanly, mirroring the prover's `into_proof`.
        Ok(())
    }
}
