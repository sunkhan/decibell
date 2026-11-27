//! Which panics this client survives.
//!
//! The release profile unwinds (`gpui-client/Cargo.toml`), as the Electron addon's does, so the
//! core's own recovery works: its guarded workers catch a panic, log it, tell the UI (a toast) and
//! end that piece of work cleanly, and the app carries on (the decoder, the encoder, the capture
//! threads, the voice and video receive loops, the encoder probe's test encodes). Every other
//! panic must still end the process, as it did when the profile aborted: a thread that died
//! silently would leave a half-alive app (a window that looks connected with nothing behind it).
//!
//! The panic hook (`decibell_release::crash`) cannot see who will catch a panic, so this crate
//! says it: [`here`] answers for the panicking thread, from
//! - a section of our own code run through [`catch`] (a `catch_unwind` that marks the thread while
//!   it runs: decibell-media's FFmpeg read / seek callbacks);
//! - a core thread whose whole body is guarded ([`CORE_GUARDED_THREADS`], by name: the core can't
//!   mark its own sections without a change to `crates/decibell-core`);
//! - the core's tokio runtime ([`RUNTIME_THREAD`]): tokio catches every task's panic, as it does in
//!   the addon. A task whose result nobody awaits is lost with it (Electron's addon behaves the same);
//!   one that `decibell_core::rt::run` awaits resumes the panic in its caller, which on the UI
//!   thread ends the process (`decibell-app`'s boot guard).
//!
//! Anything else (the UI thread, GPUI's workers, our own threads, the core's unguarded threads,
//! the audio callback threads of the sound libraries) is fatal: the hook writes the crash report
//! and aborts.

use std::cell::Cell;
use std::panic::AssertUnwindSafe;

/// Threads of `crates/decibell-core` whose body runs inside a `catch_unwind` that recovers (logs,
/// reports to the UI, ends the work): `media/mod.rs` (`decibell-audio`), `media/pipeline.rs`
/// (`decibell-voice-recv`), `media/video_recv_host.rs` (`decibell-video-recv`),
/// `media/encoder_thread.rs` (`decibell-encoder`, Windows), `media/encoder_thread_linux.rs`
/// (`decibell-encoder-linux`), `media/capture_wgc.rs` / `capture_dxgi.rs` (Windows capture).
/// Checked against the core by `core_threads_are_guarded` (a test that reads the sources).
pub const CORE_GUARDED_THREADS: &[&str] = &[
    "decibell-audio",
    "decibell-voice-recv",
    "decibell-video-recv",
    "decibell-encoder",
    "decibell-encoder-linux",
    "decibell-wgc-capture",
    "decibell-dxgi-capture",
];

/// The name of every thread of the core's tokio runtime (`decibell_model::core::build_runtime`:
/// its workers and its blocking pool, where the encoder probe's guarded test encodes run).
pub const RUNTIME_THREAD: &str = "decibell-rt";

/// Who catches a panic on this thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Catcher {
    /// A section of ours run through [`catch`].
    Guarded,
    /// A guarded core thread ([`CORE_GUARDED_THREADS`]).
    CoreThread(&'static str),
    /// The core's runtime ([`RUNTIME_THREAD`]): tokio.
    Runtime,
}

impl Catcher {
    /// For a report's tags.
    pub fn as_str(self) -> &'static str {
        match self {
            Catcher::Guarded => "guarded",
            Catcher::CoreThread(_) => "core-thread",
            Catcher::Runtime => "runtime",
        }
    }
}

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// `catch_unwind`, telling the panic hook that a panic inside `f` is caught here (so it is
/// reported as recovered and the process carries on).
pub fn catch<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    let _leave = Leave;
    std::panic::catch_unwind(AssertUnwindSafe(f))
}

/// Who catches a panic raised on the current thread now (`None`: nothing does; the process ends).
pub fn here() -> Option<Catcher> {
    if DEPTH.with(Cell::get) > 0 {
        return Some(Catcher::Guarded);
    }
    let thread = std::thread::current();
    let name = thread.name()?;
    if name == RUNTIME_THREAD {
        return Some(Catcher::Runtime);
    }
    CORE_GUARDED_THREADS.iter().find(|t| **t == name).map(|t| Catcher::CoreThread(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_thread<R: Send + 'static>(name: &str, f: impl FnOnce() -> R + Send + 'static) -> R {
        std::thread::Builder::new().name(name.into()).spawn(f).unwrap().join().unwrap()
    }

    #[test]
    fn threads_and_sections() {
        assert_eq!(on_thread("decibell-video-recv", here), Some(Catcher::CoreThread("decibell-video-recv")));
        assert_eq!(on_thread("decibell-rt", here), Some(Catcher::Runtime));
        assert_eq!(on_thread("decibell-media", here), None);
        assert_eq!(on_thread("decibell-media", || catch(here).unwrap()), Some(Catcher::Guarded));
        assert_eq!(on_thread("decibell-media", || catch(|| catch(here).unwrap()).unwrap()), Some(Catcher::Guarded));
        // Out of the section again, also after a caught panic.
        assert_eq!(on_thread("decibell-media", || { let _ = catch(|| panic!("caught")); here() }), None);
    }

    /// Every name in [`CORE_GUARDED_THREADS`] is spawned by the core inside a `catch_unwind`, and
    /// every core thread spawned around a `catch_unwind` is in the list.
    #[test]
    fn core_threads_are_guarded() {
        let media = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/decibell-core/src/media");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&media).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // A thread name, then its body's first statements: a guarded body opens with the catch.
            for (at, _) in text.match_indices(".name(\"") {
                let name: String = text[at + 7..].chars().take_while(|c| *c != '"').collect();
                let body = &text[at..(at + 1600).min(text.len())];
                let guarded = body.find("catch_unwind").is_some_and(|c| body[..c].matches(".spawn(").count() == 1);
                if guarded {
                    found.push(name);
                }
            }
        }
        found.sort();
        let mut listed: Vec<String> = CORE_GUARDED_THREADS.iter().map(|s| s.to_string()).collect();
        listed.sort();
        assert_eq!(found, listed, "the core's guarded threads changed: update CORE_GUARDED_THREADS");
    }
}
