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
pub(crate) fn stop_voice_engine_background(voice: Option<VoiceEngine>) {
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

fn leave_voice_packet() -> Vec<u8> {
    build_packet(
        packet::Type::LeaveVoiceReq,
        packet::Payload::LeaveVoiceReq(LeaveVoiceRequest {}),
        None,
    )
}

/// What a join sends, in order: when switching, our LEAVE to every
/// community; then our mute / deafen to every community; then the JOIN and
/// the MLS group's GroupInfo request on the target's connection. The server
/// forgets mute / deafen on a leave and, out of a channel, only records them,
/// so the JOIN's roster already shows them (sent after it, others saw a muted
/// user unmuted for a moment). The GroupInfo request rides right behind the
/// JOIN: the server answers it only for a member of the channel.
fn join_outbox(
    connections: &[tokio::sync::mpsc::Sender<Vec<u8>>],
    join_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    channel_id: &str,
    join_data: Vec<u8>,
    switching: bool,
    is_muted: bool,
    is_deafened: bool,
) -> Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> {
    let mut out = Vec::new();
    if switching {
        for tx in connections {
            out.push((tx.clone(), leave_voice_packet()));
        }
    }
    if is_muted || is_deafened {
        for tx in connections {
            let data = build_packet(
                packet::Type::VoiceStateNotify,
                packet::Payload::VoiceStateNotify(VoiceStateNotify {
                    is_muted,
                    is_deafened,
                }),
                None,
            );
            out.push((tx.clone(), data));
        }
    }
    out.push((join_tx.clone(), join_data));
    out.push((join_tx.clone(), crate::e2ee::group::group_info_request(channel_id)));
    out
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

    // The whole switch under one lock acquisition, so no other command sees
    // it half done. The server hears first: the JOIN goes out before the new
    // engine and MLS group start, so everyone sees us move without waiting
    // for them, and the group's handshake is under way while the engine
    // starts. (Started first, they held the JOIN back: two DNS lookups, the
    // engine's threads, the group setup.) The old engine's thread joins run
    // off this worker.
    let mut s = state_arc.lock().await;

    // A DM call owns the single VoiceEngine; the renderer hangs up
    // (call_end) before joining a channel. Refuse rather than silently
    // stealing the engine from under the call.
    if s.active_call.is_some() {
        return Err(napi::Error::from_reason("Hang up the call first"));
    }

    // Everything that can refuse the join, before the old session is touched.
    let client = s
        .communities
        .get(&server_id)
        .ok_or_else(|| napi::Error::from_reason(format!("Not connected to community {}", server_id)))?;
    let join_tx = client
        .connection_write_tx()
        .ok_or_else(|| napi::Error::from_reason("Community connection lost"))?;
    let host = client.host.clone();
    let port = client.port;
    // The UDP key is the tail of the token this connection authenticated
    // with, not of a later refresh.
    let udp_jwt = client.auth_jwt.clone();
    // The codec toggles and the saved audio devices. The devices let the
    // pipeline start on them instead of on the system defaults and then
    // hot-swapping; unset, `build_*_stream` tries the OS default, and
    // `get_default_device` falls back to the first device there is.
    let settings = config::load()
        .map_err(|e| napi::Error::from_reason(e))?
        .settings;
    // Plan C: ship encode + decode caps with the join request so the
    // server can drive watch-button gating and the LCD picker. Both
    // encode + decode caps come from the renderer's WebCodecs probes
    // (PR8 — FFmpeg-in-Rust encoding was removed). User toggles for
    // AV1/H.265 are applied here so peers don't see a codec we
    // refuse to encode with.
    let encode_caps_filtered: Vec<_> = s
        .encoder_caps
        .iter()
        .filter(|c| match c.codec {
            CodecKind::Av1 => settings.use_av1,
            CodecKind::H265 => settings.use_h265,
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
        None,
    );
    // Strict E2EE: community voice is MLS-encrypted, so this device needs
    // its identity keys loaded. The renderer checks first and opens the
    // set-up / unlock prompt; this is the backstop.
    if s.e2ee.status != crate::e2ee::session::Status::Ready {
        return Err(napi::Error::from_reason(
            "Set up encryption (Settings → Privacy) to join voice channels",
        ));
    }
    let uid = s.uid;
    if uid <= 0 {
        return Err(napi::Error::from_reason("Not signed in"));
    }
    let identity = s
        .e2ee
        .store
        .as_ref()
        .and_then(|st| st.current())
        .cloned()
        .ok_or_else(|| napi::Error::from_reason("Encryption keys not loaded"))?;

    // The old session ends here. Forget its watches now (not in the
    // deferred engine stop) so they can't wipe the new channel's; its MLS
    // group driver goes with it.
    let switching = s.voice_engine.is_some();
    if switching {
        crate::media::watched_streams_clear();
    }
    stop_voice_engine_background(s.voice_engine.take());
    s.voice_mls = None;
    s.connected_voice_server = None;
    s.connected_voice_channel = None;

    // Our persisted mute / deafen, as the engine below will hold them
    // (deafening mutes too: VoiceEngine::set_deafen).
    let saved_muted = s.voice_muted;
    let saved_deafened = s.voice_deafened;
    let saved_muted_before_deafen = s.voice_muted_before_deafen;
    let is_muted = saved_muted || saved_deafened;
    let is_deafened = saved_deafened;

    let connections: Vec<_> = s
        .communities
        .values()
        .filter_map(|c| c.connection_write_tx())
        .collect();
    let outbox = join_outbox(&connections, &join_tx, &channel_id, join_data, switching, is_muted, is_deafened);
    let join_at = outbox.len() - 2;
    for (i, (tx, data)) in outbox.into_iter().enumerate() {
        let sent = send_raw(&tx, data).await;
        if i == join_at {
            sent.map_err(napi::Error::from_reason)?;
        }
    }

    // The server lists us in the channel now. The MLS group for it joins
    // (or creates) on the GroupInfo already requested and feeds epoch keys
    // into `ring` — the engine sends nothing until the first epoch lands.
    // Media keys are per sender, named by uid (the media identity).
    let ring = crate::media::frame_crypto::new_shared(&uid.to_string());
    let started = crate::e2ee::group::start(
        state_arc.clone(),
        server_id.clone(),
        channel_id.clone(),
        uid,
        &identity,
        join_tx.clone(),
        ring.clone(),
        true,
    )
    .and_then(|handle| {
        VoiceEngine::start(
            &host,
            port,
            &udp_jwt,
            bitrate_bps,
            settings.input_device.clone(),
            settings.output_device.clone(),
            ring,
        )
        .map(|engine| (handle, engine))
    });
    let (handle, mut engine) = match started {
        Ok(x) => x,
        Err(e) => {
            // Take the JOIN back, or everyone keeps seeing us in a channel
            // we have no session for.
            drop(s);
            let _ = send_raw(&join_tx, leave_voice_packet()).await;
            return Err(napi::Error::from_reason(e));
        }
    };
    s.voice_mls = Some(handle);

    // Restore persisted mute/deafen so the user's preference is
    // sticky across voice sessions.
    if saved_deafened {
        if saved_muted_before_deafen {
            engine.set_mute(true);
        }
        engine.set_deafen(true);
    } else if saved_muted {
        engine.set_mute(true);
    }
    engine.set_muted_before_deafen(saved_muted_before_deafen);
    debug_assert_eq!((engine.is_muted(), engine.is_deafened()), (is_muted, is_deafened));

    s.voice_engine = Some(engine);
    s.connected_voice_server = Some(server_id);
    s.connected_voice_channel = Some(channel_id);
    drop(s);

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
                    None,
                );
                leave_sends.push((tx, data));
            }
        }

        // The MLS group driver goes with the session (the others remove
        // our leaf once the server drops us from the roster).
        s.voice_mls = None;

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

        crate::media::watched_streams_clear();
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

/// A moderator moved us (VOICE_FORCE_NOTIFY{MOVED}): the server already
/// switched our session, so the engine (UDP sockets, audio) keeps running
/// and only the MLS group follows — a driver left on the old channel never
/// joins the new one's group, and nobody there can open our media. Called
/// from the community read loop, before any packet for the new channel.
/// Returns the new group's command channel so the caller can seed the roster.
pub(crate) fn rebind_voice_group(
    s: &mut state::AppState,
    state_arc: std::sync::Arc<tokio::sync::Mutex<state::AppState>>,
    server_id: &str,
    channel_id: &str,
) -> Result<Option<tokio::sync::mpsc::UnboundedSender<crate::e2ee::group::GroupCmd>>, String> {
    if s.voice_engine.is_none() || s.connected_voice_server.as_deref() != Some(server_id) {
        return Ok(None);
    }
    if s.connected_voice_channel.as_deref() == Some(channel_id) {
        return Ok(None);
    }
    // The engine's pipelines hold this ring; keep the instance, drop the
    // old group's keys so nothing is sealed until the new epoch lands.
    let Some(old) = s.voice_mls.take() else {
        return Err("no voice encryption group to move".into());
    };
    let ring = old.ring.clone();
    drop(old);
    s.connected_voice_channel = Some(channel_id.to_string());
    // The server dropped our watches with the old channel.
    crate::media::watched_streams_clear();
    if s.uid <= 0 {
        return Err("Not signed in".into());
    }
    ring.store(std::sync::Arc::new(crate::media::frame_crypto::KeyRing::empty(&s.uid.to_string())));
    let identity = s
        .e2ee
        .store
        .as_ref()
        .and_then(|st| st.current())
        .cloned()
        .ok_or("Encryption keys not loaded")?;
    let client = s
        .communities
        .get(server_id)
        .ok_or_else(|| format!("Not connected to community {server_id}"))?;
    let write_tx = client.connection_write_tx().ok_or("Community connection lost")?;
    let handle = crate::e2ee::group::start(
        state_arc,
        server_id.to_string(),
        channel_id.to_string(),
        s.uid,
        &identity,
        write_tx,
        ring,
        false,
    )?;
    let tx = handle.tx.clone();
    s.voice_mls = Some(handle);
    Ok(Some(tx))
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
                None,
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
                None,
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
pub struct SetVoiceBitrateArgs {
    /// Channel bitrate in kbps; 0 = client default. Called by the
    /// renderer when the connected channel's bitrate changes mid-call
    /// (channel_updated broadcast) so the running encoder adapts
    /// without a rejoin.
    pub bitrate_kbps: i32,
}

#[napi]
pub async fn set_voice_bitrate(args: SetVoiceBitrateArgs) -> napi::Result<()> {
    let bps = if args.bitrate_kbps > 0 {
        args.bitrate_kbps * 1000
    } else {
        crate::media::codec::OpusEncoder::DEFAULT_BITRATE_BPS
    };
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_voice_bitrate(bps);
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
    /// The speaker's uid: the media plane names senders by uid.
    pub uid: i64,
    pub gain: f64,
}

#[napi]
pub async fn set_user_volume(args: SetUserVolumeArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_user_volume(args.uid.to_string(), (args.gain as f32).max(0.0));
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

#[napi(object)]
pub struct SetInputLevelReportingArgs {
    pub enabled: bool,
}

/// Gate the `voice_input_level` event (~16 Hz while in voice). Only the
/// Settings → Audio meter listens, so it switches reporting on while
/// mounted and off on unmount; default off. Sync: one atomic store.
#[napi]
pub fn set_input_level_reporting(args: SetInputLevelReportingArgs) {
    events::set_input_level_reporting(args.enabled);
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn kinds(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> Vec<packet::Type> {
        let mut out = Vec::new();
        while let Ok(data) = rx.try_recv() {
            let p = Packet::decode(data.as_slice()).expect("a packet");
            out.push(packet::Type::try_from(p.r#type).expect("a known type"));
        }
        out
    }

    fn send_all(outbox: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)>) {
        for (tx, data) in outbox {
            tx.try_send(data).expect("room in the test channel");
        }
    }

    /// A muted switch from one community's channel to another's: each
    /// connection hears LEAVE before the mute, and the target hears both
    /// before its JOIN, with the GroupInfo request right behind the JOIN.
    #[test]
    fn a_switch_leaves_then_mutes_then_joins() {
        let (old_tx, mut old_rx) = tokio::sync::mpsc::channel(16);
        let (target_tx, mut target_rx) = tokio::sync::mpsc::channel(16);
        let join = build_packet(
            packet::Type::JoinVoiceReq,
            packet::Payload::JoinVoiceReq(JoinVoiceRequest { channel_id: "lounge".into(), capabilities: None }),
            None,
        );
        let connections = vec![old_tx, target_tx.clone()];
        send_all(join_outbox(&connections, &target_tx, "lounge", join, true, true, false));
        use packet::Type::*;
        assert_eq!(kinds(&mut old_rx), vec![LeaveVoiceReq, VoiceStateNotify]);
        assert_eq!(
            kinds(&mut target_rx),
            vec![LeaveVoiceReq, VoiceStateNotify, JoinVoiceReq, MlsGroupInfoReq]
        );
    }

    /// A first join, unmuted: no LEAVE, no state, just the JOIN and the
    /// GroupInfo request (for the channel joined).
    #[test]
    fn a_first_join_sends_only_the_join() {
        let (target_tx, mut target_rx) = tokio::sync::mpsc::channel(16);
        let join = build_packet(
            packet::Type::JoinVoiceReq,
            packet::Payload::JoinVoiceReq(JoinVoiceRequest { channel_id: "lounge".into(), capabilities: None }),
            None,
        );
        let outbox = join_outbox(&[target_tx.clone()], &target_tx, "lounge", join, false, false, false);
        let info = outbox.last().map(|(_, d)| Packet::decode(d.as_slice()).unwrap());
        match info.and_then(|p| p.payload) {
            Some(packet::Payload::MlsGroupInfoReq(r)) => assert_eq!(r.channel_id, "lounge"),
            other => panic!("expected a GroupInfo request, got {other:?}"),
        }
        send_all(outbox);
        assert_eq!(kinds(&mut target_rx), vec![packet::Type::JoinVoiceReq, packet::Type::MlsGroupInfoReq]);
    }
}
