use tauri::{AppHandle, State};
use crate::state::SharedState;
use crate::media::{capture, encoder::EncoderConfig, VideoEngine, AudioStreamEngine};

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
    client.start_stream(&channel_id, fps as i32, bitrate_kbps as i32, share_audio, enc_width, enc_height).await?;

    let encoder_config = EncoderConfig {
        width: enc_width,
        height: enc_height,
        fps,
        bitrate_kbps,
        keyframe_interval_secs: 2,
    };

    // Start video pipeline
    let video_engine = VideoEngine::start(
        capture_output.receiver,
        socket.clone(),
        sender_id.clone(),
        encoder_config,
        fps,
        app.clone(),
    );

    s.video_engine = Some(video_engine);

    // Start audio stream capture if enabled
    if share_audio {
        let bitrate = audio_bitrate_kbps.unwrap_or(128);
        eprintln!("[stream] Starting audio capture for source '{}', bitrate={}kbps", source_id, bitrate);

        let is_window = is_window_source(&source_id);

        #[cfg(target_os = "linux")]
        {
            let (audio_rx, cleanup) = if is_window {
                // On Linux with portal, we don't have a PID for per-process capture.
                // Use system-minus-self for all captures via portal.
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

    Ok(())
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
    let mut s = state.lock().await;

    // Stop audio stream engine first
    if let Some(mut engine) = s.audio_stream_engine.take() {
        engine.stop();
    }

    // Stop video engine
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
    eprintln!("[stream] watch_stream called: server='{}', channel='{}', target='{}'",
        server_id, channel_id, target_username);
    let s = state.lock().await;
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    let result = client.watch_stream(&channel_id, &target_username).await;
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
