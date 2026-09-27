//! The generated blocks of report 1, `docs/benchmarks/transactions.md`.
//!
//! The report owns what sits between `<!-- flamebench:begin NAME -->` and
//! `<!-- flamebench:end NAME -->`: every table, every `<picture>` embed, the
//! commands and the footer, regenerated on every run. Everything outside the
//! markers is prose written from the measured numbers, and the report never
//! touches it. A page that does not exist yet is created with its headings,
//! every marker pair, and empty places for prose.

use std::path::Path;

use crate::cpu::CoreClass;
use crate::fixtures::DecodeMethod;
use crate::paths::relative;
use crate::results::{short_commit, Results, GAS_R1CS_ITEM, NS_PER_GAS};
use crate::svg::{
    format_time, kilobytes, megabytes, signed_percent, thousands, whole, Chart, Theme,
};

/// The generated blocks, in page order.
pub const BLOCKS: [&str; 12] = [
    "key-numbers",
    "shapes",
    "cost-by-shape",
    "tps-per-core",
    "block-capacity",
    "send-latency",
    "send-breakdown",
    "proof-capacity",
    "peak-memory",
    "gas-check",
    "reproduce",
    "footer",
];

/// The line that opens block `name`.
pub fn begin_marker(name: &str) -> String {
    format!("<!-- flamebench:begin {name} -->")
}

/// The line that closes block `name`.
pub fn end_marker(name: &str) -> String {
    format!("<!-- flamebench:end {name} -->")
}

/// A new page, its prose to be written from the run in `results_file`: the
/// headings, every marker pair, and a comment where each piece of prose
/// goes.
pub fn skeleton(results_file: &str) -> String {
    let pair = |name: &str| format!("{}\n{}\n", begin_marker(name), end_marker(name));
    format!(
        "<!-- Generated blocks are rewritten by `report`. Prose outside them was written from \
         run {results_file}; re-check it after a re-run. -->\n\
         \n\
         # What a Flame transaction costs\n\
         \n\
         Report 1 of 2. Next: [the node](../benchmarks.md), planned.\n\
         \n\
         <!-- Prose: what is measured, for which transactions, and what is not. -->\n\
         \n\
         ## Key numbers\n\
         \n\
         {key}\n\
         ## Transaction shapes\n\
         \n\
         <!-- Prose: the shapes, and why. -->\n\
         \n\
         {shapes}\n\
         <!-- Prose: what a gate is. -->\n\
         \n\
         ## Verifying\n\
         \n\
         <!-- Prose: what verify includes and excludes; the estimated proof share. -->\n\
         \n\
         {cost}\n\
         {tps}\n\
         {capacity}\n\
         <!-- Prose: no per-second chain figures; takeaways from the numbers. -->\n\
         \n\
         ## Sending\n\
         \n\
         <!-- Prose: what a send includes and excludes; warm and first sends. -->\n\
         \n\
         {latency}\n\
         {breakdown}\n\
         {proof}\n\
         {memory}\n\
         <!-- Prose: memory, the capacity limit, and takeaways from the numbers. -->\n\
         \n\
         ## Size and gas\n\
         \n\
         <!-- Prose: what gas is, and what the bytes contain. -->\n\
         \n\
         {gas}\n\
         <!-- Prose: the finding. -->\n\
         \n\
         ## How it was measured\n\
         \n\
         <!-- Prose: seeds, criterion, estimates, fresh processes, the allocator. -->\n\
         \n\
         ## Limits\n\
         \n\
         <!-- Prose: one laptop core; no batch verification; no node work. -->\n\
         \n\
         ## How to reproduce\n\
         \n\
         {reproduce}\n\
         ## Machine\n\
         \n\
         {footer}",
        key = pair("key-numbers"),
        shapes = pair("shapes"),
        cost = pair("cost-by-shape"),
        tps = pair("tps-per-core"),
        capacity = pair("block-capacity"),
        latency = pair("send-latency"),
        breakdown = pair("send-breakdown"),
        proof = pair("proof-capacity"),
        memory = pair("peak-memory"),
        gas = pair("gas-check"),
        reproduce = pair("reproduce"),
        footer = pair("footer"),
    )
}

/// Replaces the content of every block in `page`, leaving every byte
/// outside the markers as it was. Fails if a marker is missing or
/// repeated.
pub fn regenerate(page: &str, blocks: &[(&str, String)]) -> Result<String, String> {
    let mut out = page.to_owned();
    for (name, content) in blocks {
        let begin = begin_marker(name);
        let end = end_marker(name);
        let start = unique(&out, &begin)?;
        let stop = unique(&out, &end)?;
        let inner = start + begin.len();
        if stop < inner {
            return Err(format!("`{end}` comes before `{begin}`"));
        }
        out.replace_range(inner..stop, &format!("\n\n{}\n\n", content.trim()));
    }
    Ok(out)
}

fn unique(page: &str, marker: &str) -> Result<usize, String> {
    let mut found = page.match_indices(marker).map(|(at, _)| at);
    match (found.next(), found.next()) {
        (Some(at), None) => Ok(at),
        (None, _) => Err(format!("the page has no `{marker}` marker")),
        (Some(_), Some(_)) => Err(format!("the page has `{marker}` more than once")),
    }
}

/// Where the page's links point.
pub struct Links<'a> {
    /// The directory the page is in.
    pub page_dir: &'a Path,
    /// Where the charts are.
    pub charts_dir: &'a Path,
    /// The results file the page is drawn from.
    pub results_file: &'a Path,
}

/// Every block's content, drawn from `results`.
pub fn blocks(results: &Results, charts: &[Chart], links: &Links) -> Vec<(&'static str, String)> {
    let picture = |name: &str| {
        let chart = charts
            .iter()
            .find(|chart| chart.name == name)
            .expect("every chart is drawn");
        let src = |theme| {
            relative(
                links.page_dir,
                &links.charts_dir.join(chart.file_name(theme)),
            )
        };
        format!(
            "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"{}\">\n  \
             <img alt=\"{}\" src=\"{}\" width=\"720\">\n</picture>",
            src(Theme::Dark),
            crate::svg::esc(&chart.alt),
            src(Theme::Light)
        )
    };
    let with_table = |name: &str, table: String| format!("{}\n\n{table}", picture(name));
    vec![
        ("key-numbers", key_numbers(results)),
        ("shapes", shapes_table(results)),
        (
            "cost-by-shape",
            with_table("cost-by-shape", cost_table(results)),
        ),
        ("tps-per-core", picture("tps-per-core")),
        ("block-capacity", block_capacity_table(results)),
        (
            "send-latency",
            with_table("send-latency", latency_table(results)),
        ),
        (
            "send-breakdown",
            with_table("send-breakdown", breakdown_table(results)),
        ),
        (
            "proof-capacity",
            with_table("proof-capacity", capacity_table(results)),
        ),
        (
            "peak-memory",
            with_table("peak-memory", memory_table(results)),
        ),
        ("gas-check", with_table("gas-check", gas_table(results))),
        ("reproduce", reproduce(results, links)),
        ("footer", footer(results, links)),
    ]
}

fn key_numbers(results: &Results) -> String {
    let payment = results.payment();
    let label = &payment.label;
    let rows = [
        (
            format!("Verify a {label} payment"),
            format_time(payment.verify_ns),
        ),
        (
            format!("One core's verification rate, {label}"),
            format!("~{} per second", whole(payment.tps_per_core())),
        ),
        (
            format!("Send a {label} payment, warm"),
            format_time(payment.warm_send_ns()),
        ),
        (
            format!("Send a {label} payment, first in a new process"),
            format_time(payment.first_send_ns()),
        ),
        (
            format!("Size of a {label} payment"),
            kilobytes(payment.bytes as u64),
        ),
        (
            "Most outputs in one transaction".to_owned(),
            results
                .most_outputs()
                .map_or_else(|| "none".into(), |outputs| outputs.to_string()),
        ),
        (
            format!("{label} payments per block"),
            thousands(payment.payments_per_block(&results.limits) as u64),
        ),
    ];
    let mut table = String::from("| Metric | Value |\n|---|---:|\n");
    for (metric, value) in rows {
        table.push_str(&format!("| {metric} | {value} |\n"));
    }
    table
}

fn shapes_table(results: &Results) -> String {
    let limit = results.limits.max_multiplications_per_transaction;
    let mut table = format!(
        "| Shape | Inputs | Outputs | Gates, of {} | Bytes |\n|---|---:|---:|---:|---:|\n",
        thousands(limit as u64)
    );
    for shape in &results.shapes {
        table.push_str(&format!(
            "| {} | {} | {} | {} ({:.0}%) | {} |\n",
            shape.label,
            shape.inputs,
            shape.outputs,
            thousands(shape.multiplications as u64),
            shape.multiplications as f64 / limit as f64 * 100.0,
            thousands(shape.bytes as u64),
        ));
    }
    table.push_str(
        "\nGates is `TxMetrics.multiplications`, the proof's multiplication gates. Bytes is \
         `encoded_size()` of the signed transaction.\n",
    );
    table
}

fn cost_table(results: &Results) -> String {
    let mut table = String::from(
        "| Shape | Verify | Proof (est.) | VM and other | Signature | Utreexo | Total | Decode | Bytes | Per second, one core |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for shape in &results.shapes {
        let flag = if shape.estimate_overshot() {
            " †"
        } else {
            ""
        };
        table.push_str(&format!(
            "| {}{flag} | {} | {} ({:.0}%) | {} | {} | {} | {} | {} | {} | {} |\n",
            shape.label,
            format_time(shape.verify_ns),
            format_time(shape.proof_ns()),
            shape.proof_share() * 100.0,
            format_time(shape.vm_and_other_ns()),
            format_time(shape.signature_ns),
            format_time(shape.utreexo_ns() * shape.inputs as f64),
            format_time(shape.total_ns()),
            format_time(shape.decode_ns),
            thousands(shape.bytes as u64),
            whole(shape.tps_per_core()),
        ));
    }
    let forest = results.shapes[0]
        .largest_forest()
        .map_or_else(|| "no".into(), |time| thousands(time.leaves));
    let decode = match results.shapes[0].decode_method {
        DecodeMethod::BlockTx => {
            "`BlockTx::from_bytes_bounded`, as `flamed` decodes a submitted transaction"
        }
        DecodeMethod::Envelope => "`ExternalTx::from_bytes_bounded` on the envelope",
    };
    table.push_str(&format!(
        "\nProof (est.) is a synthetic R1CS verification with the same multiplier count, an \
         estimate. VM and other is verify minus the estimate minus the signature. \
         Utreexo is one membership check per input in a forest of {forest} leaves. Total is \
         verify plus Utreexo, and per second is one over it. Decode is {decode}.\n"
    ));
    let flagged: Vec<&str> = results
        .shapes
        .iter()
        .filter(|shape| shape.estimate_overshot())
        .map(|shape| shape.label.as_str())
        .collect();
    if !flagged.is_empty() {
        table.push_str(&format!(
            "\n† The synthetic estimate exceeds verify minus signature for {}, so VM and other is \
             clamped to zero: cross-check the proof share with a profile.\n",
            crate::svg::and_list(&flagged)
        ));
    }
    table
}

fn block_capacity_table(results: &Results) -> String {
    let limits = &results.limits;
    let mut table = String::from(
        "| Shape | Transactions per block | Payments per block | One core, full block |\n\
         |---|---:|---:|---:|\n",
    );
    for shape in &results.shapes {
        table.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            shape.label,
            thousands(shape.per_block(limits) as u64),
            thousands(shape.payments_per_block(limits) as u64),
            format_block_time(shape.full_block_ns(limits)),
        ));
    }
    table.push_str(&format!(
        "\nTransactions per block is min({} ÷ gates, {}), rounded down. Payments per block \
         counts every output but the change. One core, full block is transactions per block × \
         (verify + Utreexo per input).\n",
        thousands(limits.max_multiplications as u64),
        thousands(limits.max_transactions as u64),
    ));
    table
}

/// A block's verify time: seconds from one second up.
fn format_block_time(ns: f64) -> String {
    if ns >= 1e9 {
        format!("{:.2} s", ns / 1e9)
    } else {
        format_time(ns)
    }
}

fn latency_table(results: &Results) -> String {
    let runs = results
        .shapes
        .first()
        .map_or(0, |shape| shape.send.fresh.runs);
    let mut table = String::from(
        "| Shape | Warm send | New process: first send | New process: second send | Generator setup |\n\
         |---|---:|---:|---:|---:|\n",
    );
    for shape in &results.shapes {
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            shape.label,
            format_time(shape.warm_send_ns()),
            format_time(shape.first_send_ns()),
            format_time(shape.send.fresh.second_ns),
            format_time(shape.generator_setup_ns()),
        ));
    }
    table.push_str(&format!(
        "\nWarm send is prepare + build + sign + package, criterion medians. New process: two \
         sends in each of {runs} new processes pinned to the same core, medians. Generator setup \
         is the median first send minus the median second send. The chart shows a warm send \
         plus the setup; where the new processes' second sends run faster than the warm \
         median, that sum exceeds their first send.\n"
    ));
    table
}

fn breakdown_table(results: &Results) -> String {
    let mut table = String::from(
        "| Shape | Prepare | Build | Proving (est.) | Sealing (est.) | Prover run and other | Sign | Package | Warm send |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for shape in &results.shapes {
        let send = &shape.send;
        let flag = if shape.send_estimate_overshot() {
            " †"
        } else {
            ""
        };
        table.push_str(&format!(
            "| {}{flag} | {} | {} | {} ({:.0}%) | {} | {} | {} | {} | {} |\n",
            shape.label,
            format_time(send.prepare_ns),
            format_time(send.build_ns),
            format_time(shape.proving_ns()),
            shape.proving_share() * 100.0,
            format_time(shape.sealing_estimate_ns()),
            format_time(shape.prover_run_ns()),
            format_time(send.sign_ns),
            format_time(send.package_ns),
            format_time(shape.warm_send_ns()),
        ));
    }
    table.push_str(
        "\nPrepare is `InputSpec::confidential` for every input. Build is `build_transfer`: \
         sealing the notes, running the prover over the script, and proving. Proving (est.) is \
         a synthetic R1CS proof with the same gate count, and its share is of the warm send. \
         Sealing (est.) is outputs × `open_note`, which does the same key agreement, \
         transcripts, AES-SIV and commitments. Prover run and other is build minus both \
         estimates. Sign is everything `sign` computes; only moving the fields into the \
         `ExternalTx` is left out. Package is `block_tx` and its encoding.\n",
    );
    let flagged: Vec<&str> = results
        .shapes
        .iter()
        .filter(|shape| shape.send_estimate_overshot())
        .map(|shape| shape.label.as_str())
        .collect();
    if !flagged.is_empty() {
        table.push_str(&format!(
            "\n† The estimates exceed build for {}, so prover run and other is clamped to zero.\n",
            crate::svg::and_list(&flagged)
        ));
    }
    table
}

fn capacity_table(results: &Results) -> String {
    let limit = results.limits.max_multiplications_per_transaction;
    let mut table = format!(
        "| Transfer | Outputs | Gates | Of {} | Proves |\n|---|---:|---:|---:|---|\n",
        thousands(limit as u64)
    );
    for shape in &results.shapes {
        table.push_str(&format!(
            "| {} | {} | {} | {:.0}% | yes |\n",
            shape.label,
            shape.outputs,
            thousands(shape.multiplications as u64),
            shape.multiplications as f64 / limit as f64 * 100.0,
        ));
    }
    for attempt in &results.over_capacity {
        let (gates, share) = match attempt.multiplications {
            Some(gates) => (
                thousands(gates as u64),
                format!("{:.0}%", gates as f64 / limit as f64 * 100.0),
            ),
            None => ("not observable".to_owned(), "—".to_owned()),
        };
        let proves = if attempt.proves {
            "yes".to_owned()
        } else {
            match &attempt.error {
                Some(error) => format!("✕ no: `{error}`"),
                None => "✕ no".to_owned(),
            }
        };
        table.push_str(&format!(
            "| {} | {} | {gates} | {share} | {proves} |\n",
            attempt.label, attempt.outputs,
        ));
    }
    if results
        .over_capacity
        .iter()
        .any(|attempt| !attempt.proves && attempt.multiplications.is_none())
    {
        table.push_str(
            "\nA transfer that fails to prove returns no metrics, so its gate count is not \
             observable.\n",
        );
    }
    table
}

fn memory_table(results: &Results) -> String {
    let mut table = String::from("| Shape | Warm send | First send |\n|---|---:|---:|\n");
    for shape in &results.shapes {
        let send = &shape.send;
        table.push_str(&format!(
            "| {} | {} B ({}) | {} B ({}) |\n",
            shape.label,
            thousands(send.warm_peak_bytes),
            megabytes(send.warm_peak_bytes),
            thousands(send.fresh.first_peak_bytes),
            megabytes(send.fresh.first_peak_bytes),
        ));
    }
    table.push_str(
        "\nPeak heap: the most bytes in use at once between the start of a send and its end, \
         beyond what was in use at its start, from a counting allocator. A warm send runs after \
         the generator table was built, so the table is not part of it. A first send builds the \
         table, so it is; the largest of the new processes' peaks is shown. 1 MB is 1,000,000 \
         bytes.\n",
    );
    table
}

fn gas_table(results: &Results) -> String {
    let mut table = String::from(
        "| Shape | Gates | Gas | Gas, mix per gate | Measured | Predicted by gas | Off by | Predicted, mix per gate | Off by |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for shape in &results.shapes {
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            shape.label,
            thousands(shape.multiplications as u64),
            thousands(shape.gas_used),
            thousands(shape.gas_per_gate()),
            format_time(shape.verify_ns),
            format_time(shape.gas_predicted_ns()),
            signed_percent(shape.gas_error(shape.gas_predicted_ns())),
            format_time(shape.gas_per_gate_predicted_ns()),
            signed_percent(shape.gas_error(shape.gas_per_gate_predicted_ns())),
        ));
    }
    table.push_str(&format!(
        "\nPredicted is gas × {NS_PER_GAS:.0} ns. Measured is the verify median. Off by is \
         predicted over measured, minus one. Gas, mix per gate = gas − {GAS_R1CS_ITEM} × \
         (m + n)² + {GAS_R1CS_ITEM} × gates, where m is the inputs plus one for the fee and n \
         the outputs: `mix`'s R1CS charge replaced by {GAS_R1CS_ITEM} per real gate, and every \
         other charge as today.\n"
    ));
    table
}

/// `3 min 20 s`, or `45 s`.
pub fn format_duration(secs: f64) -> String {
    let secs = secs.max(0.0).round() as u64;
    if secs >= 60 {
        format!("{} min {} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

fn reproduce(results: &Results, links: &Links) -> String {
    let mut out = String::from("```text\n");
    for command in &results.run.commands {
        out.push_str(command);
        out.push('\n');
    }
    out.push_str("```\n\n");
    let redraw = relative(&crate::paths::workspace_root(), links.results_file);
    out.push_str(&format!(
        "To redraw the charts and these blocks from the committed results file, without \
         running anything:\n\n```text\ncargo run -p flamebench --bin report -- --results \
         {redraw}\n```\n"
    ));
    if !results.run.durations.is_empty() {
        let durations: Vec<String> = results
            .run
            .durations
            .iter()
            .map(|time| format!("`{}` ran for {}", time.bench, format_duration(time.secs)))
            .collect();
        out.push_str(&format!(
            "\nOn the machine below, {}, fixtures and new processes included. Building first \
             takes longer.\n",
            durations.join(" and ")
        ));
    }
    out
}

fn footer(results: &Results, links: &Links) -> String {
    let machine = &results.machine;
    let group = |filter: &dyn Fn(&crate::cpu::Cpu) -> bool| {
        machine
            .cpu_order
            .iter()
            .filter(|cpu| filter(cpu))
            .map(|cpu| cpu.cpu.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let groups = [
        (
            "fast cores",
            group(&|cpu| cpu.thread == 0 && cpu.class == CoreClass::Fast),
        ),
        (
            "compact cores",
            group(&|cpu| cpu.thread == 0 && cpu.class == CoreClass::Compact),
        ),
        ("second hardware threads", group(&|cpu| cpu.thread > 0)),
    ];
    let order: Vec<String> = groups
        .iter()
        .filter(|(_, cpus)| !cpus.is_empty())
        .map(|(name, cpus)| format!("{cpus} ({name})"))
        .collect();
    let pinned = machine.cpu_order.first().map_or_else(
        || "none".to_owned(),
        |cpu| {
            let class = match cpu.class {
                CoreClass::Fast => "a fast core",
                CoreClass::Compact => "a compact core",
            };
            match cpu.max_freq_khz {
                Some(khz) => format!(
                    "CPU {}, {class}, up to {:.2} GHz",
                    cpu.cpu,
                    khz as f64 / 1e6
                ),
                None => format!("CPU {}, {class}", cpu.cpu),
            }
        },
    );
    let unknown = || "not readable".to_owned();
    let governor = match (&machine.governor, &machine.energy_performance_preference) {
        (Some(governor), Some(epp)) => format!("`{governor}`, energy preference `{epp}`"),
        (Some(governor), None) => format!("`{governor}`"),
        (None, _) => unknown(),
    };
    let link = relative(links.page_dir, links.results_file);
    let name = links
        .results_file
        .file_name()
        .map_or_else(|| link.clone(), |name| name.to_string_lossy().into_owned());
    format!(
        "- Machine: {}, {} logical CPUs\n\
         - Cores: {}\n\
         - Pinned to: {pinned}\n\
         - Governor: {governor}\n\
         - Platform profile: {}\n\
         - Power: {}\n\
         - Compiler: `{}`\n\
         - Commit: `{}`{}\n\
         - Date: {}\n\
         - Results file: [{name}]({link})",
        machine.cpu_model,
        machine.cpu_order.len(),
        order.join("; "),
        machine
            .platform_profile
            .as_ref()
            .map_or_else(unknown, |profile| format!("`{profile}`")),
        machine.power_source.clone().unwrap_or_else(unknown),
        results.run.rustc,
        short_commit(&results.run.commit),
        if results.run.dirty {
            ", with uncommitted changes"
        } else {
            ""
        },
        results.run.date,
    )
}
