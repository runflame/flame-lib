//! The `report` binary's work, callable from tests.
//!
//! ```text
//! report --cpu-order         print the CPU order and the commands
//! report                     results from the latest run, then draw
//! report --results F         draw from a results file
//!        --out-dir DIR       charts under DIR/charts/transactions/,
//!                            results under DIR/results/transactions/;
//!                            default docs/benchmarks/
//!        --page FILE         report 1, default
//!                            docs/benchmarks/transactions.md
//! report --verifier-scaling  print verifier scaling and draw its charts
//!                            into <target>/flamebench/, never docs/
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cpu::{self, Cpu, Region};
use crate::docs::{self, Links};
use crate::estimates::{read_json, Estimates};
use crate::results::{
    BenchTime, FixturesFile, Machine, Results, Run, SendFile, SendResult, ShapeResult, UtreexoTime,
    SCHEMA,
};
use crate::scaling::Scaling;
use crate::svg::{self, format_time, signed_percent, thousands, whole, Chart, Theme};
use crate::{paths, FIXTURES_FILE, SEND, SEND_FILE, TX_COST};

/// What to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Print the CPU order and the commands.
    CpuOrder,
    /// Build a results file from the latest run, then draw.
    FromCriterion,
    /// Draw from this results file.
    FromResults(PathBuf),
    /// Print verifier scaling from this file and draw its charts.
    VerifierScaling(PathBuf),
}

/// Parsed command-line options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    pub out_dir: PathBuf,
    pub page: PathBuf,
}

/// The usage line.
pub const USAGE: &str = "usage: report [--cpu-order | --results FILE | --verifier-scaling] \
                         [--out-dir DIR] [--page FILE]";

impl Options {
    /// Parses the arguments after the program name.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Options, String> {
        let mut mode = Mode::FromCriterion;
        let mut out_dir = None;
        let mut page = paths::default_page();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .map(PathBuf::from)
                    .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
            };
            match arg.as_str() {
                "--cpu-order" => mode = Mode::CpuOrder,
                "--results" => mode = Mode::FromResults(value("--results")?),
                "--verifier-scaling" => {
                    mode = Mode::VerifierScaling(paths::scratch_dir().join(crate::scaling::FILE))
                }
                "--out-dir" => out_dir = Some(value("--out-dir")?),
                "--page" => page = value("--page")?,
                "-h" | "--help" => return Err(USAGE.into()),
                other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
            }
        }
        let out_dir = out_dir.unwrap_or_else(|| match mode {
            Mode::VerifierScaling(_) => paths::scratch_dir(),
            _ => paths::default_out_dir(),
        });
        Ok(Options {
            mode,
            out_dir,
            page,
        })
    }
}

/// Runs the report. Returns what it prints.
pub fn run(options: &Options) -> Result<String, String> {
    match &options.mode {
        Mode::CpuOrder => cpu_order(),
        Mode::FromCriterion => {
            from_criterion_and_draw(&paths::criterion_dir(), &paths::scratch_dir(), options)
        }
        Mode::FromResults(file) => {
            let results: Results = read_json(file)?;
            draw(&results, file, options)
        }
        Mode::VerifierScaling(file) => verifier_scaling(file, &options.out_dir),
    }
}

/// The documented commands that produce a run on a machine whose order
/// starts at `first`.
pub fn commands(first: Option<&Cpu>) -> Vec<String> {
    let taskset = first.map_or_else(String::new, |cpu| format!("taskset -c {} ", cpu.cpu));
    vec![
        "cargo run -p flamebench --bin report -- --cpu-order".into(),
        "cargo bench -p flamebench --no-run".into(),
        format!("{taskset}cargo bench -p flamebench --bench {TX_COST}"),
        format!("{taskset}cargo bench -p flamebench --bench {SEND}"),
        "cargo run -p flamebench --bin report".into(),
    ]
}

fn cpu_order() -> Result<String, String> {
    let cpus = crate::machine()?;
    let order = &cpus.cpu_order;
    let mut out = format!(
        "CPU order on {}: first hardware threads of fast cores, then of compact cores, \
         then second hardware threads.\n\n",
        cpus.cpu_model
    );
    out.push_str("threads  cpu  core  thread  class    max MHz  region\n");
    for (index, cpu) in order.iter().enumerate() {
        out.push_str(&format!(
            "{:>7}  {:>3}  {:>4}  {:<6}  {:<7}  {:>7}  {}\n",
            index + 1,
            cpu.cpu,
            cpu.core,
            if cpu.thread == 0 { "first" } else { "second" },
            match cpu.class {
                cpu::CoreClass::Fast => "fast",
                cpu::CoreClass::Compact => "compact",
            },
            cpu.max_freq_khz
                .map_or_else(|| "-".into(), |khz| (khz / 1000).to_string()),
            cpu.region().label(),
        ));
    }
    out.push_str(&format!(
        "\nGovernor: {}, energy preference {}. Platform profile: {}. Power: {}.\n",
        cpus.governor.as_deref().unwrap_or("not readable"),
        cpus.energy_performance_preference
            .as_deref()
            .unwrap_or("not readable"),
        cpus.platform_profile.as_deref().unwrap_or("not readable"),
        cpus.power_source.as_deref().unwrap_or("not readable"),
    ));
    let first = order
        .first()
        .ok_or_else(|| "no CPUs in the order".to_owned())?;
    out.push_str(&format!(
        "\nThe benchmarks run on CPU {}, the first in the order:\n\n",
        first.cpu
    ));
    for command in commands(Some(first)) {
        out.push_str(&format!("  {command}\n"));
    }
    Ok(out)
}

/// Builds a results file from criterion's estimates and the benches'
/// scratch files.
pub fn from_criterion(criterion_dir: &Path, scratch: &Path) -> Result<Results, String> {
    let fixtures: FixturesFile = read_scratch(scratch, FIXTURES_FILE, TX_COST)?;
    let send: SendFile = read_scratch(scratch, SEND_FILE, SEND)?;
    same_conditions(&fixtures.machine, &send.machine)?;
    let sent: Vec<_> = fixtures
        .shapes
        .iter()
        .map(|shape| {
            let sent = send
                .shapes
                .iter()
                .find(|sent| sent.fixture.id == shape.id)
                .ok_or_else(|| format!("{SEND_FILE} has no shape `{}`", shape.id))?;
            if sent.fixture != *shape {
                return Err(format!(
                    "the {} shape differs between {FIXTURES_FILE} and {SEND_FILE}: the fixtures \
                     are no longer the same bytes; re-run both benches",
                    shape.label
                ));
            }
            Ok(sent)
        })
        .collect::<Result<_, String>>()?;
    let tx_cost = Estimates::read(criterion_dir, TX_COST)?;
    let send_estimates = Estimates::read(criterion_dir, SEND)?;

    let mut recorded = Vec::new();
    let mut take = |estimates: &Estimates, function: &str, parameter: &str| {
        let estimate = estimates.recorded(function, parameter)?;
        let median = estimate.median_ns;
        if !recorded.contains(&estimate) {
            recorded.push(estimate);
        }
        Ok::<f64, String>(median)
    };

    let mut shapes = Vec::with_capacity(fixtures.shapes.len());
    for (shape, sent) in fixtures.shapes.iter().zip(sent) {
        let utreexo = fixtures
            .utreexo_leaves
            .iter()
            .map(|leaves| {
                Ok(UtreexoTime {
                    leaves: *leaves,
                    depth: leaves.trailing_zeros(),
                    ns: take(&tx_cost, "utreexo", &leaves.to_string())?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let gates = shape.multiplications.to_string();
        shapes.push(ShapeResult {
            id: shape.id.clone(),
            label: shape.label.clone(),
            inputs: shape.inputs,
            outputs: shape.outputs,
            fee: shape.fee,
            bytes: shape.bytes,
            gas_used: shape.gas_used,
            multiplications: shape.multiplications,
            decode_method: shape.decode_method,
            verify_ns: take(&tx_cost, "verify", &shape.id)?,
            decode_ns: take(&tx_cost, "decode", &shape.id)?,
            signature_ns: take(&tx_cost, "signature", &shape.id)?,
            r1cs_estimate_ns: take(&tx_cost, "r1cs_synthetic", &gates)?,
            utreexo,
            send: SendResult {
                prepare_ns: take(&send_estimates, "prepare", &shape.id)?,
                build_ns: take(&send_estimates, "build", &shape.id)?,
                sign_ns: take(&send_estimates, "sign", &shape.id)?,
                package_ns: take(&send_estimates, "package", &shape.id)?,
                open_note_ns: take(&send_estimates, "open_note", &shape.id)?,
                prove_estimate_ns: take(&send_estimates, "r1cs_prove_synthetic", &gates)?,
                warm_peak_bytes: sent.warm_peak_bytes,
                fresh: sent.fresh.clone(),
            },
        });
    }

    let root = paths::workspace_root();
    let (commit, dirty) = git_head(&root);
    let machine = fixtures.machine;
    let results = Results {
        schema: SCHEMA,
        run: Run {
            date: today(),
            commit,
            dirty,
            rustc: rustc_version(&root),
            commands: commands(machine.cpu_order.first()),
            durations: vec![
                BenchTime {
                    bench: TX_COST.into(),
                    secs: fixtures.elapsed_secs,
                },
                BenchTime {
                    bench: SEND.into(),
                    secs: send.elapsed_secs,
                },
            ],
        },
        machine,
        shapes,
        over_capacity: send.over_capacity,
        limits: fixtures.limits,
        estimates: recorded,
    };
    results.validate()?;
    Ok(results)
}

/// Refuses a run whose two benches saw different machines or conditions.
fn same_conditions(tx_cost: &Machine, send: &Machine) -> Result<(), String> {
    let describe = |cpus: &Machine| {
        format!(
            "{} on CPU {}, governor {}, energy preference {}, platform profile {}, power {}",
            cpus.cpu_model,
            cpus.cpu_order
                .first()
                .map_or_else(|| "?".into(), |cpu| cpu.cpu.to_string()),
            cpus.governor.as_deref().unwrap_or("?"),
            cpus.energy_performance_preference.as_deref().unwrap_or("?"),
            cpus.platform_profile.as_deref().unwrap_or("?"),
            cpus.power_source.as_deref().unwrap_or("?"),
        )
    };
    if tx_cost == send {
        Ok(())
    } else {
        Err(format!(
            "{TX_COST} and {SEND} ran under different conditions ({TX_COST}: {}; {SEND}: {}): \
             re-run one of them so the report has one set",
            describe(tx_cost),
            describe(send)
        ))
    }
}

fn read_scratch<T: for<'de> serde::Deserialize<'de>>(
    dir: &Path,
    name: &str,
    bench: &str,
) -> Result<T, String> {
    let path = dir.join(name);
    if !path.is_file() {
        return Err(format!(
            "{} is missing: run `cargo bench -p flamebench --bench {bench}` first, it writes {name}",
            path.display()
        ));
    }
    read_json(&path)
}

/// Writes each chart's two files into `out_dir`.
fn write_charts(charts: &[Chart], out_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    for chart in charts {
        for theme in Theme::ALL {
            let path = out_dir.join(chart.file_name(theme));
            fs::write(&path, chart.render(theme))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// What drawing produces, before anything is written.
struct Drawn {
    charts: Vec<Chart>,
    page: String,
    page_dir: PathBuf,
}

/// Draws the charts and regenerates the page's blocks from `results`,
/// which were read from or will be written to `results_file`, without
/// writing anything: a page with a missing marker fails here.
fn render(results: &Results, results_file: &Path, options: &Options) -> Result<Drawn, String> {
    results
        .validate()
        .map_err(|e| format!("{}: {e}", results_file.display()))?;
    let charts = svg::charts(results);
    let page_dir = match options.page.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let charts_dir = paths::charts_dir(&options.out_dir);
    let blocks = docs::blocks(
        results,
        &charts,
        &Links {
            page_dir: &page_dir,
            charts_dir: &charts_dir,
            results_file,
        },
    );
    let page = if options.page.exists() {
        fs::read_to_string(&options.page).map_err(|e| format!("{}: {e}", options.page.display()))?
    } else {
        let name = results_file
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        docs::skeleton(&name)
    };
    let page =
        docs::regenerate(&page, &blocks).map_err(|e| format!("{}: {e}", options.page.display()))?;
    Ok(Drawn {
        charts,
        page,
        page_dir,
    })
}

/// Writes what [`render`] drew, and returns what the report prints.
fn write(results: &Results, drawn: Drawn, options: &Options) -> Result<String, String> {
    write_charts(&drawn.charts, &paths::charts_dir(&options.out_dir))?;
    fs::create_dir_all(&drawn.page_dir)
        .map_err(|e| format!("{}: {e}", drawn.page_dir.display()))?;
    fs::write(&options.page, drawn.page).map_err(|e| format!("{}: {e}", options.page.display()))?;
    let mut out = format!(
        "wrote {} charts to {} and the blocks of {}\n\n",
        drawn.charts.len() * Theme::ALL.len(),
        paths::charts_dir(&options.out_dir).display(),
        options.page.display()
    );
    out.push_str(&developer_table(results));
    Ok(out)
}

/// Draws the charts and regenerates the page's blocks from `results`,
/// read from `results_file`.
pub fn draw(results: &Results, results_file: &Path, options: &Options) -> Result<String, String> {
    let drawn = render(results, results_file, options)?;
    write(results, drawn, options)
}

/// `report` without `--results`: builds a results file from the latest run
/// in `criterion_dir` and `scratch`, then draws. Nothing is written, the
/// results file included, unless the page takes the blocks.
pub fn from_criterion_and_draw(
    criterion_dir: &Path,
    scratch: &Path,
    options: &Options,
) -> Result<String, String> {
    let results = from_criterion(criterion_dir, scratch)?;
    let file = paths::results_dir(&options.out_dir).join(results.file_name());
    let drawn = render(&results, &file, options)?;
    crate::write_json(&file, &results).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut out = format!("wrote {}\n", file.display());
    out.push_str(&write(&results, drawn, options)?);
    Ok(out)
}

/// Every number, for the terminal.
pub fn developer_table(results: &Results) -> String {
    let limits = &results.limits;
    let mut out = format!(
        "{:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>7} {:>5} {:>9} {:>7} {:>9}\n",
        "shape",
        "verify",
        "decode",
        "signature",
        "r1cs est",
        "vm+other",
        "utreexo",
        "bytes",
        "gates",
        "per sec",
        "tx/blk",
        "block"
    );
    for shape in &results.shapes {
        out.push_str(&format!(
            "{:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>7} {:>5} {:>9} {:>7} {:>9}{}\n",
            shape.label,
            format_time(shape.verify_ns),
            format_time(shape.decode_ns),
            format_time(shape.signature_ns),
            format_time(shape.r1cs_estimate_ns),
            format_time(shape.vm_and_other_ns()),
            format_time(shape.utreexo_ns()),
            shape.bytes,
            shape.multiplications,
            whole(shape.tps_per_core()),
            shape.per_block(limits),
            format!("{:.2} s", shape.full_block_ns(limits) / 1e9),
            if shape.estimate_overshot() {
                "  estimate overshot: run the perf cross-check"
            } else {
                ""
            },
        ));
    }
    out.push_str(&format!(
        "\n{:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>9} {:>9}\n",
        "shape",
        "prepare",
        "build",
        "prove est",
        "seal est",
        "prover run",
        "sign",
        "package",
        "warm",
        "new 1st",
        "new 2nd",
        "setup",
        "first",
        "warm peak",
        "1st peak"
    ));
    for shape in &results.shapes {
        let send = &shape.send;
        out.push_str(&format!(
            "{:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>9} {:>9}{}\n",
            shape.label,
            format_time(send.prepare_ns),
            format_time(send.build_ns),
            format_time(send.prove_estimate_ns),
            format_time(shape.sealing_estimate_ns()),
            format_time(shape.prover_run_ns()),
            format_time(send.sign_ns),
            format_time(send.package_ns),
            format_time(shape.warm_send_ns()),
            format_time(send.fresh.first_ns),
            format_time(send.fresh.second_ns),
            format_time(shape.generator_setup_ns()),
            format_time(shape.first_send_ns()),
            svg::megabytes(send.warm_peak_bytes),
            svg::megabytes(send.fresh.first_peak_bytes),
            if shape.send_estimate_overshot() {
                "  send estimates overshot build"
            } else {
                ""
            },
        ));
    }
    out.push_str(&format!(
        "\n{:<8} {:>7} {:>9} {:>9} {:>10} {:>10} {:>7} {:>10} {:>7}\n",
        "shape", "gates", "gas", "per gate", "measured", "gas today", "off", "per gate", "off"
    ));
    for shape in &results.shapes {
        out.push_str(&format!(
            "{:<8} {:>7} {:>9} {:>9} {:>10} {:>10} {:>7} {:>10} {:>7}\n",
            shape.label,
            shape.multiplications,
            thousands(shape.gas_used),
            thousands(shape.gas_per_gate()),
            format_time(shape.verify_ns),
            format_time(shape.gas_predicted_ns()),
            signed_percent(shape.gas_error(shape.gas_predicted_ns())),
            format_time(shape.gas_per_gate_predicted_ns()),
            signed_percent(shape.gas_error(shape.gas_per_gate_predicted_ns())),
        ));
    }
    for attempt in &results.over_capacity {
        out.push_str(&format!(
            "\n{}: {}{}\n",
            attempt.label,
            if attempt.proves {
                "proves".to_owned()
            } else {
                format!(
                    "does not prove ({})",
                    attempt.error.as_deref().unwrap_or("no error recorded")
                )
            },
            attempt.multiplications.map_or_else(
                || ", gates not observable".to_owned(),
                |gates| { format!(", {gates} gates") }
            ),
        ));
    }
    out
}

/// `report --verifier-scaling`: prints the table and draws the two charts
/// into `out_dir`, which may not be inside `docs/`.
pub fn verifier_scaling(file: &Path, out_dir: &Path) -> Result<String, String> {
    if paths::is_within(out_dir, &paths::docs_dir()) {
        return Err(format!(
            "{}: verifier scaling is not published yet, so its charts never go under docs/",
            out_dir.display()
        ));
    }
    if !file.is_file() {
        return Err(format!(
            "{} is missing: run `cargo bench -p flamebench --bench verifier_scaling` first",
            file.display()
        ));
    }
    let scaling: Scaling = read_json(file)?;
    scaling
        .validate()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let charts = svg::scaling_charts(&scaling);
    write_charts(&charts, out_dir)?;

    let mut out = format!(
        "Verifier scaling, {} ({} per iteration), on {}:\n\n",
        scaling.shape, scaling.pool, scaling.machine.cpu_model
    );
    out.push_str(&format!(
        "{:>7}  {:<48}  {:<24}  {:>7}  {:>8}  {:>10}\n",
        "threads", "CPUs", "region", "TPS", "speed-up", "efficiency"
    ));
    for point in &scaling.points {
        let cpus: Vec<String> = scaling
            .cpus_used(point.threads)
            .iter()
            .map(|cpu| cpu.cpu.to_string())
            .collect();
        out.push_str(&format!(
            "{:>7}  {:<48}  {:<24}  {:>7}  {:>7.2}x  {:>9.0}%\n",
            point.threads,
            cpus.join(", "),
            scaling.region(point.threads).map_or("", Region::label),
            whole(point.tps),
            scaling.speedup(point),
            scaling.efficiency(point) * 100.0,
        ));
    }
    out.push_str(&format!(
        "\nwrote {} charts to {}\n",
        charts.len() * Theme::ALL.len(),
        out_dir.display()
    ));
    Ok(out)
}

/// `HEAD`, and whether tracked files differ from it.
fn git_head(root: &Path) -> (String, bool) {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // What `report` itself rewrites, the page, the charts and the results
    // files, does not make a run dirty: only the code and the rest do.
    let dirty = git(&[
        "status",
        "--porcelain",
        "--untracked-files=no",
        "--",
        ".",
        ":(exclude)docs/benchmarks",
    ])
    .is_none_or(|status| !status.is_empty());
    (commit, dirty)
}

fn rustc_version(root: &Path) -> String {
    Command::new("rustc")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

/// Today's date, UTC, as `YYYY-MM-DD`.
fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (year, month, day) = civil_date((secs / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
pub fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
