//! `Verifier` — the verifier-side [`Delegate`] implementation.
//!
//! Wraps `bulletproofs::r1cs::Verifier`: walks the same VM bytecode the
//! prover produced (with no witness queue — every `alloc` opcode binds
//! an unassigned variable) and then checks the supplied `R1CSProof`
//! against the accumulated constraint system. Mirrors
//! `zkvm::verifier::Verifier` in spirit.
//!
//! The verifier's transcript label must match the prover's exactly —
//! any divergence silently invalidates every proof. Both files
//! consume `flamevm.r1cs.v1`.

use bulletproofs::r1cs::{self, R1CSProof};
use bulletproofs::{BulletproofGens, PedersenGens};
use curve25519_dalek::ristretto::CompressedRistretto;
use merlin::Transcript;

use crate::errors::VMError;
use crate::tx::TxHeader;
use crate::vm::{Delegate, DeferredSig, TxResult, VM};

/// Phase-11 R1CS proof verifier. Wraps `bulletproofs::r1cs::Verifier`.
/// The verifier never sees prover witnesses — its `next_alloc_witness`
/// always returns `None`, and every `alloc` allocates an unassigned
/// variable.
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
            // MUST match the prover's bp_gens shape exactly — any
            // divergence silently invalidates every proof. See
            // `prover.rs::Prover::new` for the rationale on
            // `(1024, 1)`.
            bp_gens: BulletproofGens::new(1024, 1),
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
        let (result, sigs) = VM::run_external(
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
        // Same Phase-11 stub as the Prover — the `commit` opcode lands
        // alongside the rich `String` enum in Phase 13.
        Err(VMError::WitnessMissing)
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        // Proof verification happens via `verify_proof` after
        // [`VM::run_external`] returns; `finalize` here is a no-op
        // retained only so `Verifier` satisfies the `Delegate` trait
        // (mirrors the symmetric stub on the prover side).
        Ok(())
    }
}
