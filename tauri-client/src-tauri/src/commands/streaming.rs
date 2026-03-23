use tauri::{AppHandle, State};
use crate::state::SharedState;
use crate::media::{capture, encoder::EncoderConfig, VideoEngine};

#[tauri::command]
pub async fn list_capture_sources() -> Result<Vec<capture::CaptureSource>, String> {
    capture::list_sources().await
}

#[tauri::command]
pub async fn start_screen_share(
    server_id: String,
    channel_id: String,
    source_id: String,
    resolution: String,
    fps: u32,
    quality: String,
    share_audio: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Check not already streaming
    {
        let s = state.lock().await;
        if s.video_engine.is_some() {
            return Err("Already sharing screen".to_string());
        }
        if s.voice_engine.is_none() {
            return Err("Must be in a voice channel to share screen".to_string());
        }
    }

    // Resolve resolution
    let (width, height) = match resolution.as_str() {
        "720p" => (1280u32, 720u32),
        "source" => (0, 0),
        _ => (1920, 1080),
    };

    let bitrate_kbps: u32 = match quality.as_str() {
        "low" => 1500,
        "medium" => 3000,
        _ => 6000,
    };

    // Start capture (triggers portal picker dialog on Linux, blocks until user selects)
    let capture_config = capture::CaptureConfig {
        target_fps: fps,
        target_width: width,
        target_height: height,
    };
    let frame_rx = capture::start_capture(&source_id, &capture_config).await?;

    // Lock state to get socket + sender_id and set up pipeline
    let mut s = state.lock().await;

    // Re-check after portal dialog (user may have disconnected)
    let voice_engine = s.voice_engine.as_ref()
        .ok_or("Voice channel disconnected during screen selection")?;
    let socket = voice_engine.socket();
    let sender_id = voice_engine.sender_id().to_string();

    // Notify community server
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.start_stream(&channel_id, fps as i32, bitrate_kbps as i32, share_audio, width, height).await?;

    // Encoder config — use source resolution if 0x0
    let enc_width = if width == 0 { 1920 } else { width };
    let enc_height = if height == 0 { 1080 } else { height };
    let encoder_config = EncoderConfig {
        width: enc_width,
        height: enc_height,
        fps,
        bitrate_kbps,
        keyframe_interval_secs: 2,
    };

    // Start video pipeline
    let video_engine = VideoEngine::start(
        frame_rx,
        socket,
        sender_id,
        encoder_config,
        fps,
        app,
    );

    s.video_engine = Some(video_engine);

    Ok(())
}

#[tauri::command]
pub async fn stop_screen_share(
    server_id: String,
    channel_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // Stop video engine if running
    if let Some(mut engine) = s.video_engine.take() {
        engine.stop();
    }

    // Notify community server
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.stop_stream(&channel_id).await
}

#[tauri::command]
pub async fn watch_stream(
    server_id: String,
    channel_id: String,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.watch_stream(&channel_id, &target_username).await
}

#[tauri::command]
pub async fn stop_watching(
    server_id: String,
    channel_id: String,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.stop_watching(&channel_id, &target_username).await
}

#[tauri::command]
pub async fn request_keyframe(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.video_engine {
        engine.force_keyframe();
        Ok(())
    } else {
        Err("No active video engine".to_string())
    }
}
