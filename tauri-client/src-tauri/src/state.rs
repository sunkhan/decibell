use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::media::{VoiceEngine, VideoEngine, AudioStreamEngine};
use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;

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
    /// Stop signal for the temporary mic test capture (settings UI level meter).
    /// Setting this to true stops the test. None means no test is running.
    pub mic_test_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub type SharedState = Arc<Mutex<AppState>>;
