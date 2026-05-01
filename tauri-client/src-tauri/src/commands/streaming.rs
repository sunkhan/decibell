use tauri::{AppHandle, State};
use crate::state::SharedState;
use crate::media::{capture, caps::CodecKind, encoder::EncoderConfig, VideoEngine, AudioStreamEngine};
use crate::net::connection::build_packet;
use crate::net::proto::*;

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide flag flipped to true the first time GpuStreamingPipeline::build
/// fails. Prevents subsequent stream-start attempts from re-entering the GPU
/// build path, which on some setups (notably the installer-bundled FFmpeg
/// version mismatch we hit in 0.4.3) leaves the NVIDIA driver / FFmpeg static
/// state in a broken shape that crashes the second attempt with an SEH access
/// violation. Resets on app restart. The user's first-attempt fallback toast
/// already informed them GPU is unavailable; suppressing the toast on later
/// attempts in the same session keeps the UX clean.
#[cfg(target_os = "windows")]
static GPU_PIPELINE_DISABLED: AtomicBool = AtomicBool::new(false);

/// True when the chosen codec maps to NVENC (the only family the Windows
/// GPU pipeline currently builds for). H264Sw is libx264 (CPU) so it
/// must always take the CPU path.
#[cfg(target_os = "windows")]
fn is_gpu_eligible(codec: CodecKind) -> bool {
    matches!(codec, CodecKind::H264Hw | CodecKind::H265 | CodecKind::Av1)
}

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
    // Plan B Task 7 dev shim: explicit codec value (1=H264_HW, 2=H264_SW,
    // 3=H265, 4=AV1). When None or 0, defaults to H264_HW. Plan C Task 11
    // replaces this with enforced_codec driven by the production UI
    // dropdown plus auto-pick from the LCD picker for Auto mode.
    force_codec: Option<u8>,
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

    // Resolve force_codec → CodecKind.
    // Plan C semantics: None or 0 = Auto (no enforcement, LCD picker
    // selects initial codec from streamer's encode caps). Otherwise the
    // user explicitly forced a codec — that codec becomes both the
    // initial and the enforced codec on the wire (viewers without it
    // see grayed-out Watch button + can't subscribe).
    let target_codec = match force_codec {
        None | Some(0) | Some(1) => CodecKind::H264Hw,
        Some(2) => CodecKind::H264Sw,
        Some(3) => CodecKind::H265,
        Some(4) => CodecKind::Av1,
        Some(other) => return Err(format!("Unknown codec value: {}", other)),
    };
    let enforced_codec_value: i32 = match force_codec {
        None | Some(0) => 0,           // Auto — no enforcement
        Some(c) => c as i32,           // user picked X — enforce
    };

    eprintln!("[stream] start_screen_share: server='{}', channel='{}', source='{}', {}x{} @ {}fps",
        server_id, channel_id, source_id, width, height, fps);

    // ── Snapshot everything we need from AppState under a brief lock ──
    let (voice_socket, media_socket, sender_id, community_write_tx, jwt,
         watcher_event_tx, voice_caps_cache, self_username) = {
        let s = state.lock().await;
        let voice_engine = s.voice_engine.as_ref()
            .ok_or("Voice channel disconnected")?;
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let community_tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        (
            voice_engine.voice_socket(),
            voice_engine.media_socket(),
            voice_engine.sender_id().to_string(),
            community_tx,
            client.jwt.clone(),
            s.watcher_event_tx.clone(),
            s.voice_caps_cache.clone(),
            s.username.clone().unwrap_or_default(),
        )
    };

    // Resolve Quality preset, encode caps, toggles — used by both paths.
    let quality_preset = match (quality.as_str(), video_bitrate_kbps) {
        ("low", _) => crate::media::bitrate_preset::Quality::Low,
        ("medium", _) => crate::media::bitrate_preset::Quality::Medium,
        ("custom", Some(kbps)) => crate::media::bitrate_preset::Quality::Custom(kbps),
        _ => crate::media::bitrate_preset::Quality::High,
    };
    let stream_ctx = crate::media::video_pipeline::StreamerContext {
        channel_id: channel_id.clone(),
        streamer_username: self_username.clone(),
        quality: quality_preset,
    };
    let codec_settings = crate::config::load(&app)?.settings;
    let encoder_caps_all = crate::media::caps::get_or_probe_encoders(&app);
    let encode_caps_filtered: Vec<_> = encoder_caps_all.into_iter().filter(|c| match c.codec {
        crate::media::caps::CodecKind::Av1 => codec_settings.use_av1,
        crate::media::caps::CodecKind::H265 => codec_settings.use_h265,
        _ => true,
    }).collect();
    let toggles = crate::media::codec_selection::Toggles {
        use_av1: codec_settings.use_av1,
        use_h265: codec_settings.use_h265,
    };
    let enforced_codec_for_selector = if enforced_codec_value == 0 { None } else { Some(target_codec) };
    let thumbnail_channel_id = Some(channel_id.clone());

    // ── Plan-D-2: try Windows GPU zero-copy path first ──
    // GpuStreamingPipeline does its own capture (DXGI/WGC) — we skip the
    // CPU capture::start_capture entirely on success. On any build error
    // we surface a toast then fall through to the CPU path so the stream
    // still starts.
    //
    // Fail-fast: if we already failed GPU build once this session, skip
    // straight to CPU. Some installer-bundled-FFmpeg setups crash on the
    // second-or-later attempt, so we only let GPU try once per process.
    #[cfg(target_os = "windows")]
    let gpu_attempt: Option<(VideoEngine, u32, u32)> = if is_gpu_eligible(target_codec)
        && !GPU_PIPELINE_DISABLED.load(Ordering::Relaxed)
    {
        let gpu_config = EncoderConfig {
            width, height, fps, bitrate_kbps,
            keyframe_interval_secs: 2,
        };
        match VideoEngine::start_gpu(
            source_id.clone(),
            target_codec,
            gpu_config,
            fps,
            media_socket.clone(),
            sender_id.clone(),
            app.clone(),
            Some(community_write_tx.clone()),
            thumbnail_channel_id.clone(),
            self_username.clone(),
            Some(stream_ctx.clone()),
            Some(community_write_tx.clone()),
            Some(jwt.clone()),
            Some(encode_caps_filtered.clone()),
            Some(toggles.clone()),
            enforced_codec_for_selector,
            watcher_event_tx.clone(),
            voice_caps_cache.clone(),
        ) {
            Ok((engine, eff_w, eff_h)) => {
                eprintln!("[stream] GPU zero-copy pipeline started ({}x{})", eff_w, eff_h);
                Some((engine, eff_w, eff_h))
            }
            Err(e) => {
                eprintln!("[stream] GPU pipeline build failed: {} — falling back to CPU (GPU now disabled for this session)", e);
                GPU_PIPELINE_DISABLED.store(true, Ordering::Relaxed);
                crate::events::emit_stream_gpu_fallback(&app, e);
                None
            }
        }
    } else { None };
    #[cfg(not(target_os = "windows"))]
    let gpu_attempt: Option<(VideoEngine, u32, u32)> = None;

    // ── CPU fallback path: existing capture::start_capture + VideoEngine::start ──
    let (video_engine, enc_width, enc_height) = if let Some((engine, eff_w, eff_h)) = gpu_attempt {
        (engine, eff_w, eff_h)
    } else {
        let capture_config = capture::CaptureConfig {
            target_fps: fps,
            target_width: width,
            target_height: height,
        };
        let capture_output = capture::start_capture(&source_id, &capture_config).await?;
        let enc_w = capture_output.width;
        let enc_h = capture_output.height;
        let encoder_config = EncoderConfig {
            width: enc_w,
            height: enc_h,
            fps, bitrate_kbps,
            keyframe_interval_secs: 2,
        };
        let engine = VideoEngine::start(
            capture_output.receiver,
            #[cfg(target_os = "linux")]
            capture_output.gpu_receiver,
            media_socket.clone(),
            sender_id.clone(),
            encoder_config,
            fps,
            target_codec,
            app.clone(),
            Some(community_write_tx.clone()),
            thumbnail_channel_id.clone(),
            self_username.clone(),
            Some(stream_ctx.clone()),
            Some(community_write_tx.clone()),
            Some(jwt.clone()),
            Some(encode_caps_filtered.clone()),
            Some(toggles.clone()),
            enforced_codec_for_selector,
            watcher_event_tx.clone(),
            voice_caps_cache.clone(),
        );
        (engine, enc_w, enc_h)
    };

    // ── Re-lock briefly to install the engine + wire keyframe forwarding ──
    let mut s = state.lock().await;
    if let Some(ref ve) = s.voice_engine {
        ve.set_keyframe_sender(video_engine.pipeline_control_tx());
    }
    s.video_engine = Some(video_engine);

    // Build the start_stream packet under the same lock window so we can
    // re-resolve the community client (it may have been replaced if the
    // user reconnected during GPU init).
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
            chosen_codec: target_codec as i32,
            enforced_codec: enforced_codec_value,
        }),
        Some(&client.jwt),
    );

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
                voice_socket,
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
                voice_socket,
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
pub async fn watch_self_stream(
    enabled: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let ve = s.video_engine.as_ref()
        .ok_or("Not currently streaming")?;
    ve.set_self_preview(enabled);
    if enabled { ve.force_keyframe(); }
    Ok(())
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

    // Defensive: drop any stale slot for this streamer before we start.
    // stop_watching already does this on the way out, but a crash, missed
    // unmount, or codec-renegotiation hiccup can leave a frame parked in
    // the slot with a high sequence number — the new mount's lastSequence
    // starts at 0, sees the stale frame as "newer", uploads it, and then
    // ignores every fresh decode that publishes with seq=1, 2, 3...
    #[cfg(target_os = "linux")]
    crate::media::nv12_store::forget(&target_username);

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
                // Clone — we still need `target_username` below for the
                // Linux NV12-slot drop after the send completes.
                target_username: target_username.clone().into(),
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    let send_result = match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    };

    // Drop the per-streamer NV12 slot so the buffer (~3MB at 1080p) is
    // freed immediately instead of waiting for the next decode that
    // never comes. Linux-only — Windows uses WebCodecs and has no slot.
    #[cfg(target_os = "linux")]
    crate::media::nv12_store::forget(&target_username);

    send_result
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
