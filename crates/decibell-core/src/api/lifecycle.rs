//! Starting and stopping the core: what every client does once, first and
//! last. The Electron addon's `init` / `shutdown` (electron-client/native/
//! src/lib.rs) build their napi event sink and call these.

use std::sync::Arc;

use crate::api::CoreError;
use crate::events::{self, EventSink};
use crate::{config, media, state};

macro_rules! init_options {
    ($(#[$local_key_attr:meta])*) => {
        /// Boot-time options pushed in from Electron main. Everything platform-
        /// path-shaped (userData) is resolved Node-side via
        /// `app.getPath()` and shipped here so Rust never has to figure out
        /// platform-specific dirs itself — the Electron main process is the
        /// authority.
        #[cfg_attr(feature = "napi", napi_derive::napi(object))]
        pub struct InitOptions {
            pub user_data_dir: String,
            pub app_version: String,
            /// base64 of a random 32-byte key that Electron main keeps wrapped by
            /// `safeStorage` — the at-rest key for the E2EE key store. Absent when
            /// safeStorage isn't available on this machine (config.rs derivation
            /// is used instead). Named explicitly: napi's camel-casing splits on
            /// the digit (`e2EeLocalKey`), so main's `e2eeLocalKey` was never read
            /// and every store fell back to the derived key.
            $(#[$local_key_attr])*
            pub e2ee_local_key: Option<String>,
            /// True when that key is wrapped by a real OS keychain — false (or
            /// absent) for Linux's `basic_text` safeStorage backend, which wraps
            /// with a hard-coded password. The saved sign-in uses the key only
            /// when it is (config.rs).
            pub local_key_protected: Option<bool>,
        }
    };
}

// The one field attribute in the api: napi's `#[napi(object)]` reads field
// attributes before `cfg_attr` on them is expanded, so the attribute goes in
// whole (with the feature) or not at all.
#[cfg(feature = "napi")]
init_options!(#[napi(js_name = "e2eeLocalKey")]);
#[cfg(not(feature = "napi"))]
init_options!();

/// Start the core: once, before any other call. `runtime` is the tokio
/// runtime the core runs on (`crate::rt`: calls from outside a runtime go
/// there); `sink` receives every event, from any core thread. Then the
/// at-rest key, the boot paths, the TLS crypto provider (and on Windows the
/// pinned AMF runtime). The host installs its own logger (`log` facade).
///
/// The state holder ([`state::init`]) is set up here too, for hosts without a
/// load hook (the addon already did it when Node loaded it; it's idempotent).
pub fn init(opts: InitOptions, sink: Arc<dyn EventSink>, runtime: tokio::runtime::Handle) {
    crate::rt::init(runtime);
    state::init();
    events::install_sink(sink);
    let e2ee_local_key = opts.e2ee_local_key.as_deref().and_then(|b64| {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        if bytes.len() != 32 {
            log::warn!("[e2ee] ignoring at-rest key of {} bytes", bytes.len());
            return None;
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&bytes);
        Some(k)
    });
    config::set_local_key_protected(
        e2ee_local_key.is_some() && opts.local_key_protected.unwrap_or(false),
    );
    state::set_boot(state::BootConfig {
        user_data_dir: opts.user_data_dir.into(),
        app_version: opts.app_version,
        e2ee_local_key,
    });

    // rustls TLS-1.2/1.3 cipher provider. Required before any
    // ClientConfig::with_root_certificates / TlsConnector usage —
    // rustls 0.23 dropped the default-set behavior. Idempotent.
    let _ = rustls::crypto::ring::default_provider().install_default();

    #[cfg(target_os = "windows")]
    pin_amf_runtime();

    // PR8: FFmpeg removed from the addon entirely. Encode lives in the
    // renderer (Chromium WebCodecs); decode lives there too. The native
    // side just packetises encoded chunks onto UDP and doesn't link
    // against libavcodec at all anymore. Removed: `ffmpeg_next::init()`
    // and the libavcodec runtime-version diagnostic.
}

/// Load AMD's AMF runtime once and never free it.
///
/// Every AMF encoder probe/open loads amfrt64.dll (FFmpeg's
/// hwcontext_amf dlopen) and `amf_device_uninit` unloads it again. A
/// driver/AMF worker thread that outlives AMFContext::Terminate then
/// executes in unmapped code — the classic `<Unloaded_amfrt64.dll>`
/// access violation. Holding our own reference keeps the module mapped for
/// the life of the process (OBS does the same: load once, never free).
///
/// Same search flags FFmpeg's win32_dlopen uses (application dir +
/// System32), so we pin exactly the module FFmpeg will resolve, and never
/// pick a planted copy from the current directory / PATH. Missing on
/// non-AMD machines — that's fine, nothing to pin. Declared by hand
/// because the windows crate's LibraryLoader feature isn't enabled.
#[cfg(target_os = "windows")]
fn pin_amf_runtime() {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(
            lp_lib_file_name: *const u16,
            h_file: *mut std::ffi::c_void,
            dw_flags: u32,
        ) -> *mut std::ffi::c_void;
    }
    const LOAD_LIBRARY_SEARCH_APPLICATION_DIR: u32 = 0x0000_0200;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    static PINNED: std::sync::Once = std::sync::Once::new();
    PINNED.call_once(|| {
        let name: Vec<u16> = "amfrt64.dll\0".encode_utf16().collect();
        // The module handle is intentionally leaked (never FreeLibrary'd).
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            log::info!("[amf] amfrt64.dll not present (no AMD driver) — nothing to pin");
        } else {
            log::info!("[amf] amfrt64.dll pinned for the process lifetime");
        }
    });
}

/// Stop the core: drops engines, joins long-lived threads, releases TLS
/// connections so the process can exit cleanly.
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
pub async fn shutdown() -> Result<(), CoreError> {
    crate::rt::run(async move {
        let state_arc = state::shared();
        let mut s = state_arc.lock().await;

        // A DM call in progress: tell the peer we're gone (best effort, short
        // timeout) so their side ends now instead of after the 15 s watchdog,
        // and stop the punch / watchdog.
        s.pending_call = None;
        if let Some(ac) = s.active_call.take() {
            ac.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(h) = ac.watchdog {
                h.abort();
            }
            let hangup = s.central.as_ref().and_then(|c| c.connection_write_tx()).map(|tx| {
                use crate::net::proto::{call_signal, packet, CallSignal};
                let pkt = crate::net::connection::build_packet(
                    packet::Type::CallSignal,
                    packet::Payload::CallSignal(CallSignal {
                        kind: call_signal::Kind::Hangup as i32,
                        call_id: ac.call_id.clone(),
                        from: String::new(),
                        to: ac.peer.clone(),
                        pub_key: Vec::new(),
                        candidates: Vec::new(),
                        stream: None,
                        timestamp: 0,
                        pub_key_sig: Vec::new(),
                        key_id: 0,
                        from_uid: 0,
                        to_uid: ac.peer_uid,
                    }),
                    s.token.as_deref(),
                );
                (tx, pkt)
            });
            if let Some((tx, pkt)) = hangup {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(500), tx.send(pkt)).await;
            }
        }
        media::watched_streams_clear();

        // Drop in reverse start-up order: media engines first (they may
        // still be pushing packets through the voice socket), then voice
        // (owns the sockets), then network connections.
        s.video_engine = None;
        s.audio_stream_engine = None;
        s.voice_engine = None;
        s.communities.clear();
        s.central = None;
        s.credentials = None;
        s.pending_invite_resolves.clear();
        s.pending_friends_in_community.clear();

        Ok(())
    })
    .await
}
