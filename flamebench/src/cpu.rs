//! The order worker threads, and the single-core runs, take CPUs in, and
//! the conditions a run is taken under.
//!
//! The order is computed from `/sys`: first hardware threads of fast cores,
//! then first hardware threads of compact cores, then second hardware
//! threads, fast cores first. A core is fast if its highest frequency is
//! within 5% of the machine's highest. Within a group CPUs go by number,
//! except that CPU 0 and its sibling go last: CPU 0 handles more interrupts.
//!
//! So thread count `n` runs on the first `n` CPUs of the order, and the
//! region of `n` is the kind of CPU the `n`-th one is.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Where Linux describes its CPUs.
pub const SYS_CPU: &str = "/sys/devices/system/cpu";

/// How close to the highest frequency a core must reach to count as fast.
const FAST_WITHIN: f64 = 0.05;

/// One logical CPU as `/sys` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalCpu {
    pub cpu: usize,
    pub package: usize,
    pub core: usize,
    /// Every logical CPU on this CPU's core, itself included.
    pub siblings: Vec<usize>,
    /// `cpuinfo_max_freq`, in kHz, when readable.
    pub max_freq_khz: Option<u64>,
}

/// Whether a core runs at the machine's top frequency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreClass {
    Fast,
    Compact,
}

/// Which kind of CPU a thread count adds last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    FastCores,
    CompactCores,
    SecondThreads,
}

impl Region {
    /// The region's name in tables and charts.
    pub fn label(self) -> &'static str {
        match self {
            Region::FastCores => "fast cores",
            Region::CompactCores => "compact cores",
            Region::SecondThreads => "second hardware threads",
        }
    }
}

/// One CPU of the order, with what placed it there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cpu {
    pub cpu: usize,
    pub package: usize,
    pub core: usize,
    pub class: CoreClass,
    /// Its position among its core's hardware threads: 0 for the first.
    pub thread: usize,
    pub max_freq_khz: Option<u64>,
}

impl Cpu {
    /// The region a thread count ending on this CPU belongs to.
    pub fn region(&self) -> Region {
        match (self.thread, self.class) {
            (0, CoreClass::Fast) => Region::FastCores,
            (0, CoreClass::Compact) => Region::CompactCores,
            _ => Region::SecondThreads,
        }
    }
}

/// Orders `cpus` as the module documentation describes.
pub fn order(cpus: &[LogicalCpu]) -> Vec<Cpu> {
    let top = cpus.iter().filter_map(|cpu| cpu.max_freq_khz).max();
    // A core's frequency is the highest any of its threads reports.
    let core_freq = |cpu: &LogicalCpu| {
        cpus.iter()
            .filter(|other| same_core(cpu, other))
            .filter_map(|other| other.max_freq_khz)
            .max()
    };
    let cpu0 = cpus.iter().find(|cpu| cpu.cpu == 0);

    let mut ordered: Vec<(bool, Cpu)> = cpus
        .iter()
        .map(|cpu| {
            let class = match (core_freq(cpu), top) {
                (Some(freq), Some(top)) if (freq as f64) < top as f64 * (1.0 - FAST_WITHIN) => {
                    CoreClass::Compact
                }
                _ => CoreClass::Fast,
            };
            let mut siblings = cpu.siblings.clone();
            siblings.sort_unstable();
            let thread = siblings
                .iter()
                .position(|sibling| *sibling == cpu.cpu)
                .unwrap_or(0);
            let on_cpu0_core = cpu0.is_some_and(|cpu0| same_core(cpu, cpu0));
            let entry = Cpu {
                cpu: cpu.cpu,
                package: cpu.package,
                core: cpu.core,
                class,
                thread,
                max_freq_khz: cpu.max_freq_khz,
            };
            (on_cpu0_core, entry)
        })
        .collect();
    ordered.sort_by_key(|(on_cpu0_core, cpu)| {
        (
            cpu.thread > 0,
            cpu.class,
            cpu.thread,
            *on_cpu0_core,
            cpu.cpu,
        )
    });
    ordered.into_iter().map(|(_, cpu)| cpu).collect()
}

fn same_core(a: &LogicalCpu, b: &LogicalCpu) -> bool {
    a.package == b.package && a.core == b.core
}

/// The region of each thread count: the kind of the `n`-th CPU in `order`.
pub fn region(order: &[Cpu], threads: usize) -> Option<Region> {
    threads
        .checked_sub(1)
        .and_then(|index| order.get(index))
        .map(Cpu::region)
}

/// Reads the online CPUs from `root`, normally [`SYS_CPU`].
pub fn read_topology(root: &Path) -> Result<Vec<LogicalCpu>, String> {
    let online = match fs::read_to_string(root.join("online")) {
        Ok(list) => parse_cpu_list(list.trim())?,
        Err(_) => {
            let mut found = BTreeSet::new();
            let entries = fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(n) = name.to_str().and_then(|n| n.strip_prefix("cpu")) {
                    if let Ok(cpu) = n.parse::<usize>() {
                        found.insert(cpu);
                    }
                }
            }
            found.into_iter().collect()
        }
    };
    if online.is_empty() {
        return Err(format!("{}: no online CPUs", root.display()));
    }
    online
        .into_iter()
        .map(|cpu| {
            let dir = root.join(format!("cpu{cpu}"));
            let read = |file: &str| {
                fs::read_to_string(dir.join(file))
                    .map(|text| text.trim().to_owned())
                    .map_err(|e| format!("{}: {e}", dir.join(file).display()))
            };
            let number = |file: &str| {
                read(file)?
                    .parse::<usize>()
                    .map_err(|e| format!("{}: {e}", dir.join(file).display()))
            };
            Ok(LogicalCpu {
                cpu,
                package: number("topology/physical_package_id").unwrap_or(0),
                core: number("topology/core_id")?,
                siblings: parse_cpu_list(&read("topology/thread_siblings_list")?)?,
                max_freq_khz: read("cpufreq/cpuinfo_max_freq")
                    .ok()
                    .and_then(|freq| freq.parse().ok()),
            })
        })
        .collect()
}

/// Parses a kernel CPU list such as `0-3,8,10-11`.
pub fn parse_cpu_list(list: &str) -> Result<Vec<usize>, String> {
    let mut cpus = Vec::new();
    for part in list.split(',').filter(|part| !part.is_empty()) {
        let parse = |n: &str| {
            n.trim()
                .parse::<usize>()
                .map_err(|_| format!("bad CPU list `{list}`"))
        };
        match part.split_once('-') {
            Some((first, last)) => cpus.extend(parse(first)?..=parse(last)?),
            None => cpus.push(parse(part)?),
        }
    }
    Ok(cpus)
}

/// The conditions a run was taken under, as far as they can be read
/// without privileges.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conditions {
    /// `scaling_governor`, or every distinct value joined by `/`.
    pub governor: Option<String>,
    /// `energy_performance_preference`, the same way.
    pub energy_performance_preference: Option<String>,
    /// The firmware's power profile, from [`PLATFORM_PROFILE`].
    pub platform_profile: Option<String>,
    /// `mains` or `battery`, when `/sys/class/power_supply` says.
    pub power_source: Option<String>,
}

/// Where the firmware reports its power profile: `low-power`, `balanced`,
/// `performance` and the like. Desktop power-profile daemons set it.
pub const PLATFORM_PROFILE: &str = "/sys/firmware/acpi/platform_profile";

/// Reads the governor, the platform profile and the power source of the
/// CPUs in `cpus`.
pub fn read_conditions(root: &Path, cpus: &[Cpu]) -> Conditions {
    let joined = |file: &str| {
        let values: BTreeSet<String> = cpus
            .iter()
            .filter_map(|cpu| {
                fs::read_to_string(root.join(format!("cpu{}/cpufreq/{file}", cpu.cpu))).ok()
            })
            .map(|value| value.trim().to_owned())
            .collect();
        (!values.is_empty()).then(|| values.into_iter().collect::<Vec<_>>().join("/"))
    };
    Conditions {
        governor: joined("scaling_governor"),
        energy_performance_preference: joined("energy_performance_preference"),
        platform_profile: fs::read_to_string(PLATFORM_PROFILE)
            .ok()
            .map(|profile| profile.trim().to_owned())
            .filter(|profile| !profile.is_empty()),
        power_source: read_power_source(Path::new("/sys/class/power_supply")),
    }
}

fn read_power_source(root: &Path) -> Option<String> {
    let mut mains = None;
    let mut discharging = false;
    for entry in fs::read_dir(root).ok()?.flatten() {
        let read = |file: &str| {
            fs::read_to_string(entry.path().join(file))
                .map(|text| text.trim().to_owned())
                .ok()
        };
        match read("type").as_deref() {
            Some("Mains") => {
                let online = read("online").as_deref() == Some("1");
                mains = Some(mains.unwrap_or(false) || online);
            }
            Some("Battery") => discharging |= read("status").as_deref() == Some("Discharging"),
            _ => {}
        }
    }
    match (mains, discharging) {
        (Some(true), _) => Some("mains".to_owned()),
        (_, true) | (Some(false), _) => Some("battery".to_owned()),
        (None, false) => None,
    }
}

/// The CPU model, from `/proc/cpuinfo`.
pub fn read_cpu_model() -> Option<String> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "model name").then(|| value.trim().to_owned())
    })
}

/// The CPU model as a file name part: lowercase, without the vendor or a
/// "w/ …" tail, every run of other characters one hyphen.
/// "AMD Ryzen AI 7 350 w/ Radeon 860M" becomes `ryzen-ai-7-350`.
pub fn cpu_slug(model: &str) -> String {
    const VENDORS: [&str; 5] = ["amd", "intel", "apple", "qualcomm", "arm"];
    let lower = model.to_lowercase();
    let lower = lower.split(" w/").next().unwrap_or_default();
    let lower = lower.replace("(r)", " ").replace("(tm)", " ");
    let words: Vec<&str> = lower.split_whitespace().collect();
    let words = match words.split_first() {
        Some((first, rest)) if VENDORS.contains(first) => rest,
        _ => &words[..],
    };
    let mut slug = String::new();
    for c in words.join(" ").chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "unknown-cpu".to_owned()
    } else {
        slug
    }
}
