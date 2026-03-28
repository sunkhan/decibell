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

    // If already in voice, stop the existing engine
    if s.voice_engine.is_some() {
        for client in s.communities.values() {
            let _ = client.leave_voice_channel().await;
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

    // Stop video engine first (it depends on voice engine's socket)
    if let Some(mut engine) = s.video_engine.take() {
        engine.stop();
    }

    if let Some(mut engine) = s.voice_engine.take() {
        engine.stop();
    }

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
