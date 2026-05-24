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
    batch: musig::BatchVerifier<rand::rngs::ThreadRng>,
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
            batch: musig::BatchVerifier::new(rand::thread_rng()),
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
    /// external context, then verifies the R1CS proof and (Phase-14)
    /// batch-verifies any `DeferredSig::Explicit` records via
    /// `Signature::verify_batched`. `DeferredSig::TxBound` records
    /// are not yet checked — they need TxID computation (Phase 17).
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
        // Phase 14: append each Explicit deferred sig to the batch.
        // TxBound sigs are deferred to Phase 17 (need TxID).
        for sig in &sigs {
            if let DeferredSig::Explicit {
                verification_key,
                message,
                signature,
            } = sig
            {
                let starsig = musig::Signature::from_bytes(*signature)
                    .map_err(|_| VMError::BadSignatureBytes)?;
                let vk = musig::VerificationKey::from_compressed(*verification_key);
                let mut t = merlin::Transcript::new(b"flamevm.signrun.v1");
                t.append_message(b"msg", message);
                starsig.verify_batched(&mut t, vk, &mut verifier.batch);
            }
        }
        // Verify R1CS proof first, then drain the deferred-sig batch.
        // Both must pass for the tx to be valid. Destructure so both
        // consume-by-value methods work without borrow conflicts.
        let Verifier { cs, batch, bp_gens } = verifier;
        cs.verify(proof, pc_gens, &bp_gens)
            .map_err(|_| VMError::InvalidR1CSProof)?;
        batch
            .verify()
            .map_err(|_| VMError::BatchSignatureVerificationFailed)?;
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
    type BatchVerifier = musig::BatchVerifier<rand::rngs::ThreadRng>;

    fn cs(&mut self) -> &mut Self::CS {
        &mut self.cs
    }

    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier {
        &mut self.batch
    }

    fn commit_variable(
        &mut self,
        commitment: &crate::Commitment,
    ) -> Result<(CompressedRistretto, r1cs::Variable), VMError> {
        // Verifier-side: only the closed point is known. Call
        // `cs.commit(point)` — bulletproofs allocates a CS variable
        // bound to that point. The prover's matching call uses
        // (value, blinding) which produces the same point by Pedersen
        // construction, so both sides commit to the same value.
        // `r1cs::Verifier::commit` is an inherent method — no
        // `ConstraintSystem` trait import needed.
        let point = commitment.to_point();
        let var = self.cs.commit(point);
        Ok((point, var))
    }

    fn finalize(self, _deferred_sigs: Vec<DeferredSig>) -> Result<(), VMError> {
        // Proof verification happens via `verify_proof` after
        // [`VM::run_external`] returns; `finalize` here is a no-op
        // retained only so `Verifier` satisfies the `Delegate` trait
        // (mirrors the symmetric stub on the prover side).
        Ok(())
    }
}
