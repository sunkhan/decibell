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
    install_panic_hook(crash_path.clone());

    #[cfg(windows)]
    install_seh_filter(crash_path);

    eprintln!();
    eprintln!("=== Decibell startup {} ===", timestamp_str());
    eprintln!("[logging] log dir: {}", dir.display());
    eprintln!("[logging] version: {}", env!("CARGO_PKG_VERSION"));
    log_ffmpeg_version();
}

/// Print the FFmpeg runtime version + libavutil/libavcodec major versions
/// so dev vs release behaviour differences pinpoint to the actual DLLs
/// loaded. avutil-60 / avcodec-62 = FFmpeg 8.x; avutil-59 / avcodec-61 =
/// FFmpeg 7.x. Surfaces silently on platforms where ffmpeg isn't linked.
fn log_ffmpeg_version() {
    use std::ffi::CStr;
    unsafe {
        let raw = ffmpeg_next::sys::av_version_info();
        if !raw.is_null() {
            if let Ok(s) = CStr::from_ptr(raw).to_str() {
                eprintln!("[ffmpeg] runtime version: {}", s);
            }
        }
        eprintln!(
            "[ffmpeg] libavutil={} libavcodec={} libavformat={}",
            ffmpeg_next::sys::avutil_version() >> 16,
            ffmpeg_next::sys::avcodec_version() >> 16,
            ffmpeg_next::sys::avformat_version() >> 16,
        );
    }
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

/// Windows-only: catch unhandled SEH (Structured Exception Handling)
/// exceptions and write a one-liner to crash.log before the OS terminates
/// the process. This lets us diagnose access violations (0xc0000005) and
/// other native crashes from C dependencies (FFmpeg, NVIDIA, D3D11) that
/// Rust's panic hook can't see.
///
/// Filter constraints: runs in the context of the crashing thread with
/// the stack possibly corrupted, so we keep work minimal — open file,
/// format a single record, close, return EXCEPTION_CONTINUE_SEARCH so
/// Windows still produces the usual WerFault dump.
#[cfg(windows)]
fn install_seh_filter(crash_log_path: PathBuf) {
    use windows::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };

    // Path for the filter to consume. Leak so the static box outlives the
    // filter callback (it runs at any time during the process lifetime).
    let path_static: &'static PathBuf = Box::leak(Box::new(crash_log_path));

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        let _ = std::panic::catch_unwind(|| {
            // The leaked path is the only state we need. Pull it from the
            // global the install function set up — std::sync::OnceLock
            // would be cleaner but adds atomic ordering complexity in a
            // crash context, so we use a plain static mut populated once.
            let path = match SEH_CRASH_PATH.get() {
                Some(p) => p,
                None => return,
            };
            let mut f = match OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                Ok(f) => f,
                Err(_) => return,
            };
            let _ = writeln!(f, "\n=== SEH crash {} ===", timestamp_str());
            let _ = writeln!(f, "Version: {}", env!("CARGO_PKG_VERSION"));
            let _ = writeln!(f, "Thread:  {:?}", std::thread::current().name());
            if !info.is_null() {
                let er = (*info).ExceptionRecord;
                if !er.is_null() {
                    let er = &*er;
                    let _ = writeln!(f, "Code:    0x{:08X}", er.ExceptionCode.0 as u32);
                    let _ = writeln!(f, "Address: 0x{:016X}", er.ExceptionAddress as usize);
                    let _ = writeln!(f, "Flags:   0x{:08X}", er.ExceptionFlags);
                }
            }
            let _ = f.flush();
        });
        EXCEPTION_CONTINUE_SEARCH
    }

    let _ = SEH_CRASH_PATH.set(path_static.clone());
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
}

#[cfg(windows)]
static SEH_CRASH_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

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
