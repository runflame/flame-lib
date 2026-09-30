//! Benchmark 0a, verifier scaling: how many transactions per second the
//! verifier handles on 1, 2, 4, 8 and 16 worker threads.
//!
//! It belongs to report 2, about the node, and is not published yet. The
//! `verifier_scaling` bench leaves its numbers in
//! `<target>/flamebench/verifier_scaling.json`, and
//! `report --verifier-scaling` prints them and draws its two charts next to
//! that file, never into `docs/`.

use serde::{Deserialize, Serialize};

use crate::cpu::{Cpu, Region};
use crate::results::{ratio, Machine};

/// The file the `verifier_scaling` bench writes.
pub const FILE: &str = "verifier_scaling.json";

/// Worker thread counts the bench measures, where the machine has that
/// many CPUs.
pub const THREADS: [usize; 5] = [1, 2, 4, 8, 16];

/// The thread counts this machine can run: every one of [`THREADS`] up to
/// its CPU count.
pub fn thread_counts(cpus: usize) -> Vec<usize> {
    THREADS
        .iter()
        .copied()
        .filter(|threads| *threads <= cpus)
        .collect()
}

/// One verifier-scaling run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scaling {
    /// The label of the shape every pool transaction has.
    pub shape: String,
    /// Transactions verified per iteration.
    pub pool: usize,
    /// The CPU order the workers take, and the conditions.
    pub machine: Machine,
    pub points: Vec<ScalingPoint>,
}

/// Throughput on `threads` workers, pinned to the first `threads` CPUs of
/// the order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingPoint {
    pub threads: usize,
    pub tps: f64,
}

impl Scaling {
    /// Throughput on one thread, the base of every speed-up.
    pub fn one_thread_tps(&self) -> Option<f64> {
        self.points
            .iter()
            .find(|point| point.threads == 1)
            .map(|point| point.tps)
    }

    /// Speed-up of `point` over one thread.
    pub fn speedup(&self, point: &ScalingPoint) -> f64 {
        self.one_thread_tps()
            .map_or(0.0, |base| ratio(point.tps, base))
    }

    /// Speed-up divided by the thread count.
    pub fn efficiency(&self, point: &ScalingPoint) -> f64 {
        ratio(self.speedup(point), point.threads as f64)
    }

    /// The region of `threads`: the kind of the last CPU it adds.
    pub fn region(&self, threads: usize) -> Option<Region> {
        crate::cpu::region(&self.machine.cpu_order, threads)
    }

    /// The CPUs `threads` workers run on.
    pub fn cpus_used(&self, threads: usize) -> &[Cpu] {
        let order = &self.machine.cpu_order;
        &order[..threads.min(order.len())]
    }

    /// Checks what the charts divide by and index into.
    pub fn validate(&self) -> Result<(), String> {
        if self.points.is_empty() {
            return Err("the scaling results have no points".into());
        }
        if self.one_thread_tps().is_none() {
            return Err("the scaling results have no one-thread point".into());
        }
        if self.machine.cpu_order.is_empty() {
            return Err("the scaling results hold no CPU order".into());
        }
        if self.points.iter().any(|point| point.threads == 0) {
            return Err("a scaling point has no threads".into());
        }
        if self
            .points
            .iter()
            .any(|point| !point.tps.is_finite() || point.tps < 0.0)
        {
            return Err("a scaling point has a negative or non-finite rate".into());
        }
        Ok(())
    }
}
