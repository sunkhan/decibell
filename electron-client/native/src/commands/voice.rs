//! Voice channel + voice-engine controls.
//!
//! Join / leave a voice channel, mute/deafen, voice threshold, AEC/NS/AGC
//! toggles, per-user volume, stream-audio mix controls. JoinVoiceRequest
//! ships the local client's encode + decode capabilities (Plan C) so the
//! community server can drive the LCD codec picker.
//!
//! Audio device list / input + output selection / mic test live in
//! commands/settings.rs.

use crate::config;
use crate::events;
use crate::media::caps::{self, CodecKind};
use crate::media::VoiceEngine;
use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state;

/// Send a pre-built packet via a cloned write channel with timeout.
async fn send_raw(tx: &tokio::sync::mpsc::Sender<Vec<u8>>, data: Vec<u8>) -> Result<(), String> {
    match tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

/// Stop the voice engine on a blocking thread so thread::join() doesn't
/// freeze a Tokio worker (which would hold AppState mutex and deadlock
/// the app). The streaming PR adds AudioStreamEngine + VideoEngine
/// shutdown alongside this one.
fn stop_voice_engine_background(voice: Option<VoiceEngine>) {
    tokio::task::spawn_blocking(move || {
        if let Some(mut e) = voice {
            e.stop();
        }
    });
}

#[napi(object)]
pub struct JoinVoiceChannelArgs {
    pub server_id: String,
    pub channel_id: String,
    /// Per-channel voice bitrate from CommunityAuthResponse. `None`
    /// falls back to the default Opus bitrate.
    pub voice_bitrate_kbps: Option<i32>,
}

#[napi]
pub async fn join_voice_channel(args: JoinVoiceChannelArgs) -> napi::Result<()> {
    let JoinVoiceChannelArgs {
        server_id,
        channel_id,
        voice_bitrate_kbps,
    } = args;

    let bitrate_bps = match voice_bitrate_kbps {
        Some(k) if k > 0 => k * 1000,
        _ => crate::media::codec::OpusEncoder::DEFAULT_BITRATE_BPS,
    };

    let state_arc = state::shared();

    // Take old engine + collect packets under one lock acquisition,
    // then release before any awaiting work or thread::join.
    let (
        old_voice,
        leave_sends,
        join_tx,
        join_data,
        state_sends,
        is_muted,
        is_deafened,
    ) = {
        let mut s = state_arc.lock().await;

        let mut leave_sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = Vec::new();
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

        let client = s
            .communities
            .get(&server_id)
            .ok_or_else(|| napi::Error::from_reason(format!("Not connected to community {}", server_id)))?;
        let join_tx = client
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Community connection lost"))?;
        // Plan C: ship encode + decode caps with the join request so the
        // server can drive watch-button gating and the LCD picker. Both
        // encode + decode caps come from the renderer's WebCodecs probes
        // (PR8 — FFmpeg-in-Rust encoding was removed). User toggles for
        // AV1/H.265 are applied here so peers don't see a codec we
        // refuse to encode with.
        let codec_settings = config::load()
            .map_err(|e| napi::Error::from_reason(e))?
            .settings;
        let encode_caps_filtered: Vec<_> = s
            .encoder_caps
            .iter()
            .filter(|c| match c.codec {
                CodecKind::Av1 => codec_settings.use_av1,
                CodecKind::H265 => codec_settings.use_h265,
                _ => true,
            })
            .cloned()
            .collect();
        let capabilities =
            caps::build_client_capabilities(&encode_caps_filtered, &s.decoder_caps);
        let join_data = build_packet(
            packet::Type::JoinVoiceReq,
            packet::Payload::JoinVoiceReq(JoinVoiceRequest {
                channel_id: channel_id.clone(),
                capabilities: Some(capabilities),
            }),
            Some(&client.jwt),
        );
        let host = client.host.clone();
        let port = client.port;
        let jwt = client.jwt.clone();

        let mut engine = VoiceEngine::start(&host, port, &jwt, bitrate_bps)
            .map_err(napi::Error::from_reason)?;

        // Restore persisted mute/deafen so the user's preference is
        // sticky across voice sessions.
        let saved_muted = s.voice_muted;
        let saved_deafened = s.voice_deafened;
        let saved_muted_before_deafen = s.voice_muted_before_deafen;
        if saved_deafened {
            if saved_muted_before_deafen {
                engine.set_mute(true);
            }
            engine.set_deafen(true);
        } else if saved_muted {
            engine.set_mute(true);
        }
        engine.set_muted_before_deafen(saved_muted_before_deafen);
        let is_muted = engine.is_muted();
        let is_deafened = engine.is_deafened();

        s.voice_engine = Some(engine);
        s.connected_voice_server = Some(server_id);
        s.connected_voice_channel = Some(channel_id);

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

        (
            old_voice,
            leave_sends,
            join_tx,
            join_data,
            state_sends,
            is_muted,
            is_deafened,
        )
    };

    stop_voice_engine_background(old_voice);

    for (tx, data) in leave_sends {
        let _ = send_raw(&tx, data).await;
    }
    send_raw(&join_tx, join_data)
        .await
        .map_err(napi::Error::from_reason)?;

    for (tx, data) in state_sends {
        let _ = send_raw(&tx, data).await;
    }

    events::emit_voice_state_changed(is_muted, is_deafened);
    Ok(())
}

#[napi]
pub async fn leave_voice_channel() -> napi::Result<()> {
    let state_arc = state::shared();

    let (old_voice, leave_sends, is_muted, is_deafened) = {
        let mut s = state_arc.lock().await;

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

        // Capture the engine's mute/deafen state before destroying it.
        let (saved_muted, saved_deafened, saved_mbd) = s
            .voice_engine
            .as_ref()
            .map(|e| (e.is_muted(), e.is_deafened(), e.muted_before_deafen()))
            .unwrap_or((s.voice_muted, s.voice_deafened, s.voice_muted_before_deafen));
        s.voice_muted = saved_muted;
        s.voice_deafened = saved_deafened;
        s.voice_muted_before_deafen = saved_mbd;
        let is_muted = s.voice_muted;
        let is_deafened = s.voice_deafened;

        let old_voice = s.voice_engine.take();
        s.connected_voice_server = None;
        s.connected_voice_channel = None;

        (old_voice, leave_sends, is_muted, is_deafened)
    };

    stop_voice_engine_background(old_voice);

    for (tx, data) in leave_sends {
        let _ = send_raw(&tx, data).await;
    }

    events::emit_voice_state_changed(is_muted, is_deafened);
    Ok(())
}

#[napi(object)]
pub struct SetVoiceMuteArgs {
    pub muted: bool,
}

#[napi]
pub async fn set_voice_mute(args: SetVoiceMuteArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let mut s = state_arc.lock().await;
    let (is_muted, is_deafened, mbd) = if let Some(engine) = s.voice_engine.as_mut() {
        engine.set_mute(args.muted);
        (
            engine.is_muted(),
            engine.is_deafened(),
            engine.muted_before_deafen(),
        )
    } else {
        (args.muted, s.voice_deafened, s.voice_muted_before_deafen)
    };
    s.voice_muted_before_deafen = mbd;
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
    events::emit_voice_state_changed(is_muted, is_deafened);
    Ok(())
}

#[napi(object)]
pub struct SetVoiceDeafenArgs {
    pub deafened: bool,
}

#[napi]
pub async fn set_voice_deafen(args: SetVoiceDeafenArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let mut s = state_arc.lock().await;
    let (is_muted, is_deafened, mbd) = if let Some(engine) = s.voice_engine.as_mut() {
        engine.set_deafen(args.deafened);
        (
            engine.is_muted(),
            engine.is_deafened(),
            engine.muted_before_deafen(),
        )
    } else if args.deafened {
        (true, true, s.voice_muted)
    } else {
        (s.voice_muted_before_deafen, false, s.voice_muted_before_deafen)
    };
    s.voice_muted_before_deafen = mbd;
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
    events::emit_voice_state_changed(is_muted, is_deafened);
    Ok(())
}

#[napi(object)]
pub struct SetVoiceThresholdArgs {
    pub threshold_db: f64,
}

#[napi]
pub async fn set_voice_threshold(args: SetVoiceThresholdArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_voice_threshold(args.threshold_db as f32);
        Ok(())
    } else {
        Err(napi::Error::from_reason("Not in a voice channel"))
    }
}

#[napi(object)]
pub struct SetStreamVolumeArgs {
    pub volume: f64,
}

#[napi]
pub async fn set_stream_volume(args: SetStreamVolumeArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_stream_volume((args.volume as f32).clamp(0.0, 1.0));
        Ok(())
    } else {
        Err(napi::Error::from_reason("Not in a voice channel"))
    }
}

#[napi(object)]
pub struct SetStreamStereoArgs {
    pub enabled: bool,
}

#[napi]
pub async fn set_stream_stereo(args: SetStreamStereoArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_stream_stereo(args.enabled);
        Ok(())
    } else {
        Err(napi::Error::from_reason("Not in a voice channel"))
    }
}

#[napi(object)]
pub struct SetUserVolumeArgs {
    pub username: String,
    pub gain: f64,
}

#[napi]
pub async fn set_user_volume(args: SetUserVolumeArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_user_volume(args.username, (args.gain as f32).max(0.0));
        Ok(())
    } else {
        Err(napi::Error::from_reason("Not in a voice channel"))
    }
}

#[napi(object)]
pub struct SetAecEnabledArgs {
    pub enabled: bool,
}

#[napi]
pub async fn set_aec_enabled(args: SetAecEnabledArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_aec_enabled(args.enabled);
    }
    Ok(())
}

#[napi(object)]
pub struct SetNoiseSuppressionLevelArgs {
    pub level: u8,
}

#[napi]
pub async fn set_noise_suppression_level(args: SetNoiseSuppressionLevelArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_noise_suppression_level(args.level);
    }
    Ok(())
}

#[napi(object)]
pub struct SetAgcEnabledArgs {
    pub enabled: bool,
}

#[napi]
pub async fn set_agc_enabled(args: SetAgcEnabledArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_agc_enabled(args.enabled);
    }
    Ok(())
}

#[napi(object)]
pub struct SetInputDeviceArgs {
    /// `None` falls back to the system default.
    pub name: Option<String>,
}

#[napi]
pub async fn set_input_device(args: SetInputDeviceArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_input_device(args.name);
    }
    Ok(())
}

#[napi(object)]
pub struct SetOutputDeviceArgs {
    pub name: Option<String>,
}

#[napi]
pub async fn set_output_device(args: SetOutputDeviceArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_output_device(args.name);
    }
    Ok(())
}

#[napi(object)]
pub struct SetSeparateStreamOutputArgs {
    pub enabled: bool,
    pub device: Option<String>,
}

#[napi]
pub async fn set_separate_stream_output(args: SetSeparateStreamOutputArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_separate_stream_output(args.enabled, args.device);
    }
    Ok(())
}

#[napi(object)]
pub struct SetStreamOutputDeviceArgs {
    pub name: Option<String>,
}

#[napi]
pub async fn set_stream_output_device(args: SetStreamOutputDeviceArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_stream_output_device(args.name);
    }
    Ok(())
}
