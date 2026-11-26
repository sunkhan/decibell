//! The Decibell desktop client.

use std::process::ExitCode;

use decibell_app::{args::Args, boot};

fn main() -> ExitCode {
    decibell_harness::perf::start();
    match Args::parse(std::env::args().skip(1)) {
        Ok(args) => boot::run(args),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}
