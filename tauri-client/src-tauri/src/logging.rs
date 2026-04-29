//! Persistent logging for release builds.
//!
//! Windows release Tauri apps run under windows_subsystem = "windows" and
//! have no console attached, so eprintln! writes to a void — diagnostics
//! that work in `npm run tauri dev` go silent in the installed exe.
//!
//! This module fixes that on Windows by redirecting STD_ERROR_HANDLE to
//! a log file at startup. Existing `eprintln!` calls continue to work
//! and now persist to disk.
//!
//! On all platforms, a panic hook captures crash details (location,
//! payload, backtrace, thread name) to a separate crash.log so a release
//! crash can be diagnosed post-mortem.
//!
//! Log locations:
//!   Windows: %LOCALAPPDATA%\Decibell\decibell.log + crash.log
//!   Linux:   $XDG_STATE_HOME/decibell/ (or ~/.local/state/decibell/)

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

fn log_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        let mut p = PathBuf::from(local);
        p.push("Decibell");
        std::fs::create_dir_all(&p).ok()?;
        Some(p)
    }
    #[cfg(target_os = "linux")]
    {
        let p = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local");
                p.push("state");
                p
            }))?;
        let mut p = p;
        p.push("decibell");
        std::fs::create_dir_all(&p).ok()?;
        Some(p)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

fn timestamp_str() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    // Rough UTC HH:MM:SS — no chrono dependency.
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

/// Initialize logging. Call this at the top of `pub fn run()` BEFORE any
/// eprintln! happens elsewhere — on Windows, Rust's std::io::stderr()
/// reads GetStdHandle each write, so changing STD_ERROR_HANDLE before
/// the first write redirects all subsequent output transparently.
pub fn setup() {
    let dir = match log_dir() {
        Some(d) => d,
        None => return,
    };

    let log_path = dir.join("decibell.log");
    rotate_if_oversized(&log_path);
    redirect_stderr(&log_path);

    let crash_path = dir.join("crash.log");
    install_panic_hook(crash_path);

    eprintln!();
    eprintln!("=== Decibell startup {} ===", timestamp_str());
    eprintln!("[logging] log dir: {}", dir.display());
    eprintln!("[logging] version: {}", env!("CARGO_PKG_VERSION"));
}

/// Truncate the log file if it has grown beyond 10 MB so we don't fill
/// the user's disk over many sessions. We don't keep prior log content
/// across rotations — for crash post-mortem the current run's traces
/// are what matter, and crash.log keeps panic details separately.
fn rotate_if_oversized(log_path: &std::path::Path) {
    if let Ok(meta) = std::fs::metadata(log_path) {
        if meta.len() > 10 * 1024 * 1024 {
            let _ = std::fs::remove_file(log_path);
        }
    }
}

#[cfg(windows)]
fn redirect_stderr(log_path: &std::path::Path) {
    use std::os::windows::io::IntoRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    // Open with append so multi-threaded WriteFile calls are atomic per
    // Windows's FILE_APPEND_DATA semantic — no need for our own mutex.
    let file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    // Hand ownership of the HANDLE to the OS via SetStdHandle. The File
    // is consumed by into_raw_handle so its drop won't double-close.
    let raw_handle = file.into_raw_handle();
    unsafe {
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(raw_handle as *mut std::ffi::c_void));
        // Also redirect stdout so any println!/dbg! lands in the same
        // file — handle is shared, append-mode keeps writes atomic.
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(raw_handle as *mut std::ffi::c_void));
    }
}

#[cfg(not(windows))]
fn redirect_stderr(_: &std::path::Path) {
    // No-op on Linux: terminal stderr is already user-visible from
    // launchers like the AppImage's --stderr or running the bin directly.
}

fn install_panic_hook(crash_log_path: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log_path)
        {
            let _ = writeln!(f, "\n=== Crash {} ===", timestamp_str());
            let _ = writeln!(f, "Version: {}", env!("CARGO_PKG_VERSION"));
            let _ = writeln!(f, "Thread:  {:?}", std::thread::current().name());
            let _ = writeln!(f, "Payload: {}", info);
            // force_capture defeats RUST_BACKTRACE=0 so we always get a trace
            // in the crash log even if the user hasn't set the env var.
            let bt = std::backtrace::Backtrace::force_capture();
            let _ = writeln!(f, "Backtrace:\n{}", bt);
            let _ = f.flush();
        }
        // Chain to the default hook so the panic also lands in stderr
        // (which is now the log file on Windows, or terminal on Linux).
        default_hook(info);
    }));
}
