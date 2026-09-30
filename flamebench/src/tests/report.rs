//! The results file, `report --results`, `report --verifier-scaling`, and
//! the generated blocks. Every test writes into a temporary directory,
//! never into `docs/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{sample_cpus, sample_results, sample_scaling, TempDir};
use crate::docs::{begin_marker, blocks, end_marker, regenerate, skeleton, Links, BLOCKS};
use crate::report::{civil_date, from_criterion, from_criterion_and_draw, run, Mode, Options};
use crate::results::{
    FixtureShape, FixturesFile, FreshProcess, Results, SendFile, SendShape, GAS_R1CS_ITEM,
};
use crate::svg::{kilobytes, megabytes, Theme};
use crate::{paths, FIXTURES_FILE, SEND_FILE};

/// Writes `results` into `dir` and returns the options that draw from it
/// into `dir/out` and `dir/transactions.md`.
fn options_for(dir: &Path, results: &Results) -> Options {
    let file = dir.join("results.json");
    crate::write_json(&file, results).expect("write results");
    Options {
        mode: Mode::FromResults(file),
        out_dir: dir.join("out"),
        page: dir.join("transactions.md"),
    }
}

fn svgs(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<PathBuf> = svgs(&paths::charts_dir(&dir.join("out")));
    files.push(dir.join("transactions.md"));
    files
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).expect("read");
            (path, bytes)
        })
        .collect()
}

/// Every file under `dir`, with its length and modification time.
fn tree(dir: &Path) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = entry.metadata().expect("metadata");
            if meta.is_dir() {
                pending.push(path);
            } else {
                out.push((path, meta.len(), meta.modified().ok()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_results_file_round_trips() {
    let results = sample_results();
    results.validate().expect("the sample is valid");
    let text = serde_json::to_string_pretty(&results).expect("serialize");
    assert!(text.contains("\"schema\": 2"));
    assert!(!text.contains("scaling"));
    assert!(!text.contains("block_interval"));
    let back: Results = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(back, results);
    assert_eq!(
        results.file_name(),
        "2026-09-30-ryzen-ai-7-350-0123456.json"
    );

    // A schema-1 file is refused by name.
    let mut old = results.clone();
    old.schema = 1;
    let error = old.validate().expect_err("schema 1");
    assert!(error.contains("schema 1 is not 2"), "{error}");
}

#[test]
fn report_results_draws_fourteen_svgs_and_the_blocks() {
    let dir = TempDir::new("draw");
    let options = options_for(dir.path(), &sample_results());
    let output = run(&options).expect("report --results");
    assert!(output.contains("1 → 2"), "prints the developer table");
    assert!(output.contains("1 → 14: does not prove (VMError::R1CSProofConstruction)"));

    let mut expected = Vec::new();
    for chart in [
        "cost-by-shape",
        "gas-check",
        "peak-memory",
        "proof-capacity",
        "send-breakdown",
        "send-latency",
        "tps-per-core",
    ] {
        for theme in Theme::ALL {
            expected.push(format!("{chart}.{}.svg", theme.name()));
        }
    }
    expected.sort();
    assert_eq!(
        names(&svgs(&paths::charts_dir(&dir.path().join("out")))),
        expected
    );

    let page = fs::read_to_string(&options.page).expect("page");
    for name in BLOCKS {
        assert!(page.contains(&begin_marker(name)), "{name} begins");
        assert!(page.contains(&end_marker(name)), "{name} ends");
    }
    assert!(page.contains(
        "<source media=\"(prefers-color-scheme: dark)\" srcset=\"out/charts/transactions/cost-by-shape.dark.svg\">"
    ));
    assert!(page.contains("src=\"out/charts/transactions/cost-by-shape.light.svg\" width=\"720\">"));
    assert!(page.contains("[results.json](results.json)"));

    // Key numbers, in kilobytes and per block.
    assert!(page.contains("| Verify a 1 → 2 payment | 2.22 ms |"));
    assert!(page.contains("| Size of a 1 → 2 payment | 1.9 KB |"));
    assert!(page.contains("| Most outputs in one transaction | 13 |"));
    assert!(page.contains("| 1 → 2 payments per block | 729 |"));
    assert!(page.contains("| Send a 1 → 2 payment, warm | 14.7 ms |"));
    // The first send is the new processes' measured median.
    assert!(page.contains("| Send a 1 → 2 payment, first in a new process | 47.5 ms |"));
    assert!(page.contains("| 1 → 2 | 14.7 ms | 47.5 ms | 15.8 ms | 31.7 ms |"));

    // Tables keep exact bytes.
    assert!(page.contains("| 1 → 13 | 1 | 13 | 885 (86%) | 4,227 |"));

    // Block capacity: min(100,000 ÷ gates, 10,000), payments without the
    // change, and one core's time for the block.
    assert!(page.contains("| 1 → 2 | 729 | 729 | 1.63 s |"), "{page}");
    assert!(page.contains("| 1 → 13 | 112 | 1,344 | 806 ms |"));

    // The capacity table names the failure and invents no count.
    assert!(page
        .contains("| 1 → 14 | 14 | not observable | — | ✕ no: `VMError::R1CSProofConstruction` |"));

    // The three-way gas table and its formula.
    assert!(page.contains("| 1 → 2 | 137 | 6,238 | 20,758 |"));
    assert!(page.contains("gas − 120 × (m + n)² + 120 × gates"));

    // Both peaks, and why they differ.
    assert!(page.contains("| 1 → 2 | 900,000 B (0.9 MB) | 1,400,000 B (1.4 MB) |"));

    // The commands, the redraw and the running time.
    assert!(page.contains("taskset -c 2 cargo bench -p flamebench --bench send"));
    assert!(page.contains("--results"));
    assert!(page.contains("`tx_cost` ran for 4 min 11 s and `send` ran for 5 min 18 s"));

    // The footer.
    assert!(page.contains("- Pinned to: CPU 2, a fast core, up to 5.09 GHz"));
    assert!(page.contains("- Platform profile: `balanced`"));
    assert!(page.contains("- Governor: `powersave`, energy preference `balance_performance`"));
}

#[test]
fn the_default_page_is_report_one() {
    assert_eq!(
        paths::default_page(),
        paths::workspace_root()
            .join("docs")
            .join("benchmarks")
            .join("transactions.md")
    );
    assert_eq!(
        paths::results_dir(&paths::default_out_dir()),
        paths::workspace_root().join("docs/benchmarks/results/transactions")
    );
}

#[test]
fn no_generated_block_or_chart_holds_a_chain_rate() {
    let results = sample_results();
    let dir = TempDir::new("no-rate");
    let options = options_for(dir.path(), &results);
    run(&options).expect("draw");
    let mut texts: Vec<(String, String)> = svgs(&paths::charts_dir(&dir.path().join("out")))
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).expect("svg");
            (path.display().to_string(), text)
        })
        .collect();
    let charts = crate::svg::charts(&results);
    for (name, content) in blocks(
        &results,
        &charts,
        &Links {
            page_dir: dir.path(),
            charts_dir: &paths::charts_dir(&dir.path().join("out")),
            results_file: &dir.path().join("results.json"),
        },
    ) {
        texts.push((name.to_owned(), content));
    }

    // What the first run published: transactions per block over the devnet
    // node's 15 s, and the like for other intervals.
    let mut rates = Vec::new();
    for shape in &results.shapes {
        for secs in [5.0, 10.0, 12.0, 15.0, 20.0, 30.0, 60.0] {
            for per_block in [
                shape.per_block(&results.limits),
                shape.payments_per_block(&results.limits),
            ] {
                rates.push(format!("{:.1} TPS", per_block as f64 / secs));
                rates.push(format!("{:.1} per second", per_block as f64 / secs));
            }
        }
    }
    for (name, text) in &texts {
        for banned in [
            "ceiling",
            "Ceiling",
            "block interval",
            "consensus cap",
            "TPS",
        ] {
            assert!(!text.contains(banned), "{name} holds `{banned}`");
        }
        for rate in &rates {
            assert!(!text.contains(rate.as_str()), "{name} holds `{rate}`");
        }
        // SVGs are one line each, and their tags hold `/s`; the blocks are
        // Markdown, one sentence or row per line.
        if name.ends_with(".svg") {
            continue;
        }
        for line in text.lines().filter(|line| line.contains("block")) {
            assert!(
                !line.contains("per second") && !line.contains("/s"),
                "{name} gives a per-second figure for a block: {line}"
            );
        }
    }
}

#[test]
fn the_three_way_gas_check_on_a_hand_written_result() {
    let results = sample_results();
    let per_gate: Vec<u64> = results
        .shapes
        .iter()
        .map(|shape| shape.gas_per_gate())
        .collect();
    // The first run's gas and gates: `mix` charged 120 per (m + n)² item,
    // replaced by 120 per real gate.
    assert_eq!(per_gate, [20_758, 22_136, 41_932, 117_118]);
    let payment = results.payment();
    assert_eq!(payment.mix_inputs(), 2, "one input and the fee");
    assert_eq!(payment.mix_items(), 16);
    assert_eq!(
        payment.gas_per_gate(),
        payment.gas_used - GAS_R1CS_ITEM * 16 + GAS_R1CS_ITEM * 137
    );
    assert!((payment.gas_predicted_ns() - 623_800.0).abs() < 1e-6);
    assert!((payment.gas_per_gate_predicted_ns() - 2_075_800.0).abs() < 1e-6);
    let today = payment.gas_error(payment.gas_predicted_ns());
    let fixed = payment.gas_error(payment.gas_per_gate_predicted_ns());
    assert!((today - (623_800.0 / 2_222_000.0 - 1.0)).abs() < 1e-12);
    assert!((fixed - (2_075_800.0 / 2_222_000.0 - 1.0)).abs() < 1e-12);
    assert_eq!(crate::svg::signed_percent(today), "−72%");
    assert_eq!(crate::svg::signed_percent(fixed), "−7%");
    assert_eq!(crate::svg::signed_percent(0.121), "+12%");

    // Without a fee, `mix` has one input fewer.
    let mut free = payment.clone();
    free.fee = 0;
    assert_eq!(free.mix_items(), 9);
}

#[test]
fn sizes_read_in_decimal_kilobytes() {
    assert_eq!(kilobytes(1_921), "1.9 KB");
    assert_eq!(kilobytes(4_227), "4.2 KB");
    assert_eq!(kilobytes(2_885), "2.9 KB");
    assert_eq!(kilobytes(999), "1.0 KB");
    assert_eq!(kilobytes(0), "0.0 KB");
    assert_eq!(megabytes(1_400_000), "1.4 MB");
    assert_eq!(crate::docs::format_duration(318.0), "5 min 18 s");
    assert_eq!(crate::docs::format_duration(42.4), "42 s");
    assert_eq!(crate::svg::and_list(&["1 → 2"]), "1 → 2");
    assert_eq!(
        crate::svg::and_list(&["1 → 2", "1 → 13"]),
        "1 → 2 and 1 → 13"
    );
    assert_eq!(crate::svg::and_list(&["a", "b", "c"]), "a, b and c");
}

#[test]
fn drawing_twice_gives_identical_bytes() {
    let dir = TempDir::new("twice");
    let options = options_for(dir.path(), &sample_results());
    run(&options).expect("first");
    let first = snapshot(dir.path());
    run(&options).expect("second");
    assert_eq!(snapshot(dir.path()), first);
}

#[test]
fn a_new_page_gets_every_marker_pair_and_keeps_its_prose() {
    let dir = TempDir::new("page");
    let options = options_for(dir.path(), &sample_results());
    assert!(!options.page.exists());
    run(&options).expect("creates the page");
    let page = fs::read_to_string(&options.page).expect("page");
    for name in BLOCKS {
        assert_eq!(page.matches(&begin_marker(name)).count(), 1);
        assert_eq!(page.matches(&end_marker(name)).count(), 1);
    }
    assert!(page.starts_with(
        "<!-- Generated blocks are rewritten by `report`. Prose outside them was written from \
         run results.json; re-check it after a re-run. -->\n\n# What a Flame transaction costs\n"
    ));
    assert!(page.contains("Report 1 of 2. Next: [the node](../benchmarks.md), planned."));

    // Prose written around the blocks survives a redraw byte for byte.
    let written = page.replace(
        "<!-- Prose: the finding. -->",
        "Gas holds.\n\nIt holds with room to spare.",
    );
    fs::write(&options.page, &written).expect("write prose");
    let mut changed = sample_results();
    changed.shapes[0].verify_ns *= 1.5;
    let options = options_for(dir.path(), &changed);
    run(&options).expect("redraw");
    let redrawn = fs::read_to_string(&options.page).expect("page");
    assert_ne!(redrawn, written, "the blocks changed");
    assert_eq!(outside_blocks(&redrawn), outside_blocks(&written));
    assert!(redrawn.contains("Gas holds.\n\nIt holds with room to spare."));
}

/// Everything outside the generated blocks.
fn outside_blocks(page: &str) -> String {
    let mut out = String::new();
    let mut rest = page;
    for name in BLOCKS {
        let begin = begin_marker(name);
        let end = end_marker(name);
        let start = rest.find(&begin).expect("begin") + begin.len();
        out.push_str(&rest[..start]);
        rest = &rest[rest.find(&end).expect("end")..];
    }
    out.push_str(rest);
    out
}

#[test]
fn regenerating_leaves_the_outside_unchanged() {
    let page =
        format!("intro\n{}", skeleton("x.json")).replace("## Machine", "## Machine  \n\nend notes");
    let blocks: Vec<(&str, String)> = BLOCKS
        .iter()
        .map(|name| (*name, format!("new {name}")))
        .collect();
    let once = regenerate(&page, &blocks).expect("regenerate");
    assert_eq!(outside_blocks(&once), outside_blocks(&page));
    assert!(once.contains(&format!(
        "{}\n\nnew footer\n\n{}",
        begin_marker("footer"),
        end_marker("footer")
    )));
    assert_eq!(regenerate(&once, &blocks).expect("again"), once);
}

#[test]
fn a_missing_marker_is_a_clear_error() {
    let page = skeleton("x.json").replace(&end_marker("gas-check"), "");
    let blocks: Vec<(&str, String)> = BLOCKS.iter().map(|name| (*name, String::new())).collect();
    let error = regenerate(&page, &blocks).expect_err("missing marker");
    assert!(
        error.contains("<!-- flamebench:end gas-check -->"),
        "{error}"
    );

    let dir = TempDir::new("missing-marker");
    let options = options_for(dir.path(), &sample_results());
    let page = skeleton("x.json").replace(&begin_marker("footer"), "");
    fs::write(&options.page, &page).expect("write page");
    let error = run(&options).expect_err("missing marker");
    assert!(error.contains("flamebench:begin footer"), "{error}");
    assert_eq!(
        fs::read_to_string(&options.page).unwrap(),
        page,
        "left untouched"
    );
    assert!(
        svgs(&paths::charts_dir(&options.out_dir)).is_empty(),
        "and no chart written"
    );

    let doubled = format!("{}\n{}", skeleton("x.json"), begin_marker("footer"));
    let error = regenerate(&doubled, &blocks).expect_err("repeated marker");
    assert!(error.contains("more than once"), "{error}");
}

#[test]
fn an_overshooting_estimate_is_clamped_and_flagged() {
    let mut results = sample_results();
    let shape = &mut results.shapes[3];
    shape.r1cs_estimate_ns = shape.verify_ns * 1.2;
    shape.send.prove_estimate_ns = shape.send.build_ns;
    let shape = &results.shapes[3];
    assert!(shape.estimate_overshot());
    assert_eq!(shape.vm_and_other_ns(), 0.0);
    assert_eq!(shape.proof_ns(), shape.verify_ns - shape.signature_ns);
    assert!(
        shape.send_estimate_overshot(),
        "proving plus sealing exceed build"
    );
    assert_eq!(shape.prover_run_ns(), 0.0);
    assert!(!results.shapes[0].estimate_overshot());
    assert!(!results.shapes[0].send_estimate_overshot());
    assert!(results.shapes[0].vm_and_other_ns() > 0.0);
    assert!(results.shapes[0].prover_run_ns() > 0.0);

    let dir = TempDir::new("flag");
    let options = options_for(dir.path(), &results);
    let output = run(&options).expect("draw");
    assert!(output.contains("estimate overshot"), "{output}");
    assert!(output.contains("send estimates overshot build"), "{output}");
    let page = fs::read_to_string(&options.page).expect("page");
    assert!(page.contains("| 1 → 13 † |"), "the tables flag the shape");
    assert!(page.contains("† The synthetic estimate exceeds"));
    assert!(page.contains("† The estimates exceed build for 1 → 13"));
    assert!(!page.contains("| 1 → 2 † |"));
    let chart = fs::read_to_string(
        dir.path()
            .join("out/charts/transactions/cost-by-shape.light.svg"),
    )
    .expect("svg");
    assert!(chart.contains("Flagged: the estimate exceeds"));
    let chart = fs::read_to_string(
        dir.path()
            .join("out/charts/transactions/send-breakdown.light.svg"),
    )
    .expect("svg");
    assert!(chart.contains("Flagged: the estimates exceed build for 1 → 13"));
}

/// The scratch files both benches leave, for the sample's first shape.
fn scratch_files(scratch: &Path) -> (FixturesFile, SendFile) {
    let results = sample_results();
    let shape = &results.shapes[0];
    let fixture = FixtureShape {
        id: shape.id.clone(),
        label: shape.label.clone(),
        inputs: shape.inputs,
        outputs: shape.outputs,
        fee: shape.fee,
        bytes: shape.bytes,
        gas_used: shape.gas_used,
        multiplications: shape.multiplications,
        decode_method: shape.decode_method,
    };
    let fixtures = FixturesFile {
        shapes: vec![fixture.clone()],
        utreexo_leaves: vec![1 << 10],
        limits: results.limits,
        machine: sample_cpus(),
        elapsed_secs: 100.0,
    };
    let send = SendFile {
        shapes: vec![SendShape {
            fixture,
            warm_peak_bytes: 800_000,
            fresh: FreshProcess {
                runs: 15,
                first_ns: 46e6,
                second_ns: 15e6,
                first_peak_bytes: 1_300_000,
            },
        }],
        over_capacity: results.over_capacity.clone(),
        machine: sample_cpus(),
        elapsed_secs: 200.0,
    };
    crate::write_json(&scratch.join(FIXTURES_FILE), &fixtures).unwrap();
    crate::write_json(&scratch.join(SEND_FILE), &send).unwrap();
    (fixtures, send)
}

#[test]
fn report_fails_clearly_without_its_inputs() {
    let dir = TempDir::new("missing");
    let criterion = dir.path().join("criterion");
    let scratch = dir.path().join("scratch");

    let error = from_criterion(&criterion, &scratch).expect_err("no scratch files");
    assert!(error.contains("fixtures.json is missing"), "{error}");
    assert!(error.contains("--bench tx_cost"), "{error}");

    let (fixtures, send) = scratch_files(&scratch);
    fs::remove_file(scratch.join(SEND_FILE)).unwrap();
    let error = from_criterion(&criterion, &scratch).expect_err("no send.json");
    assert!(error.contains("send.json is missing"), "{error}");
    assert!(error.contains("--bench send"), "{error}");

    // The two benches must have run under one set of conditions.
    let mut other = send.clone();
    other.machine.power_source = Some("battery".into());
    crate::write_json(&scratch.join(SEND_FILE), &other).unwrap();
    let error = from_criterion(&criterion, &scratch).expect_err("different conditions");
    assert!(error.contains("different conditions"), "{error}");
    assert!(error.contains("power battery"), "{error}");

    // And on the same fixtures.
    let mut other = send.clone();
    other.shapes[0].fixture.bytes += 1;
    crate::write_json(&scratch.join(SEND_FILE), &other).unwrap();
    let error = from_criterion(&criterion, &scratch).expect_err("different fixtures");
    assert!(error.contains("no longer the same bytes"), "{error}");
    crate::write_json(&scratch.join(SEND_FILE), &send).unwrap();

    let error = from_criterion(&criterion, &scratch).expect_err("no criterion dir");
    assert!(error.contains("no criterion results"), "{error}");

    // Every estimate but one.
    let ids = [
        ("tx_cost", "verify", "1to2", 2.4e6),
        ("tx_cost", "decode", "1to2", 7.9e4),
        ("tx_cost", "signature", "1to2", 5.0e4),
        ("tx_cost", "r1cs_synthetic", "137", 1.9e6),
        ("tx_cost", "utreexo", "1024", 9.0e3),
        ("send", "prepare", "1to2", 9.0e4),
        ("send", "build", "1to2", 1.4e7),
        ("send", "sign", "1to2", 1.3e5),
        ("send", "package", "1to2", 5.0e4),
        ("send", "open_note", "1to2", 1.5e5),
    ];
    for (group, function, parameter, ns) in ids {
        write_estimate(&criterion, group, function, parameter, ns);
    }
    let error = from_criterion(&criterion, &scratch).expect_err("no prove estimate");
    assert!(
        error.contains("no estimate for send/r1cs_prove_synthetic/137"),
        "{error}"
    );

    write_estimate(&criterion, "send", "r1cs_prove_synthetic", "137", 1.2e7);
    let results = from_criterion(&criterion, &scratch).expect("complete");
    let shape = &results.shapes[0];
    assert_eq!(shape.verify_ns, 2.4e6);
    assert_eq!(shape.utreexo[0].depth, 10);
    assert_eq!(shape.send.build_ns, 1.4e7);
    assert_eq!(shape.send.prove_estimate_ns, 1.2e7);
    assert_eq!(shape.send.fresh, send.shapes[0].fresh);
    assert_eq!(shape.send.warm_peak_bytes, 800_000);
    assert_eq!(results.over_capacity, send.over_capacity);
    assert_eq!(results.limits, fixtures.limits);
    assert_eq!(
        results.machine.platform_profile.as_deref(),
        Some("balanced")
    );
    assert_eq!(
        results
            .run
            .durations
            .iter()
            .map(|time| (time.bench.as_str(), time.secs))
            .collect::<Vec<_>>(),
        [("tx_cost", 100.0), ("send", 200.0)]
    );
    // Every median with its confidence interval, once.
    assert_eq!(results.estimates.len(), 11);
    let verify = results
        .estimates
        .iter()
        .find(|estimate| estimate.id == "tx_cost/verify/1to2")
        .expect("verify's interval");
    assert_eq!(
        (verify.lower_ns, verify.median_ns, verify.upper_ns),
        (2.4e6 * 0.99, 2.4e6, 2.4e6 * 1.02)
    );

    // An estimate directory without its estimates is an error too.
    fs::remove_file(criterion.join("tx_cost/decode/1to2/new/estimates.json")).unwrap();
    let error = from_criterion(&criterion, &scratch).expect_err("no estimates.json");
    assert!(error.contains("estimates.json"), "{error}");

    let options = Options {
        mode: Mode::FromResults(dir.path().join("absent.json")),
        out_dir: dir.path().join("out"),
        page: dir.path().join("page.md"),
    };
    let error = run(&options).expect_err("no results file");
    assert!(error.contains("absent.json"), "{error}");
}

#[test]
fn a_page_without_its_markers_leaves_no_results_file_behind() {
    let dir = TempDir::new("broken-page");
    let criterion = dir.path().join("criterion");
    let scratch = dir.path().join("scratch");
    scratch_files(&scratch);
    let ids = [
        ("tx_cost", "verify", "1to2"),
        ("tx_cost", "decode", "1to2"),
        ("tx_cost", "signature", "1to2"),
        ("tx_cost", "r1cs_synthetic", "137"),
        ("tx_cost", "utreexo", "1024"),
        ("send", "prepare", "1to2"),
        ("send", "build", "1to2"),
        ("send", "sign", "1to2"),
        ("send", "package", "1to2"),
        ("send", "open_note", "1to2"),
        ("send", "r1cs_prove_synthetic", "137"),
    ];
    for (group, function, parameter) in ids {
        write_estimate(&criterion, group, function, parameter, 1e6);
    }
    let options = Options {
        mode: Mode::FromCriterion,
        out_dir: dir.path().join("out"),
        page: dir.path().join("transactions.md"),
    };
    let page = skeleton("x.json").replace(&end_marker("gas-check"), "");
    fs::write(&options.page, &page).expect("write page");
    let error =
        from_criterion_and_draw(&criterion, &scratch, &options).expect_err("missing marker");
    assert!(error.contains("flamebench:end gas-check"), "{error}");
    assert!(
        tree(&options.out_dir).is_empty(),
        "nothing written: {error}"
    );
    assert_eq!(fs::read_to_string(&options.page).unwrap(), page);

    // With the page whole, the results file lands next to the charts.
    fs::remove_file(&options.page).unwrap();
    let output = from_criterion_and_draw(&criterion, &scratch, &options).expect("draws");
    let written = tree(&paths::results_dir(&options.out_dir));
    assert_eq!(written.len(), 1, "{output}");
    assert_eq!(svgs(&paths::charts_dir(&options.out_dir)).len(), 14);
    assert!(
        svgs(&options.out_dir).is_empty(),
        "no chart at the top of the output directory"
    );
}

/// Writes the two files criterion 0.8 leaves for one benchmark.
fn write_estimate(criterion: &Path, group: &str, function: &str, parameter: &str, ns: f64) {
    let dir = criterion
        .join(group)
        .join(function)
        .join(parameter)
        .join("new");
    fs::create_dir_all(&dir).unwrap();
    let benchmark = serde_json::json!({
        "group_id": group,
        "function_id": function,
        "value_str": parameter,
        "throughput": null,
        "full_id": format!("{group}/{function}/{parameter}"),
        "directory_name": format!("{group}/{function}/{parameter}"),
        "title": format!("{group}/{function}/{parameter}"),
    });
    fs::write(dir.join("benchmark.json"), benchmark.to_string()).unwrap();
    let estimate = |point: f64| {
        serde_json::json!({
            "confidence_interval": {
                "confidence_level": 0.95,
                "lower_bound": point * 0.99,
                "upper_bound": point * 1.02,
            },
            "point_estimate": point,
            "standard_error": 0.0,
        })
    };
    let estimates = serde_json::json!({
        "mean": estimate(ns * 1.01),
        "median": estimate(ns),
        "median_abs_dev": estimate(0.0),
        "slope": null,
        "std_dev": estimate(0.0),
    });
    fs::write(dir.join("estimates.json"), estimates.to_string()).unwrap();
}

#[test]
fn verifier_scaling_draws_into_the_target_and_never_into_docs() {
    let docs = paths::docs_dir();
    let before = tree(&docs);

    let dir = TempDir::new("scaling");
    let file = dir.path().join(crate::scaling::FILE);
    crate::write_json(&file, &sample_scaling()).expect("write scaling");
    let options = Options {
        mode: Mode::VerifierScaling(file.clone()),
        out_dir: dir.path().join("out"),
        page: paths::default_page(),
    };
    let output = run(&options).expect("report --verifier-scaling");
    assert!(output.contains("threads"), "prints the table");
    assert!(output.contains("compact cores"), "{output}");
    assert!(!output.contains("ceiling"));
    assert_eq!(
        names(&svgs(&dir.path().join("out"))),
        [
            "verifier-efficiency.dark.svg",
            "verifier-efficiency.light.svg",
            "verifier-scaling.dark.svg",
            "verifier-scaling.light.svg",
        ]
    );

    // Pointed at docs/, it refuses and writes nothing.
    for out_dir in [paths::default_out_dir(), docs.clone()] {
        let options = Options {
            mode: Mode::VerifierScaling(file.clone()),
            out_dir,
            page: paths::default_page(),
        };
        let error = run(&options).expect_err("docs/ is refused");
        assert!(error.contains("never go under docs/"), "{error}");
    }
    assert_eq!(tree(&docs), before, "nothing under docs/ changed");

    let error = crate::report::verifier_scaling(&dir.path().join("absent.json"), dir.path())
        .expect_err("no file");
    assert!(error.contains("--bench verifier_scaling"), "{error}");
}

#[test]
fn options_parse() {
    let parse = |args: &[&str]| Options::parse(args.iter().map(|arg| arg.to_string()));
    assert_eq!(parse(&["--cpu-order"]).unwrap().mode, Mode::CpuOrder);
    let default = parse(&[]).unwrap();
    assert_eq!(default.mode, Mode::FromCriterion);
    assert_eq!(default.out_dir, paths::default_out_dir());
    assert_eq!(default.page, paths::default_page());
    let options = parse(&["--results", "r.json", "--out-dir", "o", "--page", "p.md"]).unwrap();
    assert_eq!(options.mode, Mode::FromResults("r.json".into()));
    assert_eq!(options.out_dir, PathBuf::from("o"));
    assert_eq!(options.page, PathBuf::from("p.md"));

    let scaling = parse(&["--verifier-scaling"]).unwrap();
    assert_eq!(
        scaling.mode,
        Mode::VerifierScaling(paths::scratch_dir().join("verifier_scaling.json"))
    );
    assert_eq!(scaling.out_dir, paths::scratch_dir());
    let scaling = parse(&["--out-dir", "o", "--verifier-scaling"]).unwrap();
    assert_eq!(scaling.out_dir, PathBuf::from("o"));

    assert!(parse(&["--results"]).is_err());
    assert!(parse(&["--bogus"]).is_err());
}

#[test]
fn civil_dates() {
    assert_eq!(civil_date(0), (1970, 1, 1));
    assert_eq!(civil_date(20_723), (2026, 9, 27));
    assert_eq!(civil_date(11_016), (2000, 2, 29));
}
