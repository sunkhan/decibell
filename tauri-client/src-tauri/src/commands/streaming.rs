use tauri::{AppHandle, State};
use crate::state::SharedState;
use crate::media::{capture, encoder::EncoderConfig, VideoEngine, AudioStreamEngine};
use crate::net::connection::build_packet;
use crate::net::proto::*;

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
    video_bitrate_kbps: Option<u32>,
    share_audio: bool,
    audio_bitrate_kbps: Option<u32>,
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

    let bitrate_kbps: u32 = video_bitrate_kbps.unwrap_or_else(|| match quality.as_str() {
        "low" => 3000,
        "medium" => 6000,
        _ => 10000,
    });

    eprintln!("[stream] start_screen_share: server='{}', channel='{}', source='{}', {}x{} @ {}fps",
        server_id, channel_id, source_id, width, height, fps);

    // Start capture (triggers portal picker dialog on Linux, blocks until user selects)
    let capture_config = capture::CaptureConfig {
        target_fps: fps,
        target_width: width,
        target_height: height,
    };
    let capture_output = capture::start_capture(&source_id, &capture_config).await?;
    let enc_width = capture_output.width;
    let enc_height = capture_output.height;

    // Lock state to extract needed data, set up engines, then drop before sending
    let mut s = state.lock().await;

    // Re-check after portal dialog (user may have disconnected)
    let voice_engine = s.voice_engine.as_ref()
        .ok_or("Voice channel disconnected during screen selection")?;
    let socket = voice_engine.socket();
    let sender_id = voice_engine.sender_id().to_string();

    // Get community connection write channel and build the start_stream packet
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    let start_stream_tx = client.connection_write_tx()
        .ok_or("Community connection lost")?;
    let start_stream_data = build_packet(
        packet::Type::StartStreamReq,
        packet::Payload::StartStreamReq(StartStreamRequest {
            channel_id: channel_id.clone().into(),
            target_fps: fps as i32,
            target_bitrate_kbps: bitrate_kbps as i32,
            has_audio: share_audio,
            resolution_width: enc_width,
            resolution_height: enc_height,
        }),
        Some(&client.jwt),
    );

    let encoder_config = EncoderConfig {
        width: enc_width,
        height: enc_height,
        fps,
        bitrate_kbps,
        keyframe_interval_secs: 2,
    };

    // Clone the community connection's write channel so the video event bridge
    // can send thumbnails without locking AppState (avoids Tokio deadlock).
    let thumbnail_write_tx = client.connection_write_tx();
    let thumbnail_channel_id = Some(channel_id.clone());

    // Start video pipeline
    let video_engine = VideoEngine::start(
        capture_output.receiver,
        #[cfg(target_os = "linux")]
        capture_output.gpu_receiver,
        socket.clone(),
        sender_id.clone(),
        encoder_config,
        fps,
        app.clone(),
        thumbnail_write_tx,
        thumbnail_channel_id,
    );

    // Wire up keyframe forwarding: voice bridge → video encoder
    if let Some(ref ve) = s.voice_engine {
        ve.set_keyframe_sender(video_engine.pipeline_control_tx());
    }

    s.video_engine = Some(video_engine);

    // Start audio stream capture if enabled
    if share_audio {
        let bitrate = audio_bitrate_kbps.unwrap_or(128);
        eprintln!("[stream] Starting audio capture for source '{}', bitrate={}kbps", source_id, bitrate);

        let is_window = is_window_source(&source_id);

        #[cfg(target_os = "linux")]
        {
            let (audio_rx, cleanup) = if is_window {
                crate::media::capture_audio_pipewire::start_system_audio_capture()?
            } else {
                crate::media::capture_audio_pipewire::start_system_audio_capture()?
            };
            let audio_engine = AudioStreamEngine::start(
                audio_rx,
                socket,
                sender_id,
                bitrate,
                app,
                Some(cleanup),
            );
            s.audio_stream_engine = Some(audio_engine);
        }

        #[cfg(target_os = "windows")]
        {
            let audio_rx = if is_window {
                let pid = get_pid_from_source_id(&source_id)?;
                crate::media::capture_audio_wasapi::start_process_audio_capture(pid)?
            } else {
                crate::media::capture_audio_wasapi::start_system_audio_capture()?
            };
            let audio_engine = AudioStreamEngine::start(
                audio_rx,
                socket,
                sender_id,
                bitrate,
                app,
            );
            s.audio_stream_engine = Some(audio_engine);
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            eprintln!("[stream] Audio capture not supported on this platform");
        }
    }

    drop(s); // Release lock BEFORE sending

    // Notify community server (outside lock)
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        start_stream_tx.send(start_stream_data),
    ).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

/// Check if a source_id refers to a window (vs a screen/monitor).
fn is_window_source(source_id: &str) -> bool {
    // Linux portal: always "portal" (could be screen or window — we can't distinguish)
    // Windows: monitors start with "monitor:", windows start with "window:"
    !source_id.starts_with("monitor:") && source_id != "portal"
}

/// Extract the PID from a window source_id on Windows.
/// Source IDs for windows are formatted as "window:<hwnd>" by capture_wgc.
#[cfg(target_os = "windows")]
fn get_pid_from_source_id(source_id: &str) -> Result<u32, String> {
    let hwnd_str = source_id
        .strip_prefix("window:")
        .ok_or_else(|| format!("Invalid window source_id: {}", source_id))?;
    let hwnd_val: isize = hwnd_str
        .parse()
        .map_err(|_| format!("Invalid HWND in source_id: {}", hwnd_str))?;

    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    use windows::Win32::Foundation::HWND;
    let hwnd = HWND(hwnd_val as *mut _);
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return Err(format!("Could not get PID for HWND {}", hwnd_val));
    }
    Ok(pid)
}

#[tauri::command]
pub async fn stop_screen_share(
    server_id: String,
    channel_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Take engines and build packet under the lock, then release immediately.
    // engine.stop() calls thread::join() which blocks — must not hold the mutex.
    let (old_audio_stream, old_video, stop_tx, stop_data) = {
        let mut s = state.lock().await;

        if let Some(ref ve) = s.voice_engine {
            ve.clear_keyframe_sender();
        }

        let old_audio_stream = s.audio_stream_engine.take();
        let old_video = s.video_engine.take();

        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let stop_tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        let stop_data = build_packet(
            packet::Type::StopStreamReq,
            packet::Payload::StopStreamReq(StopStreamRequest {
                channel_id: channel_id.into(),
            }),
            Some(&client.jwt),
        );

        (old_audio_stream, old_video, stop_tx, stop_data)
    }; // Lock released here

    // Stop engines on a background thread (thread::join blocks)
    tokio::task::spawn_blocking(move || {
        if let Some(mut e) = old_audio_stream { e.stop(); }
        if let Some(mut e) = old_video { e.stop(); }
    });

    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stop_tx.send(stop_data),
    ).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

#[tauri::command]
pub async fn watch_stream(
    server_id: String,
    channel_id: String,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    eprintln!("[stream] watch_stream called: server='{}', channel='{}', target='{}'",
        server_id, channel_id, target_username);

    let (write_tx, data) = {
        let s = state.lock().await;
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        let pkt = build_packet(
            packet::Type::WatchStreamReq,
            packet::Payload::WatchStreamReq(WatchStreamRequest {
                channel_id: channel_id.into(),
                target_username: target_username.into(),
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        write_tx.send(data),
    ).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    };
    eprintln!("[stream] watch_stream result: {:?}", result);
    result
}

#[tauri::command]
pub async fn stop_watching(
    server_id: String,
    channel_id: String,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        let pkt = build_packet(
            packet::Type::StopWatchingReq,
            packet::Payload::StopWatchingReq(StopWatchingRequest {
                channel_id: channel_id.into(),
                target_username: target_username.into(),
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
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
