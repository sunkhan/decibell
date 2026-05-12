//! Process-global state.
//!
//! Two layers:
//!   - `BootConfig` is set once from `init(opts, bus)` and never changes.
//!     Anything that needs `userDataDir` / `appVersion` reads it without
//!     locking.
//!   - `AppState` is the mutable engine state (network clients, voice/video
//!     engines, etc.). Wrapped in `Arc<tokio::sync::Mutex<_>>` so commands
//!     can hold the lock across `.await` points (TLS writes, SQLite, etc.).
//!     Mirrors the existing tauri-client `state.rs` shape — fields land here
//!     as the corresponding modules port over.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::media::caps::{CodecCap, PeerCaps};
use crate::media::{AudioStreamEngine, VideoEngine, VoiceEngine};
use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;
use crate::net::proto::{
    FetchAvatarRes, InviteResolveResponse, UpdateAvatarRes,
};

/// Plan C: server-pushed event when a watcher starts or stops watching the
/// LOCAL user's stream. Distributed to the active video pipeline via a tokio
/// broadcast channel so the CodecSelector can recompute the LCD codec choice.
#[derive(Clone, Debug)]
pub struct WatcherEvent {
    pub channel_id: String,
    pub streamer_username: String,
    pub watcher_username: String,
    /// Mirrors chatproj::StreamWatcherNotify::Action — 1 = JOINED, 2 = LEFT.
    pub action: i32,
}

pub struct BootConfig {
    pub user_data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub app_version: String,
}

pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    /// Stored login credentials so a dropped central connection can
    /// silently re-authenticate. Cleared on logout. Persisted (encrypted)
    /// to userData via config.rs so app restart auto-logs-in.
    pub credentials: Option<(String, String)>,
    /// In-flight `resolve_invite_code` calls keyed by (uppercased)
    /// invite code. The central router fulfils each waiter when the
    /// matching INVITE_RESOLVE_RES arrives; the caller drops its half
    /// after a 5s timeout.
    pub pending_invite_resolves: HashMap<String, oneshot::Sender<InviteResolveResponse>>,
    /// Active voice channel session. None when not in a voice channel.
    /// Lifecycle: created in `join_voice_channel`, dropped in
    /// `leave_voice_channel` or on logout.
    pub voice_engine: Option<VoiceEngine>,
    /// Active screen-share encoding pipeline. None when not streaming.
    pub video_engine: Option<VideoEngine>,
    /// Active stream-audio encoding pipeline. None when not streaming
    /// (or streaming without share-audio).
    pub audio_stream_engine: Option<AudioStreamEngine>,
    pub connected_voice_server: Option<String>,
    pub connected_voice_channel: Option<String>,
    /// Persisted across voice disconnects so reconnecting to another
    /// channel keeps the user's mute/deafen state. Mirrored on the
    /// VoiceEngine when one is alive.
    pub voice_muted: bool,
    pub voice_deafened: bool,
    pub voice_muted_before_deafen: bool,
    /// Stop signal for the temporary mic-test capture (settings UI
    /// level meter). Setting this to true stops the test. None when
    /// no test is running.
    pub mic_test_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// WebCodecs decoder capabilities probed in the React layer at boot
    /// and shipped here via `set_decoder_caps`. PR8: encoder caps now
    /// also come from the renderer (WebCodecs.VideoEncoder.isConfigSupported)
    /// and live alongside decode caps; native-side FFmpeg encoder probing
    /// went away with the FFmpeg removal.
    pub decoder_caps: Vec<CodecCap>,
    pub encoder_caps: Vec<CodecCap>,
    /// Plan C: per-user encode + decode capability snapshot mirrored
    /// from VoicePresenceUpdate. Pipeline reads from here when a
    /// STREAM_WATCHER_NOTIFY arrives so the LCD picker can plug in
    /// the joining watcher's decode caps without an AppState round-trip.
    pub voice_caps_cache: Arc<RwLock<HashMap<String, PeerCaps>>>,
    /// Plan C: tokio broadcast channel for STREAM_WATCHER_NOTIFY events
    /// inbound from the community server. Pipeline subscribes once at
    /// startup, drops the AppState lock, and processes events without
    /// touching AppState again.
    pub watcher_event_tx: broadcast::Sender<WatcherEvent>,
    /// In-flight UPDATE_AVATAR_REQ — at most one upload at a time per
    /// session (the AccountTab disables its buttons during the round-
    /// trip). Replaced if a new upload starts; the previous oneshot
    /// resolves to nothing and the renderer-side .await times out
    /// at 5s.
    pub pending_avatar_update: Option<oneshot::Sender<UpdateAvatarRes>>,
    /// In-flight FETCH_AVATAR_REQ calls keyed by target username. The
    /// central router resolves each waiter when the matching
    /// FETCH_AVATAR_RES arrives; the caller drops its half after a 5s
    /// timeout.
    pub pending_avatar_fetches: HashMap<String, oneshot::Sender<FetchAvatarRes>>,
}

impl Default for AppState {
    fn default() -> Self {
        let (watcher_event_tx, _) = broadcast::channel(64);
        Self {
            central: None,
            communities: HashMap::new(),
            username: None,
            token: None,
            credentials: None,
            pending_invite_resolves: HashMap::new(),
            voice_engine: None,
            video_engine: None,
            audio_stream_engine: None,
            connected_voice_server: None,
            connected_voice_channel: None,
            voice_muted: false,
            voice_deafened: false,
            voice_muted_before_deafen: false,
            mic_test_stop: None,
            decoder_caps: Vec::new(),
            encoder_caps: Vec::new(),
            voice_caps_cache: Arc::new(RwLock::new(HashMap::new())),
            watcher_event_tx,
            pending_avatar_update: None,
            pending_avatar_fetches: HashMap::new(),
        }
    }
}

static BOOT: OnceLock<BootConfig> = OnceLock::new();
static APP: OnceLock<Arc<Mutex<AppState>>> = OnceLock::new();

/// Called once from `#[napi::module_init]`. Allocates the AppState
/// holder; BootConfig is filled in later by `init(opts, bus)`.
pub fn init() {
    let _ = APP.set(Arc::new(Mutex::new(AppState::default())));
}

pub fn set_boot(cfg: BootConfig) {
    let _ = BOOT.set(cfg);
}

pub fn boot() -> &'static BootConfig {
    BOOT.get().expect("state::set_boot has not been called")
}

pub fn shared() -> Arc<Mutex<AppState>> {
    APP.get()
        .expect("state::init has not been called")
        .clone()
}
