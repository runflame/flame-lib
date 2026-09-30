//! Tests: fast, no timing. Only `shapes()` is ever built, never `pool()`,
//! and it is built once per test binary.

mod alloc;
mod cpu;
mod report;
mod shapes;
mod svg;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::cpu::{order, LogicalCpu};
use crate::fixtures::DecodeMethod;
use crate::report::commands;
use crate::results::{
    BenchTime, CapacityAttempt, ChainLimits, Estimate, FreshProcess, Machine, Results, Run,
    SendResult, ShapeResult, UtreexoTime, SCHEMA,
};
use crate::scaling::{Scaling, ScalingPoint};

/// A directory under the system temp dir, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(name: &str) -> TempDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "flamebench-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create a temp dir");
        TempDir(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A logical CPU on core `core` with the given siblings and frequency.
pub(crate) fn logical(cpu: usize, core: usize, siblings: &[usize], mhz: u64) -> LogicalCpu {
    LogicalCpu {
        cpu,
        package: 0,
        core,
        siblings: siblings.to_vec(),
        max_freq_khz: Some(mhz * 1000),
    }
}

/// The dev machine: CPUs 0, 2, 4, 6 fast, 1, 3, 5, 7 compact, and CPU
/// `n + 8` the second hardware thread of CPU `n`.
pub(crate) fn this_machine() -> Vec<LogicalCpu> {
    (0..16)
        .map(|cpu| {
            let core = cpu % 8;
            let mhz = match core {
                0 | 2 | 4 | 6 => 5091,
                7 => 3351,
                _ => 3506,
            };
            logical(cpu, core, &[core, core + 8], mhz)
        })
        .collect()
}

/// The dev machine's CPU order and conditions, as a bench records them.
pub(crate) fn sample_cpus() -> Machine {
    Machine {
        cpu_model: "AMD Ryzen AI 7 350 w/ Radeon 860M".into(),
        cpu_order: order(&this_machine()),
        governor: Some("powersave".into()),
        energy_performance_preference: Some("balance_performance".into()),
        platform_profile: Some("balanced".into()),
        power_source: Some("mains".into()),
    }
}

/// A hand-written results file with made-up but plausible numbers. The
/// shapes' gas and gates are the first run's.
pub(crate) fn sample_results() -> Results {
    let shape =
        |id: &str, label: &str, inputs, outputs, bytes, gas, mult, verify: f64, build: f64| {
            ShapeResult {
                id: id.into(),
                label: label.into(),
                inputs,
                outputs,
                fee: 1_000,
                bytes,
                gas_used: gas,
                multiplications: mult,
                decode_method: DecodeMethod::BlockTx,
                verify_ns: verify,
                decode_ns: 61_000.0 + 9_000.0 * outputs as f64,
                signature_ns: 38_000.0 + 12_000.0 * inputs as f64,
                r1cs_estimate_ns: verify * 0.8,
                utreexo: vec![
                    UtreexoTime {
                        leaves: 1 << 10,
                        depth: 10,
                        ns: 9_100.0,
                    },
                    UtreexoTime {
                        leaves: 1 << 16,
                        depth: 16,
                        ns: 14_600.0,
                    },
                ],
                send: SendResult {
                    prepare_ns: 90_000.0 * inputs as f64,
                    build_ns: build,
                    sign_ns: 110_000.0 + 20_000.0 * inputs as f64,
                    package_ns: 40_000.0 + 5_000.0 * outputs as f64,
                    open_note_ns: 150_000.0,
                    prove_estimate_ns: build * 0.85,
                    warm_peak_bytes: 400_000 + 250_000 * outputs as u64,
                    fresh: FreshProcess {
                        runs: 15,
                        first_ns: build * 1.1 + 31_700_000.0,
                        second_ns: build * 1.1,
                        first_peak_bytes: 900_000 + 250_000 * outputs as u64,
                    },
                },
            }
        };
    let cpus = sample_cpus();
    Results {
        schema: SCHEMA,
        run: Run {
            date: "2026-09-30".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            dirty: true,
            rustc: "rustc 1.90.0 (1159e78c4 2025-09-14)".into(),
            commands: commands(cpus.cpu_order.first()),
            durations: vec![
                BenchTime {
                    bench: "tx_cost".into(),
                    secs: 251.4,
                },
                BenchTime {
                    bench: "send".into(),
                    secs: 318.0,
                },
            ],
        },
        machine: cpus,
        shapes: vec![
            shape(
                "1to2",
                "1 → 2",
                1,
                2,
                1_921,
                6_238,
                137,
                2_222_000.0,
                14_400_000.0,
            ),
            shape(
                "2to2",
                "2 → 2",
                2,
                2,
                2_089,
                8_216,
                141,
                2_263_000.0,
                15_900_000.0,
            ),
            shape(
                "4to4",
                "4 → 4",
                4,
                4,
                2_885,
                17_932,
                281,
                3_940_000.0,
                27_800_000.0,
            ),
            shape(
                "1to13",
                "1 → 13",
                1,
                13,
                4_227,
                37_918,
                885,
                7_181_000.0,
                49_700_000.0,
            ),
        ],
        over_capacity: vec![CapacityAttempt {
            id: "1to14".into(),
            label: "1 → 14".into(),
            inputs: 1,
            outputs: 14,
            proves: false,
            error: Some("VMError::R1CSProofConstruction".into()),
            multiplications: None,
        }],
        limits: ChainLimits {
            max_multiplications: 100_000,
            max_multiplications_per_transaction: 1_024,
            max_transactions: 10_000,
        },
        estimates: vec![Estimate {
            id: "tx_cost/verify/1to2".into(),
            median_ns: 2_222_000.0,
            lower_ns: 2_210_000.0,
            upper_ns: 2_236_000.0,
        }],
    }
}

/// A hand-written verifier-scaling run on the dev machine.
pub(crate) fn sample_scaling() -> Scaling {
    Scaling {
        shape: "1 → 2".into(),
        pool: 256,
        machine: sample_cpus(),
        points: [
            (1, 405.0),
            (2, 790.0),
            (4, 1_530.0),
            (8, 2_380.0),
            (16, 3_010.0),
        ]
        .into_iter()
        .map(|(threads, tps)| ScalingPoint { threads, tps })
        .collect(),
    }
}
