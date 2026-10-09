//! 🎻 CTL → Rust. Reads one schema and writes generated source to stdout.

use std::{
    env, fs,
    io::{self, Write},
    process::{Command, ExitCode, Stdio},
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let path = args.next().ok_or("usage: ctlc schema.ctl")?;
    if args.next().is_some() {
        return Err("usage: ctlc schema.ctl".into());
    }
    if path == "--help" || path == "-h" {
        println!(
            "🎻 Cell Type Language compiler\nusage: ctlc schema.ctl\nWrites formatted Rust types and Cell codecs to stdout (requires rustfmt)."
        );
        return Ok(());
    }
    let source =
        fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
    let rust =
        cells::ctl::compile(&source).map_err(|e| format!("{}:{e}", path.to_string_lossy()))?;
    let mut formatter = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot format generated Rust: rustfmt: {e}"))?;
    formatter
        .stdin
        .take()
        .expect("piped rustfmt input")
        .write_all(rust.as_bytes())
        .map_err(|e| format!("cannot send generated Rust to rustfmt: {e}"))?;
    let formatted = formatter.wait_with_output().map_err(|e| e.to_string())?;
    if !formatted.status.success() {
        return Err(format!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&formatted.stderr).trim()
        ));
    }
    io::stdout()
        .lock()
        .write_all(&formatted.stdout)
        .map_err(|e| e.to_string())
}
