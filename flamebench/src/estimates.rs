//! Reading criterion's results.
//!
//! Criterion 0.8 writes one directory per benchmark under its output
//! directory ([`crate::paths::criterion_dir`]), named after the group, the
//! function and the parameter. The latest run of each sits in `new/`:
//! `benchmark.json` names it and `estimates.json` holds its statistics.
//! The report takes the median's `point_estimate`, in nanoseconds per
//! iteration, and its confidence interval.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::results::Estimate as Recorded;

/// Every median criterion recorded for one group, by
/// `function/parameter`.
#[derive(Debug)]
pub struct Estimates {
    dir: PathBuf,
    group: String,
    medians: BTreeMap<String, Estimate>,
}

#[derive(Deserialize)]
struct BenchmarkFile {
    group_id: String,
    function_id: Option<String>,
    value_str: Option<String>,
}

#[derive(Deserialize)]
struct EstimatesFile {
    median: Estimate,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct Estimate {
    point_estimate: f64,
    confidence_interval: Interval,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct Interval {
    lower_bound: f64,
    upper_bound: f64,
}

impl Estimates {
    /// Reads every `new/benchmark.json` under `<dir>/<group>` and the
    /// `estimates.json` beside it.
    pub fn read(dir: &Path, group: &str) -> Result<Estimates, String> {
        let group_dir = dir.join(group);
        if !group_dir.is_dir() {
            return Err(format!(
                "no criterion results in {}: run `cargo bench -p flamebench --bench {group}` first",
                group_dir.display()
            ));
        }
        let mut medians = BTreeMap::new();
        let mut pending = vec![group_dir];
        while let Some(dir) = pending.pop() {
            let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for entry in entries {
                let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .file_name()
                    .is_some_and(|name| name == "benchmark.json")
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name == "new")
                {
                    let benchmark: BenchmarkFile = read_json(&path)?;
                    if benchmark.group_id != group {
                        continue;
                    }
                    let id = [benchmark.function_id, benchmark.value_str]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("/");
                    let estimates: EstimatesFile =
                        read_json(&path.with_file_name("estimates.json"))?;
                    medians.insert(id, estimates.median);
                }
            }
        }
        Ok(Estimates {
            dir: dir.to_path_buf(),
            group: group.to_owned(),
            medians,
        })
    }

    /// The median of `<group>/<function>/<parameter>`, in nanoseconds.
    pub fn median(&self, function: &str, parameter: &str) -> Result<f64, String> {
        self.recorded(function, parameter)
            .map(|estimate| estimate.median_ns)
    }

    /// The median of `<group>/<function>/<parameter>` with its confidence
    /// interval, in nanoseconds.
    pub fn recorded(&self, function: &str, parameter: &str) -> Result<Recorded, String> {
        let group = &self.group;
        let id = format!("{function}/{parameter}");
        match self.medians.get(&id) {
            Some(estimate)
                if estimate.point_estimate.is_finite() && estimate.point_estimate > 0.0 =>
            {
                Ok(Recorded {
                    id: format!("{group}/{id}"),
                    median_ns: estimate.point_estimate,
                    lower_ns: estimate.confidence_interval.lower_bound,
                    upper_ns: estimate.confidence_interval.upper_bound,
                })
            }
            Some(estimate) => Err(format!(
                "criterion's median for {group}/{id} is {}",
                estimate.point_estimate
            )),
            None => Err(format!(
                "criterion has no estimate for {group}/{id} in {}: run the benchmarks \
                 under \"How to reproduce\" in docs/benchmarks/transactions.md",
                self.dir.display()
            )),
        }
    }
}

/// Reads and parses a JSON file, naming it in any error.
pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}
