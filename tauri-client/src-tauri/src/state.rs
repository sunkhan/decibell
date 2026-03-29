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
}

pub type SharedState = Arc<Mutex<AppState>>;
