//! The Decibell desktop client.

use std::process::ExitCode;

use decibell_app::{args::Args, boot};

fn main() -> ExitCode {
    decibell_harness::perf::start();
    // SVT-AV1 (the software screen-share encoder of last resort) prints its whole configuration to
    // stderr every time it opens unless told otherwise; level 1 keeps its errors. A value the user
    // set wins.
    if std::env::var_os("SVT_LOG").is_none() {
        // SAFETY: nothing has spawned a thread yet (`perf::start` only reads the clock), so no
        // other thread can be reading the environment.
        unsafe { std::env::set_var("SVT_LOG", "1") };
    }
    match Args::parse(std::env::args().skip(1)) {
        Ok(args) => boot::run(args),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}
