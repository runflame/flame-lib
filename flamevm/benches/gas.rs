#[macro_use]
extern crate criterion;

use bulletproofs::r1cs::{ConstraintSystem, Prover as R1csProver, Verifier as R1csVerifier};
use bulletproofs::{BulletproofGens, PedersenGens};
use criterion::{black_box, Criterion};
use curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::VartimeMultiscalarMul;
use flamevm::{MultiscalarMul, Point};
use merlin::Transcript;
use musig::{BatchVerifier, Signature, VerificationKey};
use sha2::Digest;
use std::time::Duration;

fn hashes(c: &mut Criterion) {
    for &n in &[0usize, 32, 1_024, 65_536] {
        let bytes = vec![0x5a; n];
        let sha256_bytes = bytes.clone();
        let sha512_bytes = bytes.clone();
        let sha3_bytes = bytes.clone();
        c.bench_function(&format!("gas/hash/sha256/{}", n), move |b| {
            b.iter(|| sha2::Sha256::digest(black_box(&sha256_bytes)))
        });
        c.bench_function(&format!("gas/hash/sha512/{}", n), move |b| {
            b.iter(|| sha2::Sha512::digest(black_box(&sha512_bytes)))
        });
        c.bench_function(&format!("gas/hash/sha3_256/{}", n), move |b| {
            b.iter(|| sha3::Sha3_256::digest(black_box(&sha3_bytes)))
        });
        c.bench_function(&format!("gas/hash/keccak256/{}", n), move |b| {
            b.iter(|| sha3::Keccak256::digest(black_box(&bytes)))
        });
    }
}

fn point_decompression(c: &mut Criterion) {
    let valid = RISTRETTO_BASEPOINT_POINT.compress();
    let invalid = CompressedRistretto([0xff; 32]);
    c.bench_function("gas/point/decompress_valid", move |b| {
        b.iter(|| black_box(valid).decompress())
    });
    c.bench_function("gas/point/decompress_invalid", move |b| {
        b.iter(|| black_box(invalid).decompress())
    });
}

fn signatures(c: &mut Criterion) {
    let secret = Scalar::from(7u64);
    let key = VerificationKey::from_secret(&secret);
    let signature = Signature::sign_message(b"gas.bench", b"message", secret);
    c.bench_function("gas/signature/immediate", move |b| {
        b.iter(|| {
            signature
                .verify_message(b"gas.bench", black_box(b"message"), key)
                .unwrap()
        })
    });

    for &n in &[1usize, 8, 64, 256] {
        c.bench_function(&format!("gas/signature/batch/{}", n), move |b| {
            b.iter(|| {
                let mut batch = BatchVerifier::with_capacity(rand::thread_rng(), n);
                for _ in 0..n {
                    signature.verify_message_batched(b"gas.bench", b"message", key, &mut batch);
                }
                batch.verify().unwrap()
            })
        });
    }
}

fn make_r1cs_proof(n: usize) -> (bulletproofs::r1cs::R1CSProof, BulletproofGens) {
    let pc_gens = PedersenGens::default();
    let bp_gens = BulletproofGens::new(n.next_power_of_two().max(1), 1);
    let mut prover = R1csProver::new(&pc_gens, Transcript::new(b"flamevm.gas.bench"));
    for _ in 0..n {
        let (l, r, o) = prover
            .allocate_multiplier(Some((Scalar::ONE, Scalar::ONE)))
            .unwrap();
        prover.constrain(l - Scalar::ONE);
        prover.constrain(r - Scalar::ONE);
        prover.constrain(o - Scalar::ONE);
    }
    (prover.prove(&bp_gens).unwrap(), bp_gens)
}

fn r1cs_verification(c: &mut Criterion) {
    for &n in &[1usize, 8, 64, 512] {
        let pc_gens = PedersenGens::default();
        let (proof, bp_gens) = make_r1cs_proof(n);
        c.bench_function(&format!("gas/r1cs/verify/{}", n), move |b| {
            b.iter(|| {
                let mut verifier = R1csVerifier::new(Transcript::new(b"flamevm.gas.bench"));
                for _ in 0..n {
                    let (l, r, o) = verifier.allocate_multiplier(None).unwrap();
                    verifier.constrain(l - Scalar::ONE);
                    verifier.constrain(r - Scalar::ONE);
                    verifier.constrain(o - Scalar::ONE);
                }
                verifier.verify(&proof, &pc_gens, &bp_gens).unwrap()
            })
        });
    }
}

fn msm(c: &mut Criterion) {
    let point = Point::from_compressed(RISTRETTO_BASEPOINT_POINT.compress());
    for &n in &[1usize, 8, 64, 256, 1_024] {
        let grow_point = point.clone();
        c.bench_function(&format!("gas/msm/grow/{}", n), move |b| {
            b.iter(|| {
                let mut msm = MultiscalarMul::from_point(&grow_point);
                for _ in 1..n {
                    msm = msm.push_point(&grow_point);
                }
                black_box(msm)
            })
        });

        let scalars = vec![Scalar::ONE; n];
        let points = vec![RISTRETTO_BASEPOINT_POINT; n];
        c.bench_function(&format!("gas/msm/finalize/{}", n), move |b| {
            b.iter(|| {
                black_box(RistrettoPoint::vartime_multiscalar_mul(
                    black_box(&scalars),
                    black_box(&points),
                ))
            })
        });
    }
}

criterion_group! {
    name = gas;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(1));
    targets = hashes, point_decompression, signatures, r1cs_verification, msm
}
criterion_main!(gas);
