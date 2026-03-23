use tauri::State;
use crate::state::SharedState;
use crate::media::capture;

#[tauri::command]
pub async fn list_capture_sources() -> Result<Vec<capture::CaptureSource>, String> {
    capture::list_sources().await
}

#[tauri::command]
pub async fn start_screen_share(
    server_id: String,
    channel_id: String,
    _source_id: String,
    resolution: String,
    fps: u32,
    quality: String,
    share_audio: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;

    if s.video_engine.is_some() {
        return Err("Already sharing screen".to_string());
    }

    // Resolve resolution
    let (width, height) = match resolution.as_str() {
        "720p" => (1280u32, 720u32),
        "source" => (0, 0),
        _ => (1920, 1080),
    };

    let bitrate_kbps: i32 = match quality.as_str() {
        "low" => 1500,
        "medium" => 3000,
        _ => 6000,
    };

    // Notify community server
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.start_stream(&channel_id, fps as i32, bitrate_kbps, share_audio, width, height).await?;

    // NOTE: The actual capture + encode pipeline startup is a stub here.
    // The TCP signaling works (stream appears in active streams list), but
    // actual video frame capture/encoding requires the platform capture backends
    // to be fully implemented. When ready, this will:
    // 1. Call capture::start_capture(source_id, config) to get frame_rx
    // 2. Get Arc<UdpSocket> from the VoiceEngine (shared socket)
    // 3. Spawn video_pipeline::run_video_send_pipeline on a dedicated thread
    // 4. Store the VideoEngine handle in AppState

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
