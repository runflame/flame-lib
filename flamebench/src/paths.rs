//! Where everything is read and written.
//!
//! Cargo runs a benchmark from the package directory and `cargo run` from
//! wherever the user is, so every path starts at this crate's manifest.

use std::env;
use std::path::{Path, PathBuf};

/// The workspace root: this crate's parent directory.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("flamebench sits inside the workspace")
        .to_path_buf()
}

/// `CARGO_TARGET_DIR` if set, otherwise the workspace's `target/`.
pub fn target_dir() -> PathBuf {
    match env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => absolute(PathBuf::from(dir)),
        None => workspace_root().join("target"),
    }
}

/// Where the benchmark leaves what the report reads besides criterion's
/// estimates: `<target>/flamebench/`.
pub fn scratch_dir() -> PathBuf {
    target_dir().join("flamebench")
}

/// Where criterion 0.8 writes its results. It takes `CRITERION_HOME`, then
/// `CARGO_TARGET_DIR/criterion`, then the target directory `cargo metadata`
/// reports; without a `build.target-dir` override that last one is the
/// workspace's `target/`.
pub fn criterion_dir() -> PathBuf {
    match env::var_os("CRITERION_HOME") {
        Some(dir) => absolute(PathBuf::from(dir)),
        None => target_dir().join("criterion"),
    }
}

/// The workspace's `docs/`, where nothing unpublished may be written.
pub fn docs_dir() -> PathBuf {
    workspace_root().join("docs")
}

/// Everything a report publishes lives under `docs/benchmarks/`: the page,
/// its charts under `charts/<report>/`, its results under `results/<report>/`.
pub fn default_out_dir() -> PathBuf {
    docs_dir().join("benchmarks")
}

/// Report 1: `docs/benchmarks/transactions.md`.
pub fn default_page() -> PathBuf {
    default_out_dir().join("transactions.md")
}

/// Where report 1's charts go, under an output directory:
/// `<out_dir>/charts/transactions/`.
pub fn charts_dir(out_dir: &Path) -> PathBuf {
    out_dir.join("charts").join("transactions")
}

/// Where report 1's results files go, under an output directory:
/// `<out_dir>/results/transactions/`.
pub fn results_dir(out_dir: &Path) -> PathBuf {
    out_dir.join("results").join("transactions")
}

/// Whether `path` is `dir` or inside it, both made absolute first.
pub fn is_within(path: &Path, dir: &Path) -> bool {
    normalize(&absolute(path.to_path_buf())).starts_with(normalize(&absolute(dir.to_path_buf())))
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map(|dir| dir.join(&path))
            .unwrap_or(path)
    }
}

/// `to` relative to the directory `from`, with `/` separators, for links in
/// the docs page. Both are made absolute first.
pub fn relative(from: &Path, to: &Path) -> String {
    let from = normalize(&absolute(from.to_path_buf()));
    let to = normalize(&absolute(to.to_path_buf()));
    let common = from
        .components()
        .zip(to.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = from
        .components()
        .skip(common)
        .map(|_| "..".into())
        .collect();
    parts.extend(
        to.components()
            .skip(common)
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Removes `.` and folds `..` without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}
