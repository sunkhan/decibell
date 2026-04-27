use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

use crate::media::caps::CodecCap;
use crate::media::{VoiceEngine, VideoEngine, AudioStreamEngine};
use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;
use crate::net::proto::InviteResolveResponse;

#[derive(Default)]
pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub credentials: Option<(String, String)>,
    pub voice_engine: Option<VoiceEngine>,
    pub video_engine: Option<VideoEngine>,
    pub audio_stream_engine: Option<AudioStreamEngine>,
    pub connected_voice_server: Option<String>,
    pub connected_voice_channel: Option<String>,
    /// Persisted mute/deafen state across voice disconnects so the user
    /// stays muted/deafened when reconnecting to another channel.
    pub voice_muted: bool,
    pub voice_deafened: bool,
    pub voice_muted_before_deafen: bool,
    /// Stop signal for the temporary mic test capture (settings UI level meter).
    /// Setting this to true stops the test. None means no test is running.
    pub mic_test_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// In-flight invite-resolve lookups keyed by invite code (uppercased).
    /// Router fulfils each sender when INVITE_RESOLVE_RES arrives; the caller
    /// drops its half after timeout.
    pub pending_invite_resolves: HashMap<String, oneshot::Sender<InviteResolveResponse>>,
    /// Upload/download throttle rates in bytes per second. Both default to 0
    /// (unlimited). Shared via Arc<AtomicU64> so in-flight transfers pick up
    /// slider changes mid-stream without restart.
    pub upload_limit_bps: Arc<std::sync::atomic::AtomicU64>,
    pub download_limit_bps: Arc<std::sync::atomic::AtomicU64>,
    /// In-flight uploads keyed by client-side pending id. Lets the UI cancel
    /// mid-stream and the upload task to poll the flag between sub-chunks.
    pub active_uploads: HashMap<String, Arc<AtomicBool>>,
    /// Local HTTP media server's port. Powers `<video>`/`<audio>` playback —
    /// WebKitGTK's GStreamer pipeline can't consume custom URI schemes
    /// (asset://) or cross-origin file:// URLs, but it handles
    /// http://127.0.0.1:PORT/... cleanly via souphttpsrc, with proper
    /// Range support for seeking. Set once at app init; 0 if startup
    /// failed (in which case media playback falls back to errors).
    pub local_media_port: u16,
    /// WebCodecs decoder capabilities probed in the React layer and
    /// shipped here via the `set_decoder_caps` command at app boot
    /// (and again on Settings → Refresh). Empty until React calls in.
    /// Encode caps live separately in caps.rs's static cache because
    /// they don't change during a session unless explicitly refreshed.
    pub decoder_caps: Vec<CodecCap>,
}

pub type SharedState = Arc<Mutex<AppState>>;
