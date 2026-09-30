//! `first-send`, run once as the `send` bench runs it: a new process that
//! reads its scratch file, rebuilds the transfer without proving, and
//! sends twice.

use std::fs;
use std::process::Command;

use flamebench::first_send::{Report, Scratch};
use flamebench::fixtures::{self, SHAPES};

#[test]
fn first_send_reads_its_scratch_inputs_and_succeeds() {
    let spec = SHAPES[0];
    let spent = fixtures::funding().into_iter().take(spec.inputs).collect();
    let transfer = fixtures::transfer(spec, spent);
    let scratch = Scratch::new(spec.id, &transfer).expect("scratch");

    let dir = std::env::temp_dir().join(format!("flamebench-first-send-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("1to2.json");
    flamebench::write_json(&file, &scratch).expect("write scratch");

    let output = Command::new(env!("CARGO_BIN_EXE_first-send"))
        .arg(&file)
        .output()
        .expect("run first-send");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "first-send failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Report =
        serde_json::from_str(stdout.lines().last().expect("a line")).expect("a JSON report");
    assert!(report.first_ns > 0 && report.second_ns > 0);
    assert!(report.first_peak_bytes > 0 && report.second_peak_bytes > 0);
    let sent = transfer
        .send(&mut fixtures::rng(fixtures::SEND_RNG_SEED))
        .expect("the same send here");
    assert_eq!(
        report.bytes,
        sent.len(),
        "the child packaged the same shape"
    );

    // A missing or broken scratch file fails with a message.
    let output = Command::new(env!("CARGO_BIN_EXE_first-send"))
        .arg(dir.join("absent.json"))
        .output()
        .expect("run first-send");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("absent.json"));
}
