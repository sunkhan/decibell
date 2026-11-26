//! Where GPUI's (and our) `log` records go: stderr, errors only unless `DECIBELL_LOG` says
//! otherwise (`off`, `error`, `warn`, `info`, `debug`, `trace`). `info` shows which GPU
//! adapter the renderer chose.

use std::io::Write as _;

use log::{LevelFilter, Log, Metadata, Record};

struct Stderr;

impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            // A closed stderr must not take the app down.
            let _ = writeln!(
                std::io::stderr().lock(),
                "decibell: {} [{}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    let level = std::env::var("DECIBELL_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(LevelFilter::Error);
    if log::set_logger(&Stderr).is_ok() {
        log::set_max_level(level);
    }
}
