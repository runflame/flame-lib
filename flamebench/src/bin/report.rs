//! Turns benchmark results into the charts and generated blocks of
//! report 1, `docs/benchmarks/transactions.md`. See `flamebench::report`.

use std::process::ExitCode;

use flamebench::report::{run, Options};

fn main() -> ExitCode {
    let result = Options::parse(std::env::args().skip(1)).and_then(|options| run(&options));
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("report: {error}");
            ExitCode::FAILURE
        }
    }
}
