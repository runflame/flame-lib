//! Verifier-side [`Delegate`] implementation.

use std::sync::OnceLock;

use bulletproofs::r1cs::{self, ConstraintSystem, R1CSProof};
use bulletproofs::{BulletproofGens, PedersenGens};
use curve25519_dalek::ristretto::CompressedRistretto;
use merlin::Transcript;

/// Shared singleton bulletproof generators. **Must** match
/// `prover::shared_bp_gens` exactly — any divergence silently
/// invalidates every proof. See `prover.rs` for the sizing
/// rationale.
fn shared_bp_gens() -> &'static BulletproofGens {
    static BP_GENS: OnceLock<BulletproofGens> = OnceLock::new();
    BP_GENS.get_or_init(|| BulletproofGens::new(1024, 1))
}

use crate::errors::VMError;
use crate::tx::TxHeader;
use crate::vm::{Delegate, DeferredSig, TxResult, VM};

/// Phase-11 R1CS proof verifier. Wraps `bulletproofs::r1cs::Verifier`.
/// The verifier never sees prover witnesses — its `next_alloc_witness`
/// always returns `None`, and every `alloc` allocates an unassigned
/// variable.
pub struct Verifier {
    cs: r1cs::Verifier<Transcript>,
    batch: musig::BatchVerifier<rand::rngs::ThreadRng>,
}

impl Verifier {
    /// Constructs a fresh verifier. The transcript label matches the
    /// prover's exactly — divergence here would silently invalidate
    /// every proof. Reuses [`shared_bp_gens`] for the generators.
    pub fn new() -> Self {
        let cs = r1cs::Verifier::new(Transcript::new(b"flamevm.r1cs.v1"));
        Self {
            cs,
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
            .verify(proof, pc_gens, shared_bp_gens())
            .map_err(|_| VMError::InvalidR1CSProof)
    }

    /// Public entry point: runs `bytecode` through the VM in
    /// external context, computes `TxID::from_log(&txlog)`, binds
    /// it into the R1CS transcript under domain `b"flamevm.txid"`,
    /// then:
    /// 1. appends each `DeferredSig::Explicit` to the batch
    ///    verifier via `Signature::verify_batched`.
    /// 2. if any `DeferredSig::TxBound` items were recorded,
    ///    requires the caller to pass the aggregate multi-signature
    ///    in `txbound_signature`, builds the `flamevm.signtx.v1`
    ///    transcript, binds it to TxID, and adds the multi-message
    ///    verification to the batch via
    ///    `Multisignature::verify_multi_batched`.
    /// 3. Verifies the R1CS proof.
    /// 4. Drains the batch.
    ///
    /// Returns the [`TxResult`] with `proof = None` — the proof
    /// has been verified by this point, so the caller doesn't need
    /// to handle it. `result.txid`, `result.txlog`, and
    /// `result.deferred_sigs` are populated for downstream
    /// inspection.
    ///
    /// `txbound_signature` is `None` for transactions without TxBound
    /// items (e.g. pure cell-open transactions). It is `Some(sig)` for
    /// transactions that emitted at least one `signtx`; passing
    /// `None` while TxBound items are present errors
    /// `MissingTxBoundSignature`.
    pub fn verify(
        pc_gens: &PedersenGens,
        bytecode: Vec<u8>,
        proof: &R1CSProof,
        header: TxHeader,
        gas_limit: u64,
        mem_limit: u64,
        txbound_signature: Option<musig::Signature>,
    ) -> Result<TxResult, VMError> {
        let mut verifier = Verifier::new();
        // Verifier-side: parse the wire bytecode into the canonical
        // witness-free Program. Both sides feed `VM::run` through
        // the same shape.
        let program = crate::program::Program::parse(&bytecode)?;
        let result = VM::run(
            header,
            program,
            gas_limit,
            mem_limit,
            &mut verifier,
        )?;
        // Append each Explicit deferred sig to the batch.
        for sig in &result.deferred_sigs {
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
        // Bind the canonical TxID into the R1CS transcript so the
        // proof commits to the full transaction (header + log), not
        // just the constraint system shape. Must mirror the prover
        // step exactly — divergence silently invalidates every
        // proof.
        let txid = result.txid;
        verifier
            .cs
            .transcript()
            .append_message(b"flamevm.txid", &txid.0);
        // Collect TxBound items and add the multi-message
        // verification to the batch. The transcript domain is
        // `flamevm.signtx.v1` bound to TxID — the prover must use
        // the same transcript when constructing the
        // multi-signature.
        let txbound_items: Vec<(musig::VerificationKey, [u8; 32])> = result
            .deferred_sigs
            .iter()
            .filter_map(|s| match s {
                DeferredSig::TxBound {
                    verification_key,
                    cell_id,
                } => Some((
                    musig::VerificationKey::from_compressed(*verification_key),
                    *cell_id,
                )),
                _ => None,
            })
            .collect();
        if !txbound_items.is_empty() {
            use musig::Multisignature;
            let signature = txbound_signature
                .ok_or(VMError::MissingTxBoundSignature)?;
            let mut t = merlin::Transcript::new(b"flamevm.signtx.v1");
            t.append_message(b"txid", &txid.0);
            signature.verify_multi_batched(&mut t, txbound_items, &mut verifier.batch);
        } else if txbound_signature.is_some() {
            // No TxBound items recorded but caller passed a signature:
            // that's a caller bug — the signature would never be
            // checked, so reject it explicitly to surface the
            // mismatch rather than silently accepting.
            return Err(VMError::SpuriousTxBoundSignature);
        }
        // Verify R1CS proof first, then drain the deferred-sig batch.
        // Both must pass for the tx to be valid. Destructure so both
        // consume-by-value methods work without borrow conflicts.
        let Verifier { cs, batch } = verifier;
        cs.verify(proof, pc_gens, shared_bp_gens())
            .map_err(|_| VMError::InvalidR1CSProof)?;
        batch
            .verify()
            .map_err(|_| VMError::BatchSignatureVerificationFailed)?;
        Ok(result)
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
        // [`VM::run`] returns; `finalize` here is a no-op
        // retained only so `Verifier` satisfies the `Delegate` trait
        // (mirrors the symmetric stub on the prover side).
        Ok(())
    }
}
