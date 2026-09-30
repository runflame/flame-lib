//! Benchmark 2, sending: what it costs a wallet to build and sign one
//! transaction of each shape, and how much heap that takes.
//!
//! Run it pinned from outside with `taskset`, on the first CPU of the
//! order, as `tx_cost` is; the full run refuses to start otherwise. It has
//! two parts.
//!
//! - **Criterion groups**, in a warm process: `send/prepare`, `send/build`,
//!   `send/sign`, `send/package` and `send/open_note` measure a send's
//!   steps; `send/r1cs_prove_synthetic` estimates the proving share.
//! - **Outside criterion**, in the full run only, with no filter or other
//!   option: the peak heap of one warm send per shape, the 1→14 transfer
//!   that must fail, and the first send in new processes:
//!   [`FIRST_SEND_RUNS`] runs of the `first-send` binary per shape, each
//!   pinned with `taskset` to the CPU this bench is pinned to.
//!
//! The full run leaves what it measured outside criterion in
//! `<target>/flamebench/send.json` for the report.

use std::collections::BTreeSet;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use bulletproofs::PedersenGens;
use criterion::{BatchSize, BenchmarkId, Criterion, SamplingMode};
use flamebench::alloc::Counting;
use flamebench::first_send::{Report, Scratch};
use flamebench::fixtures::{self, Shape, Transfer, SEND_RNG_SEED};
use flamebench::results::{CapacityAttempt, FreshProcess, SendFile, SendShape};
use flamebench::synthetic;
use flamebench::{paths, FIRST_SEND_RUNS, SEND, SEND_FILE};
use flamepayments::{open_note, BuilderError};
use flamevm::VMError;

#[global_allocator]
static ALLOC: Counting = Counting::new();

fn main() {
    let started = Instant::now();
    let machine = flamebench::machine().expect("read the CPU order from /sys");
    // Checked before anything runs, so a misplaced run fails at once rather
    // than after its criterion groups.
    let full_run = flamebench::full_run();
    let cpu = if full_run {
        match flamebench::this_pinned_cpu(&machine.cpu_order) {
            Ok(cpu) => Some(cpu),
            Err(error) => panic!("{error}"),
        }
    } else {
        None
    };
    let (shapes, over_capacity) = fixtures::shapes_and_over_capacity();

    let mut criterion = Criterion::default().configure_from_args();
    steps(&mut criterion, &shapes);
    proving(&mut criterion, &shapes);
    criterion.final_summary();
    let Some(cpu) = cpu else {
        eprintln!("send: not the full run, so {SEND_FILE} is left as it was");
        return;
    };

    let warm: Vec<(u64, usize)> = shapes
        .iter()
        .map(|shape| warm_peak(&shape.transfer))
        .collect();
    let bytes: Vec<usize> = warm.iter().map(|(_, bytes)| *bytes).collect();
    let fresh = fresh_processes(&shapes, &bytes, cpu);
    let shapes = shapes
        .iter()
        .zip(warm)
        .zip(fresh)
        .map(|((shape, (warm_peak_bytes, _)), fresh)| SendShape {
            fixture: flamebench::fixture_shape(shape),
            warm_peak_bytes,
            fresh,
        })
        .collect();
    let file = paths::scratch_dir().join(SEND_FILE);
    flamebench::write_json(
        &file,
        &SendFile {
            shapes,
            over_capacity: vec![attempt(&over_capacity)],
            machine,
            elapsed_secs: started.elapsed().as_secs_f64(),
        },
    )
    .expect("write send.json");
    println!("wrote {}", file.display());
}

/// The steps of a send that take microseconds: criterion's defaults.
/// `build` takes tens of milliseconds and runs in [`proving`].
fn steps(c: &mut Criterion, shapes: &[Shape]) {
    let mut group = c.benchmark_group(SEND);

    for shape in shapes {
        assert!(shape.transfer.prepare().is_ok());
        group.bench_with_input(
            BenchmarkId::new("prepare", shape.spec.id),
            &shape.transfer,
            |b, transfer| b.iter(|| black_box(transfer.prepare())),
        );
    }

    for shape in shapes {
        let transfer = &shape.transfer;
        let inputs = transfer.prepare().expect("prepare");
        let unsigned = transfer
            .build(
                &inputs,
                &transfer.output_specs(),
                &mut fixtures::rng(SEND_RNG_SEED),
            )
            .expect("build");
        let keys = transfer.keys();
        assert!(fixtures::signature(&unsigned, &keys).is_ok());
        group.bench_with_input(
            BenchmarkId::new("sign", shape.spec.id),
            &unsigned,
            |b, unsigned| b.iter(|| black_box(fixtures::signature(unsigned, &keys))),
        );
    }

    for shape in shapes {
        let inputs = shape.spec.inputs;
        assert!(fixtures::package(fixtures::copy_tx(&shape.tx), inputs).is_ok());
        group.bench_with_input(
            BenchmarkId::new("package", shape.spec.id),
            &shape.tx,
            |b, tx| {
                b.iter_batched(
                    || fixtures::copy_tx(tx),
                    |tx| fixtures::package(tx, inputs),
                    BatchSize::SmallInput,
                )
            },
        );
    }

    for shape in shapes {
        let log = shape.tx.verify(fixtures::limits()).expect("verify");
        let owner = shape.transfer.outputs[0].path.owner();
        let (contract, note) = fixtures::paid_output(&log, &owner);
        assert!(open_note(contract, note, &owner.address, &owner.view_key).is_ok());
        group.bench_with_input(
            BenchmarkId::new("open_note", shape.spec.id),
            &(contract, note),
            |b, (contract, note)| {
                b.iter(|| black_box(open_note(contract, *note, &owner.address, &owner.view_key)))
            },
        );
    }

    group.finish();
}

/// The steps that take tens of milliseconds: `build`, and the synthetic
/// prove that estimates its proving share. Flat sampling keeps the run
/// short.
fn proving(c: &mut Criterion, shapes: &[Shape]) {
    let mut group = c.benchmark_group(SEND);
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(50)
        .measurement_time(Duration::from_secs(10));

    for shape in shapes {
        let transfer = &shape.transfer;
        let inputs = transfer.prepare().expect("prepare");
        let outputs = transfer.output_specs();
        // A new `r` every iteration, the same ones every run.
        let mut rng = fixtures::rng(SEND_RNG_SEED);
        assert!(transfer.build(&inputs, &outputs, &mut rng).is_ok());
        group.bench_with_input(
            BenchmarkId::new("build", shape.spec.id),
            transfer,
            |b, transfer| b.iter(|| black_box(transfer.build(&inputs, &outputs, &mut rng))),
        );
    }

    let gates: BTreeSet<usize> = shapes
        .iter()
        .map(|shape| shape.metrics.multiplications)
        .collect();
    let pc_gens = PedersenGens::default();
    for count in gates {
        assert!(synthetic::prove(synthetic::constrain(&pc_gens, count)).is_ok());
        group.bench_with_input(
            BenchmarkId::new("r1cs_prove_synthetic", count),
            &count,
            |b, count| {
                b.iter_batched(
                    || synthetic::constrain(&pc_gens, *count),
                    synthetic::prove,
                    BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

/// The peak heap of one warm send, and the length of what it packaged:
/// the generator table was built long before, so it is not part of it.
fn warm_peak(transfer: &Transfer) -> (u64, usize) {
    let mut rng = fixtures::rng(SEND_RNG_SEED);
    ALLOC.start();
    let sent = transfer.send(&mut rng);
    let peak = ALLOC.stop();
    (peak, sent.expect("a warm send").len())
}

/// Tries the transfer past the capacity, and records what happened.
fn attempt(transfer: &Transfer) -> CapacityAttempt {
    let spec = fixtures::OVER_CAPACITY;
    let inputs = transfer.prepare().expect("prepare");
    let built = transfer.build(
        &inputs,
        &transfer.output_specs(),
        &mut fixtures::rng(SEND_RNG_SEED),
    );
    // It fails for the known reason, or it proves; anything else is a bug
    // to look at, not a result to publish.
    let (proves, error, multiplications) = match built {
        Ok(unsigned) => (true, None, Some(unsigned.metrics().multiplications)),
        Err(BuilderError::Vm(error @ VMError::R1CSProofConstruction)) => {
            (false, Some(format!("VMError::{error:?}")), None)
        }
        Err(error) => panic!("{} fails for an unexpected reason: {error:?}", spec.label),
    };
    CapacityAttempt {
        id: spec.id.into(),
        label: spec.label.into(),
        inputs: spec.inputs,
        outputs: spec.outputs,
        proves,
        error,
        multiplications,
    }
}

/// The first send in new processes: [`FIRST_SEND_RUNS`] runs of
/// `first-send` per shape, the shapes taking turns, each pinned to `cpu`.
/// Each must package `bytes` as long as a warm send did.
fn fresh_processes(shapes: &[Shape], bytes: &[usize], cpu: usize) -> Vec<FreshProcess> {
    let dir = paths::scratch_dir().join("first_send");
    let files: Vec<_> = shapes
        .iter()
        .map(|shape| {
            let file = dir.join(format!("{}.json", shape.spec.id));
            let scratch = Scratch::new(shape.spec.id, &shape.transfer).expect("scratch");
            flamebench::write_json(&file, &scratch).expect("write a first-send scratch file");
            file
        })
        .collect();
    let mut reports: Vec<Vec<Report>> = vec![Vec::new(); shapes.len()];
    for run in 0..FIRST_SEND_RUNS {
        for (index, file) in files.iter().enumerate() {
            let report = first_send(file, cpu);
            assert_eq!(
                report.bytes, bytes[index],
                "{}: a new process packages what a warm one does",
                shapes[index].spec.label
            );
            reports[index].push(report);
        }
        eprintln!("first-send: run {} of {FIRST_SEND_RUNS}", run + 1);
    }
    reports
        .iter()
        .map(|runs| FreshProcess {
            runs: runs.len(),
            first_ns: median(runs.iter().map(|report| report.first_ns as f64)),
            second_ns: median(runs.iter().map(|report| report.second_ns as f64)),
            first_peak_bytes: runs
                .iter()
                .map(|report| report.first_peak_bytes)
                .max()
                .unwrap_or(0),
        })
        .collect()
}

/// One run of `first-send` on `scratch`, pinned to `cpu`.
fn first_send(scratch: &Path, cpu: usize) -> Report {
    let output = Command::new("taskset")
        .arg("-c")
        .arg(cpu.to_string())
        .arg(env!("CARGO_BIN_EXE_first-send"))
        .arg(scratch)
        .output()
        .expect("run first-send under taskset");
    assert!(
        output.status.success(),
        "first-send failed on {}: {}",
        scratch.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().last().expect("first-send prints a report");
    serde_json::from_str(line).expect("first-send prints a JSON report")
}

/// The median of `values`.
fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.collect();
    values.sort_by(f64::total_cmp);
    match values.len() {
        0 => 0.0,
        n if n % 2 == 1 => values[n / 2],
        n => (values[n / 2 - 1] + values[n / 2]) / 2.0,
    }
}
