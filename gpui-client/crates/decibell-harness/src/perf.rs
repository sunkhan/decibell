//! Startup and frame instrumentation. Always compiled, and free while nothing reads it: two
//! atomics GPUI bumps per frame (`patches/0002-gpui-frame-counters.patch`), one next-frame
//! callback for the first frame, no timer and no thread.
//!
//! With `DECIBELL_PERF=1` or `--perf` ([`enable_report`]) the process prints, on stderr:
//!
//! ```text
//! decibell: first-frame 87.4 ms since start monotonic_ns=5123456789
//! decibell: frames drawn=3 presented=3          (on SIGUSR1, and at exit)
//! ```
//!
//! `first-frame` is the `--ready-marker` for `tools/perf/startup.sh`
//! (`'first-frame .* monotonic_ns=(?P<ns>\d+)'` makes it exact). "Since start" counts from the
//! top of `main`: about a millisecond after the process was spawned.

use std::io::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{FRAMES_DRAWN, FRAMES_PRESENTED, Window};

static START: OnceLock<Instant> = OnceLock::new();
/// Microseconds from [`start`] to the first presented frame, plus one; 0 until then.
static FIRST_FRAME_MICROS: AtomicU64 = AtomicU64::new(0);
static REPORT: AtomicBool = AtomicBool::new(false);

/// Call first thing in `main`.
pub fn start() {
    START.get_or_init(Instant::now);
}

pub fn since_start() -> Duration {
    START.get_or_init(Instant::now).elapsed()
}

/// Turns on the stderr lines described in the module docs.
pub fn enable_report() {
    REPORT.store(true, Ordering::Relaxed);
    #[cfg(unix)]
    // Safety: the handler only reads atomics and calls write(2).
    unsafe {
        libc::signal(libc::SIGUSR1, on_sigusr1 as *const () as libc::sighandler_t);
    }
}

/// Records when `window`'s first frame reached the screen. Call once, when the window opens.
///
/// GPUI has no "presented" event. A next-frame callback that runs after the window has
/// presented is the closest: on Wayland it is the compositor's frame callback for that
/// present, elsewhere the first frame request after it (one refresh interval at most).
pub fn watch_first_frame(window: &Window) {
    watch_first_frame_attempt(window, 0);
}

fn watch_first_frame_attempt(window: &Window, attempt: u32) {
    window.on_next_frame(move |window, _| {
        // The first request is the one that presents; a few retries cover a present that
        // was put off, without ever turning into a loop of frame requests.
        if window.frames_presented() == 0 && attempt < 8 {
            watch_first_frame_attempt(window, attempt + 1);
            return;
        }
        let elapsed = since_start();
        if FIRST_FRAME_MICROS
            .compare_exchange(
                0,
                elapsed.as_micros() as u64 + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
            && REPORT.load(Ordering::Relaxed)
        {
            let ms = elapsed.as_secs_f64() * 1e3;
            match monotonic_ns() {
                Some(ns) => print(format_args!(
                    "decibell: first-frame {ms:.1} ms since start monotonic_ns={ns}"
                )),
                None => print(format_args!("decibell: first-frame {ms:.1} ms since start")),
            }
        }
    });
}

/// Time from [`start`] to the first presented frame, once there was one.
pub fn first_frame() -> Option<Duration> {
    match FIRST_FRAME_MICROS.load(Ordering::Relaxed) {
        0 => None,
        micros => Some(Duration::from_micros(micros - 1)),
    }
}

/// `(drawn, presented)` over every window since the process started. Per window:
/// `Window::frames_drawn` / `Window::frames_presented`.
pub fn frames() -> (u64, u64) {
    (
        FRAMES_DRAWN.load(Ordering::Relaxed),
        FRAMES_PRESENTED.load(Ordering::Relaxed),
    )
}

/// Prints the frame counters if reporting is on. Call when the app quits.
pub fn report_frames() {
    if REPORT.load(Ordering::Relaxed) {
        let (drawn, presented) = frames();
        print(format_args!(
            "decibell: frames drawn={drawn} presented={presented}"
        ));
    }
}

/// How often the calling thread has blocked and been woken since it started (voluntary context
/// switches). Call it on the main thread: at idle the difference over a few seconds is the
/// event loop's wakeup count. `None` where the kernel doesn't say (anything but Linux).
pub fn thread_wakeups() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/thread-self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))?
        .trim()
        .parse()
        .ok()
}

fn print(line: std::fmt::Arguments) {
    // A closed stderr must not take the app down, which eprintln! would.
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

#[cfg(unix)]
fn monotonic_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: `ts` is a valid timespec to write to.
    (unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0)
        .then(|| ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

#[cfg(not(unix))]
fn monotonic_ns() -> Option<u64> {
    None
}

/// Writes `decibell: frames drawn=N presented=M\n` with nothing but atomics, stack memory and
/// write(2): the only things a signal handler may use.
#[cfg(unix)]
extern "C" fn on_sigusr1(_: libc::c_int) {
    let mut buffer = [0u8; 96];
    let mut len = 0;
    let mut push = |bytes: &[u8]| {
        buffer[len..len + bytes.len()].copy_from_slice(bytes);
        len += bytes.len();
    };
    push(b"decibell: frames drawn=");
    push_decimal(&mut push, FRAMES_DRAWN.load(Ordering::Relaxed));
    push(b" presented=");
    push_decimal(&mut push, FRAMES_PRESENTED.load(Ordering::Relaxed));
    push(b"\n");
    // Safety: `buffer[..len]` is initialised memory owned by this frame.
    unsafe { libc::write(2, buffer.as_ptr().cast(), len) };
}

#[cfg(unix)]
fn push_decimal(push: &mut impl FnMut(&[u8]), mut value: u64) {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    push(&digits[start..]);
}
