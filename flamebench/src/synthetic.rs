//! The measurements that stand in for work the benchmarked code does not
//! expose or does not do.
//!
//! - **R1CS verification.** The proof is verified inside
//!   `Verifier::verify_with_cells`, out of reach from outside `flamevm`. Its
//!   share is estimated by verifying a synthetic proof with the same number
//!   of multipliers, built the way `flamevm/benches/gas.rs` builds one, with
//!   the generators the verifier uses. Its constraints are trivial and it
//!   commits no variables, so it was expected to be a lower bound; a
//!   profile of the real verification is the check.
//! - **R1CS proving.** The same holds on the sender's side: `build_transfer`
//!   runs the VM and proves in one call. The proving share is estimated by
//!   proving the same synthetic constraint system, split into
//!   [`constrain`], which is setup, and [`prove`], which is timed.
//! - **Utreexo.** Membership is checked by the chain and the mempool, per
//!   input, not by `verify`. One check costs one hash per level of the path,
//!   so it is measured in a small and a mid-size forest and scales by depth.

use std::sync::OnceLock;

use bulletproofs::r1cs::{ConstraintSystem, Prover, R1CSError, R1CSProof, Verifier};
use bulletproofs::{BulletproofGens, PedersenGens};
use curve25519_dalek::scalar::Scalar;
use flamechain::utreexo::{Forest, Hasher, Proof, UtreexoError};
use flamechain::{utreexo_hasher, ContractLeaf};
use merlin::Transcript;

/// The label both sides of the synthetic proof use.
const LABEL: &[u8] = b"flamebench.r1cs";

/// The generators `flamevm`'s prover and verifier use, 1,024 multipliers
/// for one party, built once per process as `flamevm` builds its own.
pub fn bulletproof_gens() -> &'static BulletproofGens {
    static BP_GENS: OnceLock<BulletproofGens> = OnceLock::new();
    BP_GENS.get_or_init(|| BulletproofGens::new(1024, 1))
}

/// A prover holding `multipliers` multipliers, each constrained to
/// `1 · 1 = 1`: the setup [`prove`] consumes.
pub fn constrain(pc_gens: &PedersenGens, multipliers: usize) -> Prover<'_, Transcript> {
    let mut prover = Prover::new(pc_gens, Transcript::new(LABEL));
    for _ in 0..multipliers {
        let (l, r, o) = prover
            .allocate_multiplier(Some((Scalar::ONE, Scalar::ONE)))
            .expect("a prover multiplier with an assignment");
        prover.constrain(l - Scalar::ONE);
        prover.constrain(r - Scalar::ONE);
        prover.constrain(o - Scalar::ONE);
    }
    prover
}

/// Proves a constrained prover with the shared generators: what one
/// iteration of `send/r1cs_prove_synthetic` does.
pub fn prove(prover: Prover<'_, Transcript>) -> Result<R1CSProof, R1CSError> {
    prover.prove(bulletproof_gens())
}

/// A synthetic R1CS proof of `multipliers` multipliers, each constrained to
/// `1 · 1 = 1`.
pub struct SyntheticProof {
    multipliers: usize,
    proof: R1CSProof,
    pc_gens: PedersenGens,
}

impl SyntheticProof {
    /// Proves it. Slow; build it outside the timed loop.
    pub fn new(multipliers: usize) -> SyntheticProof {
        let pc_gens = PedersenGens::default();
        let proof = prove(constrain(&pc_gens, multipliers))
            .expect("the synthetic proof fits the generators");
        SyntheticProof {
            multipliers,
            proof,
            pc_gens,
        }
    }

    /// The multiplier count.
    pub fn multipliers(&self) -> usize {
        self.multipliers
    }

    /// Verifies it: what one iteration of `tx_cost/r1cs_synthetic` does.
    pub fn verify(&self) -> Result<(), R1CSError> {
        let mut verifier = Verifier::new(Transcript::new(LABEL));
        for _ in 0..self.multipliers {
            let (l, r, o) = verifier.allocate_multiplier(None)?;
            verifier.constrain(l - Scalar::ONE);
            verifier.constrain(r - Scalar::ONE);
            verifier.constrain(o - Scalar::ONE);
        }
        verifier.verify(&self.proof, &self.pc_gens, bulletproof_gens())
    }
}

/// A forest of `2^depth` contract leaves and the path of its first leaf.
pub struct Membership {
    forest: Forest,
    leaf: ContractLeaf,
    proof: Proof,
    hasher: Hasher<ContractLeaf>,
}

impl Membership {
    /// Builds the forest and takes its first leaf's path through
    /// `Catchup::update_proof`, as a wallet refreshes a proof.
    pub fn new(depth: u32) -> Membership {
        let hasher = utreexo_hasher::<ContractLeaf>();
        let mut work = Forest::new().work_forest();
        for index in 0..1u64 << depth {
            work.insert(&leaf(index), &hasher);
        }
        let (forest, catchup) = work.normalize(&hasher);
        let proof = catchup
            .update_proof(&leaf(0), Proof::Transient, &hasher)
            .expect("the catchup commits every inserted leaf");
        assert_eq!(
            proof.as_path().map(|path| path.neighbors.len()),
            Some(depth as usize),
            "a perfect forest of 2^{depth} leaves has paths of {depth} hashes"
        );
        Membership {
            forest,
            leaf: leaf(0),
            proof,
            hasher,
        }
    }

    /// Leaves in the forest.
    pub fn leaves(&self) -> u64 {
        self.forest.count()
    }

    /// Checks the path: what one iteration of `tx_cost/utreexo` does.
    pub fn verify(&self) -> Result<(), UtreexoError> {
        let path = self.proof.as_path().ok_or(UtreexoError::InvalidProof)?;
        self.forest.verify(&self.leaf, path, &self.hasher)
    }
}

fn leaf(index: u64) -> ContractLeaf {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&index.to_le_bytes());
    ContractLeaf(id)
}
