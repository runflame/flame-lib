//! One new process's first and second send, for the `send` bench. See
//! `flamebench::first_send`.
//!
//! ```text
//! first-send SCRATCH.json
//! ```
//!
//! Prints one JSON line: both sends' times, their peak heap, and the
//! length of what they packaged.

use std::path::Path;
use std::process::ExitCode;

use flamebench::alloc::Counting;
use flamebench::first_send;

#[global_allocator]
static ALLOC: Counting = Counting::new();

fn main() -> ExitCode {
    let Some(scratch) = std::env::args().nth(1) else {
        eprintln!("usage: first-send SCRATCH.json");
        return ExitCode::FAILURE;
    };
    match first_send::run(Path::new(&scratch), &ALLOC) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string(&report).expect("a report serializes")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("first-send: {error}");
            ExitCode::FAILURE
        }
    }
}
