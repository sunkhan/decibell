use tauri::{AppHandle, State};

use crate::events;
use crate::media::VoiceEngine;
use crate::state::SharedState;

#[tauri::command]
pub async fn join_voice_channel(
    server_id: String,
    channel_id: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // If already in voice, stop existing engines in dependency order
    if s.voice_engine.is_some() {
        for client in s.communities.values() {
            let _ = client.leave_voice_channel().await;
        }
        if let Some(mut engine) = s.audio_stream_engine.take() {
            engine.stop();
        }
        if let Some(mut engine) = s.video_engine.take() {
            engine.stop();
        }
        if let Some(mut engine) = s.voice_engine.take() {
            engine.stop();
        }
    }

    // Send JOIN_VOICE_REQ over TCP and get connection details
    let (host, port, jwt) = {
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        client.join_voice_channel(&channel_id).await?;
        (client.host.clone(), client.port, client.jwt.clone())
    };

    // Start VoiceEngine
    let engine = VoiceEngine::start(&host, port, &jwt, app.clone())?;
    s.voice_engine = Some(engine);
    s.connected_voice_server = Some(server_id);
    s.connected_voice_channel = Some(channel_id);

    events::emit_voice_state_changed(&app, false, false);
    Ok(())
}

#[tauri::command]
pub async fn leave_voice_channel(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    for client in s.communities.values() {
        let _ = client.leave_voice_channel().await;
    }

    // Stop engines in dependency order: audio stream → video → voice
    if let Some(mut engine) = s.audio_stream_engine.take() {
        engine.stop();
    }
    if let Some(mut engine) = s.video_engine.take() {
        engine.stop();
    }
    if let Some(mut engine) = s.voice_engine.take() {
        engine.stop();
    }
    s.connected_voice_server = None;
    s.connected_voice_channel = None;

    events::emit_voice_state_changed(&app, false, false);
    Ok(())
}

#[tauri::command]
pub async fn set_voice_mute(
    muted: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    if let Some(ref mut engine) = s.voice_engine {
        engine.set_mute(muted);
        let is_muted = engine.is_muted();
        let is_deafened = engine.is_deafened();
        events::emit_voice_state_changed(&app, is_muted, is_deafened);
        // Notify community server of state change
        for client in s.communities.values() {
            let _ = client.send_voice_state_notify(is_muted, is_deafened).await;
        }
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
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
pub async fn set_voice_deafen(
    deafened: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    if let Some(ref mut engine) = s.voice_engine {
        engine.set_deafen(deafened);
        let is_muted = engine.is_muted();
        let is_deafened = engine.is_deafened();
        events::emit_voice_state_changed(&app, is_muted, is_deafened);
        // Notify community server of state change
        for client in s.communities.values() {
            let _ = client.send_voice_state_notify(is_muted, is_deafened).await;
        }
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}
