//! Prover-side [`Delegate`] implementation.

use std::sync::OnceLock;

use bulletproofs::r1cs::{self, ConstraintSystem, R1CSProof};
use bulletproofs::{BulletproofGens, PedersenGens};
use curve25519_dalek::ristretto::CompressedRistretto;
use merlin::Transcript;

/// Shared singleton bulletproof generators (1024 generators ×
/// 1 party) — sized to cover Phase-12 64-bit range proofs plus
/// Phase-13 cloak multi-range proofs with headroom. `BulletproofGens`
/// allocation is ~16 KB; sharing across all prover/verifier
/// instances saves that cost per tx. Same sizing for both sides —
/// any divergence silently invalidates every proof.
fn shared_bp_gens() -> &'static BulletproofGens {
    static BP_GENS: OnceLock<BulletproofGens> = OnceLock::new();
    BP_GENS.get_or_init(|| BulletproofGens::new(1024, 1))
}

use crate::errors::VMError;
use crate::program::ScriptBuilder;
use crate::tx::TxHeader;
use crate::vm::{Delegate, TxResult, VM};

/// R1CS proof builder. Wraps `bulletproofs::r1cs::Prover` and a
/// `musig::BatchVerifier` (used by deferred-signature batching at
/// finalize); owns the constraint system and the bulletproof
/// generators.
///
/// Lifetimes: the inner `r1cs::Prover` borrows from the `PedersenGens`
/// passed at construction. Callers typically build a
/// `PedersenGens::default()` on the stack in a scope that contains the
/// entire prove operation; the public [`Prover::prove`] entry point
/// handles that automatically.
pub struct Prover<'g> {
    cs: r1cs::Prover<'g, Transcript>,
    batch: musig::BatchVerifier<rand::rngs::ThreadRng>,
}

impl<'g> Prover<'g> {
    /// Constructs a fresh prover. `pc_gens` must outlive the prover.
    /// Reuses the process-wide [`shared_bp_gens`] singleton to avoid
    /// re-allocating ~16 KB of generators per prover instance.
    pub fn new(pc_gens: &'g PedersenGens) -> Self {
        let cs = r1cs::Prover::new(pc_gens, Transcript::new(b"flamevm.r1cs"));
        Self {
            cs,
            batch: musig::BatchVerifier::new(rand::thread_rng()),
        }
    }

    /// Drives the inner `r1cs::Prover` to emit the proof, consuming
    /// self. Called by [`Prover::prove`] after the VM script has
    /// finished cleanly.
    pub fn into_proof(self) -> Result<R1CSProof, VMError> {
        self.cs
            .prove(shared_bp_gens())
            .map_err(|_| VMError::R1CSProofConstruction)
    }

    /// Public entry point: runs `program` through the VM in external
    /// context (witnesses attached to each Alloc) and produces the proof.
    /// The canonical TxID is bound into the R1CS transcript under
    /// `b"flamevm.txid"` — the verifier must mirror this exactly or every
    /// proof fails.
    ///
    /// Returns the full [`TxResult`]: `proof = Some(...)`,
    /// `bytecode = program.to_bytecode()` (the verifier walks the
    /// same bytes), `txid` / `txlog` / `total_fee` etc. populated.
    pub fn prove(
        pc_gens: &'g PedersenGens,
        program: ScriptBuilder,
        header: TxHeader,
        gas_limit: u64,
        mem_limit: u64,
    ) -> Result<TxResult, VMError> {
        let mut prover = Prover::new(pc_gens);
        // Run the VM but receive the result without the proof set —
        // we'll fold it in after the R1CS prove. `VM::run` consumes
        // the witness-bearing ScriptBuilder and stores its bytecode form on
        // the returned TxResult.
        let mut result = VM::run(
            header,
            program,
            gas_limit,
            mem_limit,
            &mut prover,
        )?;
        // Bind the canonical TxID into the R1CS transcript so the
        // proof commits to the full transaction effects (header +
        // log), not just the constraint system shape. Verifier
        // mirrors this exact step before `cs.verify`.
        prover.cs.transcript().append_message(b"flamevm.txid", &result.txid.0);
        let proof = prover.into_proof()?;
        result.proof = Some(proof);
        Ok(result)
    }
}

impl<'g> Delegate for Prover<'g> {
    type CS = r1cs::Prover<'g, Transcript>;
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
        // Prover-side: extract the witness (value + blinding) from the
        // open commitment and call `cs.commit(value, blinding)`. The
        // resulting point matches `commitment.to_point()` by Pedersen
        // construction (which is what the verifier independently
        // commits to its CS). `r1cs::Prover::commit` is an inherent
        // method — no `ConstraintSystem` trait import needed.
        // `WitnessMissing` is prover-only (the verifier has no witness and
        // takes the closed-point path). A script that `commit`s then
        // `expr`s over an opaque commitment fails here on the prover but
        // not the verifier — a fail-closed liveness divergence (the proof
        // binds the whole CS, so the verifier rejects rather than accepts;
        // see vm.rs `int253_to_signed_integer`). Not a soundness hole.
        let (value, blinding) =
            commitment.witness().ok_or(VMError::WitnessMissing)?;
        let scalar: curve25519_dalek::scalar::Scalar = value.into();
        Ok(self.cs.commit(scalar, blinding))
    }

}
