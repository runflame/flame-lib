//! Benchmark 0a, verifier scaling: the verifier-scaling pool on 1, 2, 4, 8
//! and 16 worker threads, worker `i` pinned to the `i`-th CPU of the order.
//!
//! It belongs to report 2, about the node, and is not published yet. It
//! runs unpinned, because it pins its own workers, and leaves its numbers
//! in `<target>/flamebench/verifier_scaling.json`, never in a results file:
//!
//! ```text
//! cargo bench -p flamebench --bench verifier_scaling
//! cargo run -p flamebench --bin report -- --verifier-scaling
//! ```

use std::hint::black_box;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use core_affinity::CoreId;
use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput};
use flamebench::cpu::Cpu;
use flamebench::estimates::Estimates;
use flamebench::fixtures::{self, Pool};
use flamebench::scaling::{self, Scaling, ScalingPoint};
use flamebench::{paths, VERIFIER_SCALING};
use flamevm::ExternalTx;

fn main() {
    let machine = flamebench::machine().expect("read the CPU order from /sys");
    let mut criterion = Criterion::default().configure_from_args();
    throughput(&mut criterion, &machine.cpu_order);
    criterion.final_summary();
    if !flamebench::full_run() {
        eprintln!(
            "verifier_scaling: not the full run, so {} is left as it was",
            scaling::FILE
        );
        return;
    }

    // What criterion measured for each thread count in this run.
    let Ok(estimates) = Estimates::read(&paths::criterion_dir(), VERIFIER_SCALING) else {
        return;
    };
    let points: Vec<ScalingPoint> = scaling::thread_counts(machine.cpu_order.len())
        .into_iter()
        .filter_map(|threads| {
            let ns = estimates.median("throughput", &threads.to_string()).ok()?;
            Some(ScalingPoint {
                threads,
                tps: fixtures::POOL_SIZE as f64 * 1e9 / ns,
            })
        })
        .collect();
    if points.is_empty() {
        return;
    }
    let file = paths::scratch_dir().join(scaling::FILE);
    flamebench::write_json(
        &file,
        &Scaling {
            shape: fixtures::SHAPES[0].label.into(),
            pool: fixtures::POOL_SIZE,
            machine,
            points,
        },
    )
    .expect("write verifier_scaling.json");
    println!("wrote {}", file.display());
}

/// The whole pool on `threads` workers. One iteration verifies every
/// transaction once, so it takes about a second on one thread: few, flat
/// samples.
fn throughput(c: &mut Criterion, order: &[Cpu]) {
    // Built on first use, so a run that measures nothing does not prove it.
    let pool: OnceLock<Pool> = OnceLock::new();
    let pool = || {
        pool.get_or_init(|| {
            let pool = fixtures::pool();
            for tx in &pool.txs {
                assert!(tx.verify(fixtures::limits()).is_ok());
            }
            pool
        })
    };

    let mut group = c.benchmark_group(VERIFIER_SCALING);
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(10)
        .measurement_time(Duration::from_secs(15))
        .throughput(Throughput::Elements(fixtures::POOL_SIZE as u64));

    for threads in scaling::thread_counts(order.len()) {
        let cores: Vec<CoreId> = order[..threads]
            .iter()
            .map(|cpu| CoreId { id: cpu.cpu })
            .collect();
        group.bench_with_input(
            BenchmarkId::new("throughput", threads),
            &cores,
            |b, cores| {
                let pool = pool();
                b.iter(|| verify_pool(&pool.txs, cores))
            },
        );
    }

    group.finish();
}

/// Splits `txs` into one equal chunk per core and verifies each chunk on a
/// worker pinned to its core.
fn verify_pool(txs: &[ExternalTx], cores: &[CoreId]) {
    let chunk = txs.len().div_ceil(cores.len());
    thread::scope(|scope| {
        for (chunk, core) in txs.chunks(chunk).zip(cores) {
            scope.spawn(move || {
                assert!(core_affinity::set_for_current(*core), "pin to {core:?}");
                for tx in chunk {
                    black_box(tx.verify(fixtures::limits()).is_ok());
                }
            });
        }
    });
}
