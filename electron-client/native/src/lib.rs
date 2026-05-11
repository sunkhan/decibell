#![deny(clippy::all)]

#[macro_use]
extern crate napi_derive;

mod commands;
mod config;
mod events;
mod media;
mod net;
mod state;

use napi::JsFunction;

/// Runs once when Node loads the addon, before any command. Stands up
/// the AppState holder so `state::shared()` is callable. Anything
/// requiring the EventBus or paths waits for `init(opts, bus)`.
#[napi::module_init]
fn on_load() {
    state::init();
}

/// Boot-time options pushed in from Electron main. Everything platform-
/// path-shaped (userData, cache) is resolved Node-side via
/// `app.getPath()` and shipped here so Rust never has to figure out
/// platform-specific dirs itself — the Electron main process is the
/// authority.
#[napi(object)]
pub struct InitOptions {
    pub user_data_dir: String,
    pub cache_dir: String,
    pub app_version: String,
}

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
    events::install(bus)?;
    // PR7c: dedicated TSFN for encoded video frames. Carries Buffer
    // payloads (zero-copy view over Vec<u8>) instead of base64+JSON
    // through the main bus. ~3 MB/s saved per stream at 60fps.
    events::install_stream_bus(stream_bus)?;
    // Same idea for per-stream JPEG thumbnails — raw Buffer instead of
    // a base64-encoded `data:image/jpeg;base64,…` string riding the
    // JSON bus. Renderer wraps the bytes in a blob: URL.
    events::install_stream_thumbnail_bus(stream_thumbnail_bus)?;
    state::set_boot(state::BootConfig {
        user_data_dir: opts.user_data_dir.into(),
        cache_dir: opts.cache_dir.into(),
        app_version: opts.app_version,
    });

    // env_logger so log::warn! / log::info! land in the Electron main
    // stdout. Defaults to `info`; override with RUST_LOG. Idempotent:
    // try_init silently no-ops if a previous init() call already set
    // it (HMR reload-the-addon scenarios).
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init();

    // rustls TLS-1.2/1.3 cipher provider. Required before any
    // ClientConfig::with_root_certificates / TlsConnector usage —
    // rustls 0.23 dropped the default-set behavior. Idempotent.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // PR8: FFmpeg removed from the addon entirely. Encode lives in the
    // renderer (Chromium WebCodecs); decode lives there too. The native
    // side just packetises encoded chunks onto UDP and doesn't link
    // against libavcodec at all anymore. Removed: `ffmpeg_next::init()`
    // and the libavcodec runtime-version diagnostic.

    Ok(())
}

/// Called from Electron main on `before-quit`. Drops engines, joins
/// long-lived threads, releases the EventBus TSFN. PR2 has nothing to
/// tear down yet — bodies grow as engines port.
#[napi]
pub async fn shutdown() -> napi::Result<()> {
    Ok(())
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
