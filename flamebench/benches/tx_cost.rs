//! Benchmark 1, verifying: what it costs one core to verify one
//! transaction of each shape, and its parts.
//!
//! Every group runs on one core, pinned from outside with `taskset` to the
//! first CPU of the order; the full run refuses to start otherwise, and
//! only the full run, with no filter or other option, leaves
//! `fixtures.json` for the report. `docs/benchmarks/transactions.md` has the
//! commands, and `cargo run -p flamebench --bin report -- --cpu-order`
//! prints them for the machine at hand.

use std::collections::BTreeSet;
use std::hint::black_box;
use std::time::Instant;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use flamebench::fixtures::{self, Shape};
use flamebench::synthetic::{Membership, SyntheticProof};
use flamebench::{paths, TX_COST, UTREEXO_DEPTHS};
use flamevm::ContractID;
use merlin::Transcript;
use musig::{Multisignature, Signature, VerificationKey};

fn tx_cost(c: &mut Criterion) {
    let started = Instant::now();
    let machine = flamebench::machine().expect("read the CPU order from /sys");
    // Only the documented run leaves fixtures.json for the report, and it
    // must run where the report says it did.
    let full_run = flamebench::full_run();
    if full_run {
        if let Err(error) = flamebench::this_pinned_cpu(&machine.cpu_order) {
            panic!("{error}");
        }
    }
    let shapes = fixtures::shapes();
    single_core(c, &shapes);
    if !full_run {
        eprintln!(
            "tx_cost: not the full run, so {} is left as it was",
            flamebench::FIXTURES_FILE
        );
        return;
    }
    flamebench::write_json(
        &paths::scratch_dir().join(flamebench::FIXTURES_FILE),
        &flamebench::fixtures_file(&shapes, machine, started.elapsed().as_secs_f64()),
    )
    .expect("write fixtures.json");
}

/// The groups that time one transaction, or one part of one, on one core.
/// Criterion's defaults throughout.
fn single_core(c: &mut Criterion, shapes: &[Shape]) {
    let limits = fixtures::limits();
    let mut group = c.benchmark_group(TX_COST);

    for shape in shapes {
        assert!(shape.tx.verify_with_metrics(limits).is_ok());
        group.bench_with_input(
            BenchmarkId::new("verify", shape.spec.id),
            &shape.tx,
            |b, tx| b.iter(|| black_box(tx.verify_with_metrics(black_box(limits)))),
        );
    }

    for shape in shapes {
        assert!(shape.decode_bytes().is_ok());
        group.bench_with_input(
            BenchmarkId::new("decode", shape.spec.id),
            &shape.bytes,
            |b, bytes| b.iter(|| black_box(fixtures::decode(black_box(bytes), shape.decode))),
        );
    }

    for shape in shapes {
        let signature = shape.tx.signature.expect("every shape is signed");
        let items = signature_items(shape);
        assert!(verify_signature(&signature, &shape.txid.0, items.clone()).is_ok());
        group.bench_with_input(
            BenchmarkId::new("signature", shape.spec.id),
            &items,
            |b, items| {
                b.iter(|| black_box(verify_signature(&signature, &shape.txid.0, items.clone())))
            },
        );
    }

    let multipliers: BTreeSet<usize> = shapes
        .iter()
        .map(|shape| shape.metrics.multiplications)
        .collect();
    for count in multipliers {
        let proof = SyntheticProof::new(count);
        assert!(proof.verify().is_ok());
        group.bench_with_input(
            BenchmarkId::new("r1cs_synthetic", count),
            &proof,
            |b, proof| b.iter(|| black_box(proof.verify())),
        );
    }

    for depth in UTREEXO_DEPTHS {
        let membership = Membership::new(depth);
        assert!(membership.verify().is_ok());
        group.bench_with_input(
            BenchmarkId::new("utreexo", membership.leaves()),
            &membership,
            |b, membership| b.iter(|| black_box(membership.verify())),
        );
    }

    group.finish();
}

/// The signature's messages as the verifier builds them.
fn signature_items(shape: &Shape) -> Vec<(VerificationKey, ContractID)> {
    shape
        .items
        .iter()
        .map(|(key, contract)| (VerificationKey::from_compressed(*key), *contract))
        .collect()
}

/// `flamepayments::sign`'s transcript, verified.
fn verify_signature(
    signature: &Signature,
    txid: &[u8; 32],
    items: Vec<(VerificationKey, ContractID)>,
) -> Result<(), musig::StarsigError> {
    let mut transcript = Transcript::new(b"flamevm.signtx");
    transcript.append_message(b"txid", txid);
    signature.verify_multi(&mut transcript, items)
}

criterion_group!(benches, tx_cost);
criterion_main!(benches);
