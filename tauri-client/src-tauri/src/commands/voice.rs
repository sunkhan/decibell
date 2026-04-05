use tauri::{AppHandle, State};

use crate::events;
use crate::media::{VoiceEngine, VideoEngine, AudioStreamEngine};
use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

/// Send a pre-built packet via a cloned write channel with timeout.
async fn send_raw(tx: &tokio::sync::mpsc::Sender<Vec<u8>>, data: Vec<u8>) -> Result<(), String> {
    match tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

/// Stop engines on a blocking thread so thread::join() doesn't block a
/// Tokio worker (which would hold the AppState mutex and freeze the app).
fn stop_engines_background(
    audio_stream: Option<AudioStreamEngine>,
    video: Option<VideoEngine>,
    voice: Option<VoiceEngine>,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(mut e) = audio_stream { e.stop(); }
        if let Some(mut e) = video { e.stop(); }
        if let Some(mut e) = voice { e.stop(); }
    });
}

#[tauri::command]
pub async fn join_voice_channel(
    server_id: String,
    channel_id: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Take old engines and collect packets under the lock, then release
    let (old_audio_stream, old_video, old_voice, leave_sends, join_tx, join_data, state_sends, is_muted, is_deafened) = {
        let mut s = state.lock().await;

        let mut leave_sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = Vec::new();
        let old_audio_stream = s.audio_stream_engine.take();
        let old_video = s.video_engine.take();
        let old_voice = if s.voice_engine.is_some() {
            for client in s.communities.values() {
                if let Some(tx) = client.connection_write_tx() {
                    let data = build_packet(
                        packet::Type::LeaveVoiceReq,
                        packet::Payload::LeaveVoiceReq(LeaveVoiceRequest {}),
                        Some(&client.jwt),
                    );
                    leave_sends.push((tx, data));
                }
            }
            s.voice_engine.take()
        } else {
            None
        };

        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let join_tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        let join_data = build_packet(
            packet::Type::JoinVoiceReq,
            packet::Payload::JoinVoiceReq(JoinVoiceRequest {
                channel_id: channel_id.clone().into(),
            }),
            Some(&client.jwt),
        );
        let host = client.host.clone();
        let port = client.port;
        let jwt = client.jwt.clone();

        // Start VoiceEngine (spawns threads, no blocking I/O)
        let mut engine = VoiceEngine::start(&host, port, &jwt, app.clone())?;

        // Restore persisted mute/deafen state from previous session
        let saved_muted = s.voice_muted;
        let saved_deafened = s.voice_deafened;
        if saved_deafened {
            engine.set_deafen(true);
        } else if saved_muted {
            engine.set_mute(true);
        }
        let is_muted = engine.is_muted();
        let is_deafened = engine.is_deafened();

        s.voice_engine = Some(engine);
        s.connected_voice_server = Some(server_id);
        s.connected_voice_channel = Some(channel_id);

        // Build state notification so the server knows we're muted/deafened
        let mut state_sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = Vec::new();
        if is_muted || is_deafened {
            for client_val in s.communities.values() {
                if let Some(tx) = client_val.connection_write_tx() {
                    let data = build_packet(
                        packet::Type::VoiceStateNotify,
                        packet::Payload::VoiceStateNotify(VoiceStateNotify {
                            is_muted,
                            is_deafened,
                        }),
                        Some(&client_val.jwt),
                    );
                    state_sends.push((tx, data));
                }
            }
        }

        (old_audio_stream, old_video, old_voice, leave_sends, join_tx, join_data, state_sends, is_muted, is_deafened)
    }; // Lock released here

    // Stop old engines on a background thread (thread::join blocks)
    stop_engines_background(old_audio_stream, old_video, old_voice);

    for (tx, data) in leave_sends {
        let _ = send_raw(&tx, data).await;
    }
    send_raw(&join_tx, join_data).await?;

    // Notify server of restored mute/deafen state
    for (tx, data) in state_sends {
        let _ = send_raw(&tx, data).await;
    }

    events::emit_voice_state_changed(&app, is_muted, is_deafened);
    Ok(())
}

#[tauri::command]
pub async fn leave_voice_channel(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Take engines and collect packets under the lock
    let (old_audio_stream, old_video, old_voice, leave_sends, is_muted, is_deafened) = {
        let mut s = state.lock().await;

        let mut leave_sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = Vec::new();
        for client in s.communities.values() {
            if let Some(tx) = client.connection_write_tx() {
                let data = build_packet(
                    packet::Type::LeaveVoiceReq,
                    packet::Payload::LeaveVoiceReq(LeaveVoiceRequest {}),
                    Some(&client.jwt),
                );
                leave_sends.push((tx, data));
            }
        }

        // Save mute/deafen state before destroying the engine
        let (saved_muted, saved_deafened) = s.voice_engine.as_ref()
            .map(|e| (e.is_muted(), e.is_deafened()))
            .unwrap_or((s.voice_muted, s.voice_deafened));
        s.voice_muted = saved_muted;
        s.voice_deafened = saved_deafened;
        let is_muted = s.voice_muted;
        let is_deafened = s.voice_deafened;

        let old_audio_stream = s.audio_stream_engine.take();
        let old_video = s.video_engine.take();
        let old_voice = s.voice_engine.take();
        s.connected_voice_server = None;
        s.connected_voice_channel = None;

        (old_audio_stream, old_video, old_voice, leave_sends, is_muted, is_deafened)
    }; // Lock released here

    // Stop engines on a background thread (thread::join blocks)
    stop_engines_background(old_audio_stream, old_video, old_voice);

    for (tx, data) in leave_sends {
        let _ = send_raw(&tx, data).await;
    }

    // Emit the persisted state so the frontend keeps showing muted/deafened
    events::emit_voice_state_changed(&app, is_muted, is_deafened);
    Ok(())
}

#[tauri::command]
pub async fn set_voice_mute(
    muted: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    let engine = s.voice_engine.as_mut()
        .ok_or("Not in a voice channel")?;
    engine.set_mute(muted);
    let is_muted = engine.is_muted();
    let is_deafened = engine.is_deafened();
    s.voice_muted = is_muted;
    s.voice_deafened = is_deafened;

    let mut notify_sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = Vec::new();
    for client in s.communities.values() {
        if let Some(tx) = client.connection_write_tx() {
            let data = build_packet(
                packet::Type::VoiceStateNotify,
                packet::Payload::VoiceStateNotify(VoiceStateNotify {
                    is_muted,
                    is_deafened,
                }),
                Some(&client.jwt),
            );
            notify_sends.push((tx, data));
        }
    }
    drop(s);

    for (tx, data) in notify_sends {
        let _ = send_raw(&tx, data).await;
    }
    events::emit_voice_state_changed(&app, is_muted, is_deafened);
    Ok(())
}

#[tauri::command]
pub async fn set_voice_threshold(
    threshold_db: f32,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_voice_threshold(threshold_db);
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}

#[tauri::command]
pub async fn set_stream_volume(
    volume: f32,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_stream_volume(volume.clamp(0.0, 1.0));
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}

#[tauri::command]
pub async fn set_stream_stereo(
    enabled: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_stream_stereo(enabled);
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}

#[tauri::command]
pub async fn set_user_volume(
    username: String,
    gain: f32,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_user_volume(username, gain.max(0.0));
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}

#[tauri::command]
pub async fn set_aec_enabled(
    enabled: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_aec_enabled(enabled);
    }
    Ok(())
}

#[tauri::command]
pub async fn set_noise_suppression_level(
    level: u8,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_noise_suppression_level(level);
    }
    Ok(())
}

#[tauri::command]
pub async fn set_agc_enabled(
    enabled: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_agc_enabled(enabled);
    }
    Ok(())
}

#[tauri::command]
pub async fn set_voice_deafen(
    deafened: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    let engine = s.voice_engine.as_mut()
        .ok_or("Not in a voice channel")?;
    engine.set_deafen(deafened);
    let is_muted = engine.is_muted();
    let is_deafened = engine.is_deafened();
    s.voice_muted = is_muted;
    s.voice_deafened = is_deafened;

    let mut notify_sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = Vec::new();
    for client in s.communities.values() {
        if let Some(tx) = client.connection_write_tx() {
            let data = build_packet(
                packet::Type::VoiceStateNotify,
                packet::Payload::VoiceStateNotify(VoiceStateNotify {
                    is_muted,
                    is_deafened,
                }),
                Some(&client.jwt),
            );
            notify_sends.push((tx, data));
        }
    }
    drop(s);

    for (tx, data) in notify_sends {
        let _ = send_raw(&tx, data).await;
    }
    events::emit_voice_state_changed(&app, is_muted, is_deafened);
    Ok(())
}
