#![deny(clippy::all)]

//! The Electron addon: a thin napi layer over `decibell-core`
//! (crates/decibell-core). This crate owns `init` / `shutdown` / `ping`, the
//! napi event sink (`napi_sink.rs`) and the `#[napi]` commands, each a
//! one-line wrapper over `decibell_core::api` (where the bodies, and the
//! argument / result structs, live); everything they drive lives in the core.

#[macro_use]
extern crate napi_derive;

mod commands;
mod napi_sink;

pub(crate) use decibell_core::{api, state};

use decibell_core::api::lifecycle::InitOptions;
use napi::JsFunction;

/// Runs once when Node loads the addon, before any command. Stands up
/// the AppState holder so `state::shared()` is callable. Anything
/// requiring the EventBus or paths waits for `init(opts, bus)`.
#[napi::module_init]
fn on_load() {
    // Electron sets this child process's stdout/stderr pipes to
    // non-blocking. Under bursty native logging (e.g. the encoder thread's
    // per-keyframe diagnostics) the pipe fills, writes return EAGAIN, and
    // `eprintln!`/`println!` PANIC ("failed printing to stderr: Resource
    // temporarily unavailable") — which silently killed the encode thread.
    // Clear O_NONBLOCK so writes block briefly (Electron drains the pipe)
    // instead of panicking. Unix-only; harmless if the flag's already clear.
    #[cfg(unix)]
    unsafe {
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags != -1 {
                libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
            }
        }
    }
    state::init();
}

// (The doc comment below is copied into index.d.ts by napi: keep it as is.
// "The EventBus TSFN" is now the napi event sink, `napi_sink::NapiSink`, over
// all three callbacks, installed into decibell-core's `events`. The rest of
// the start-up is the core's, `api::lifecycle::init`.)
/// Called once from Electron main after `app.whenReady()` and the
/// first BrowserWindow has been created. Installs the EventBus TSFN
/// (so any thread can `events::send(...)`) and stashes BootConfig so
/// any module can read paths via `state::boot()`.
///
/// Sync, not async — the JsFunction handle has to be turned into a
/// ThreadsafeFunction before any await point.
#[napi]
pub fn init(
    opts: InitOptions,
    bus: JsFunction,
    stream_bus: JsFunction,
    stream_thumbnail_bus: JsFunction,
) -> napi::Result<()> {
    // `bus` carries the named JSON events. PR7c: `stream_bus` is a
    // dedicated TSFN for encoded video frames. Carries Buffer payloads
    // (zero-copy view over Vec<u8>) instead of base64+JSON through the
    // main bus. ~3 MB/s saved per stream at 60fps. Same idea for
    // per-stream JPEG thumbnails (`stream_thumbnail_bus`) — raw Buffer
    // instead of a base64-encoded `data:image/jpeg;base64,…` string
    // riding the JSON bus. Renderer wraps the bytes in a blob: URL.
    let sink = napi_sink::NapiSink::new(bus, stream_bus, stream_thumbnail_bus)?;

    // env_logger so log::warn! / log::info! land in the Electron main
    // stdout. Defaults to `info`; override with RUST_LOG. Idempotent:
    // try_init silently no-ops if a previous init() call already set
    // it (HMR reload-the-addon scenarios). The host's job, not the core's:
    // set up before the core starts so its start-up logs land too.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init();

    // napi runs sync functions inside its tokio runtime's context
    // (`within_runtime_if_available`), so this is napi's runtime: every
    // command already runs on it, and the core spawns onto it from threads
    // outside it.
    api::lifecycle::init(opts, std::sync::Arc::new(sink), tokio::runtime::Handle::current());
    Ok(())
}

/// Called from Electron main on `before-quit`. Drops engines, joins
/// long-lived threads, releases TLS connections so the process can
/// exit cleanly.
///
/// Without this, closing the BrowserWindow leaves the main process
/// hanging — the native tokio runtime holds a never-ending thumbnail
/// task, the capture + encoder std::threads sit in their poll loops,
/// and napi-rs's tokio runtime won't drop until those exit. The user
/// then has to kill `decibell.exe` from Task Manager.
///
/// Resetting AppState to default explicitly drops each engine; their
/// `Drop` impls handle the actual teardown:
///   - VideoEngine::stop_windows() joins capture + encoder threads
///     and aborts the thumbnail tokio task
///   - AudioStreamEngine::stop() joins the share-audio thread
///   - VoiceEngine::stop() joins audio + video-recv threads and
///     aborts the event bridge
///   - CommunityClient / CentralClient drops close their TLS sockets
#[napi]
pub async fn shutdown() -> napi::Result<()> {
    api::lifecycle::shutdown().await.map_err(napi::Error::from)
}

// ── Diagnostic exports ────────────────────────────────────────────
// These live in lib.rs deliberately rather than in commands/ —
// they're not part of the application surface, they're smoke tests.
// Any new application command goes in commands/<module>.rs.

#[napi]
pub fn ping() -> String {
    "pong".to_string()
}

#[napi]
pub async fn ping_async() -> napi::Result<String> {
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    Ok("pong-async".to_string())
}
