//! Flame benchmarks.
//!
//! Report 1, `docs/benchmarks/transactions.md`, measures one transaction on
//! its own, in FlameVM and the wallet library, with no node:
//!
//! - **Benchmark 1, verifying** (the `tx_cost` bench): what it costs one
//!   core to verify one transaction of each common shape, broken into
//!   parts, with its size and gas.
//! - **Benchmark 2, sending** (the `send` bench): what it costs a wallet to
//!   build and sign one, in a warm process and in a fresh one, and how much
//!   heap that takes.
//!
//! The `report` binary turns criterion's estimates and the benches' scratch
//! files into a results file under `docs/benchmarks/results/transactions/`,
//! and a results file into the charts and the generated blocks of the
//! report. See that page for how to reproduce a run.
//!
//! Benchmark 0a, verifier scaling (the `verifier_scaling` bench), belongs to
//! report 2, about the node, and is not published yet: see [`scaling`].

use std::fs;
use std::io;
use std::path::Path;

pub mod alloc;
pub mod cpu;
pub mod docs;
pub mod estimates;
pub mod first_send;
pub mod fixtures;
pub mod paths;
pub mod report;
pub mod results;
pub mod scaling;
pub mod svg;
pub mod synthetic;

#[cfg(test)]
mod tests;

use results::{ChainLimits, FixtureShape, FixturesFile, Machine};

/// The criterion group of benchmark 1.
pub const TX_COST: &str = "tx_cost";

/// The criterion group of benchmark 2.
pub const SEND: &str = "send";

/// The criterion group of benchmark 0a.
pub const VERIFIER_SCALING: &str = "verifier_scaling";

/// Utreexo forest depths: 2^10 and 2^16 leaves.
pub const UTREEXO_DEPTHS: [u32; 2] = [10, 16];

/// The scratch file `tx_cost` leaves: its fixtures and conditions.
pub const FIXTURES_FILE: &str = "fixtures.json";

/// The scratch file `send` leaves: what it measured outside criterion.
pub const SEND_FILE: &str = "send.json";

/// Fresh processes per shape for the first-send measurement.
pub const FIRST_SEND_RUNS: usize = 15;

/// The consensus limits the report needs.
pub fn chain_limits() -> ChainLimits {
    let limits = fixtures::chain_params().limits;
    ChainLimits {
        max_multiplications: limits.max_multiplications,
        max_multiplications_per_transaction: limits.max_multiplications_per_transaction,
        max_transactions: limits.max_transactions,
    }
}

/// What a benchmark records about one shape it built.
pub fn fixture_shape(shape: &fixtures::Shape) -> FixtureShape {
    FixtureShape {
        id: shape.spec.id.into(),
        label: shape.spec.label.into(),
        inputs: shape.spec.inputs,
        outputs: shape.spec.outputs,
        fee: shape.metrics.total_fee,
        bytes: shape.tx.encoded_size(),
        gas_used: shape.metrics.gas_used,
        multiplications: shape.metrics.multiplications,
        decode_method: shape.decode,
    }
}

/// What the `tx_cost` bench records about the shapes it built.
pub fn fixtures_file(
    shapes: &[fixtures::Shape],
    machine: Machine,
    elapsed_secs: f64,
) -> FixturesFile {
    FixturesFile {
        shapes: shapes.iter().map(fixture_shape).collect(),
        utreexo_leaves: UTREEXO_DEPTHS.iter().map(|depth| 1u64 << depth).collect(),
        limits: chain_limits(),
        machine,
        elapsed_secs,
    }
}

/// The live CPU order and conditions.
pub fn machine() -> Result<Machine, String> {
    let root = Path::new(cpu::SYS_CPU);
    let order = cpu::order(&cpu::read_topology(root)?);
    let conditions = cpu::read_conditions(root, &order);
    Ok(Machine {
        cpu_model: cpu::read_cpu_model().unwrap_or_else(|| "unknown CPU".into()),
        cpu_order: order,
        governor: conditions.governor,
        energy_performance_preference: conditions.energy_performance_preference,
        platform_profile: conditions.platform_profile,
        power_source: conditions.power_source,
    })
}

/// Whether this bench binary runs as the documented command: `cargo bench`
/// with no filter and no other option. Only such a run writes the scratch
/// files the report reads, and only it runs a bench's steps outside
/// criterion, so a filtered, listed, tested or profiled run never leaves a
/// partial picture behind.
pub fn full_run() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    full_run_args(&args)
}

/// [`full_run`] for these arguments, the program name left out.
pub fn full_run_args(args: &[String]) -> bool {
    args.len() == 1 && args[0] == "--bench"
}

/// The one CPU this process is pinned to, which must be the first of the
/// order: where every timing in report 1 is taken, and what its footer
/// says. `allowed` is the process's affinity.
pub fn pinned_cpu(allowed: &[usize], order: &[cpu::Cpu]) -> Result<usize, String> {
    let first = order
        .first()
        .ok_or_else(|| "no CPUs in the order".to_owned())?
        .cpu;
    match allowed {
        [cpu] if *cpu == first => Ok(first),
        _ => Err(format!(
            "the benchmarks run pinned to CPU {first}, the first in the order, and this \
             process may run on {allowed:?}: run it as `taskset -c {first} cargo bench …`, \
             as `cargo run -p flamebench --bin report -- --cpu-order` prints"
        )),
    }
}

/// [`pinned_cpu`] for this process.
pub fn this_pinned_cpu(order: &[cpu::Cpu]) -> Result<usize, String> {
    let allowed: Vec<usize> = core_affinity::get_core_ids()
        .ok_or_else(|| "cannot read this process's CPU affinity".to_owned())?
        .into_iter()
        .map(|core| core.id)
        .collect();
    pinned_cpu(&allowed, order)
}

/// Writes `value` as pretty JSON with a trailing newline.
pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(path, text)
}
