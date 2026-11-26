//! Voice channels and streams (`features/voice/useVoiceEvents.ts`, `useStreamThumbnails.ts`,
//! `useServerEvents.ts`' `voice_force_notify` and live bitrate, `voiceStore.clearServerPresence`).

use std::collections::{HashMap, HashSet};

use decibell_core::events::{
    NativeStreamFailedPayload, StreamCodecChangedPayload, StreamGpuFallbackPayload, StreamPresenceUpdatedPayload,
    StreamThumbnail, VoiceConnectionStatsPayload, VoiceE2eeStatePayload, VoiceErrorPayload, VoiceForceNotifyPayload,
    VoiceInputLevelPayload, VoicePingUpdatedPayload, VoicePresenceUpdatedPayload, VoiceStateChangedPayload,
    VoiceUserSpeakingPayload, VoiceUserStateChangedPayload,
};
use gpui::{App, SharedString};

use crate::Model;
use crate::actions::voice::{self as act, StreamEnd, core, play};
use crate::ids::{ChannelId, ServerId, Uid};
use crate::live::Sound;
use crate::live::voice::codecs::{VideoCodec, codec_change_text};
use crate::live::voice::session::{Participant, VoiceE2ee, VoiceE2eePhase};
use crate::live::voice::stats::StatsSample;
use crate::live::voice::streams::{OwnStreamPhase, StreamInfo};
use crate::stores::community::{Channel, ChannelKind};
use crate::stores::navigation::ActiveView;

/// A channel's roster: every connected community sends every visible channel's. For the
/// connected channel also the join / leave sounds, the stage's participants and the
/// saved per-user gains (someone muted earlier stays muted).
pub fn voice_presence_updated(model: &Model, p: &VoicePresenceUpdatedPayload, cx: &mut App) {
    let uids = p.participant_uids.clone();
    if uids.len() != p.participants.len() {
        return;
    }
    let names: HashMap<Uid, &String> = uids.iter().copied().zip(&p.participants).collect();
    let key = (ServerId::from_wire(&p.server_id), ChannelId::from(p.channel_id.clone()));
    let me = model.auth.read(cx).uid;
    let voice = model.live.voice.clone();
    // The store corrects the roster for our own join / leave the server hasn't caught
    // up with yet.
    let shown = voice.rosters.update(cx, |r, cx| r.set_presence(key.clone(), uids, &p.user_states, &p.user_capabilities, me, cx).clone());
    if voice.session.read(cx).connected().as_ref() != Some(&key) {
        return;
    }
    let current: HashSet<Uid> = shown.roster.iter().copied().collect();
    if let Some(prev) = voice.session.read(cx).prev_participants.clone() {
        for _ in shown.roster.iter().filter(|u| !prev.contains(u) && **u != me) {
            play(model, Sound::UserJoin, cx);
        }
        for _ in prev.iter().filter(|u| !current.contains(u) && **u != me) {
            play(model, Sound::UserLeave, cx);
        }
    }
    voice.session.update(cx, |s, _| s.prev_participants = Some(current));
    let participants = shown
        .roster
        .iter()
        .map(|uid| {
            let fallback: SharedString = names.get(uid).map(|n| SharedString::from((*n).clone())).unwrap_or_default();
            Participant { uid: *uid, name: model.name_of(*uid, &fallback, cx), state: shown.state(*uid) }
        })
        .collect();
    voice.participants.update(cx, |p, cx| p.set(participants, cx));
    act::apply_saved_gains(model, shown.roster.iter().copied(), cx);
}

/// The media plane names speakers by uid; 0 is ourselves.
pub fn voice_user_speaking(model: &Model, p: &VoiceUserSpeakingPayload, cx: &mut App) {
    let uid = if p.uid == 0 { model.auth.read(cx).uid } else { p.uid };
    if uid > 0 {
        model.live.voice.speaking.update(cx, |s, cx| s.set(uid, p.speaking, cx));
    }
}

pub fn voice_state_changed(model: &Model, p: &VoiceStateChangedPayload, cx: &mut App) {
    model.live.voice.session.update(cx, |s, cx| s.set_self_state(p.is_muted, p.is_deafened, cx));
}

pub fn voice_user_state_changed(model: &Model, p: &VoiceUserStateChangedPayload, cx: &mut App) {
    if p.uid > 0 {
        model.live.voice.participants.update(cx, |list, cx| list.patch(p.uid, p.is_muted, p.is_deafened, cx));
    }
}

pub fn voice_input_level(model: &Model, p: &VoiceInputLevelPayload, cx: &mut App) {
    model.live.voice.input_level.update(cx, |l, cx| l.set(p.db, cx));
}

pub fn voice_ping_updated(model: &Model, p: &VoicePingUpdatedPayload, cx: &mut App) {
    model.live.voice.stats.update(cx, |s, cx| s.set_latency(p.latency_ms, cx));
}

pub fn voice_connection_stats(model: &Model, p: &VoiceConnectionStatsPayload, cx: &mut App) {
    let at = cx.background_executor().now();
    let sample = StatsSample { at, ping_ms: p.latency_ms, loss_pct: p.packet_loss_pct };
    model.live.voice.stats.update(cx, |s, cx| s.push(sample, cx));
}

pub fn voice_error(model: &Model, p: &VoiceErrorPayload, cx: &mut App) {
    let message: SharedString = p.message.clone().into();
    model.live.voice.session.update(cx, |s, cx| s.set_error(Some(message), cx));
}

/// The MLS group of the connected channel. Keyed by community + channel so a stale state
/// from the previous channel can't paint the new one. The quarantine and failure edges
/// are toasted.
pub fn voice_e2ee_state(model: &Model, p: &VoiceE2eeStatePayload, cx: &mut App) {
    let key = (ServerId::from_wire(&p.server_id), ChannelId::from(p.channel_id.clone()));
    let session = model.live.voice.session.clone();
    if session.read(cx).connected().as_ref() != Some(&key) {
        return;
    }
    let prev = session.read(cx).e2ee.as_ref().map(|e| e.phase);
    let next = VoiceE2ee::from(p);
    let phase = next.phase;
    let unverified = next.unverified.clone();
    session.update(cx, |s, cx| s.set_e2ee(Some(next), cx));
    if phase == VoiceE2eePhase::Quarantine && prev != Some(VoiceE2eePhase::Quarantine) {
        let unknown = SharedString::new_static("Unknown user");
        let names: Vec<String> = unverified.iter().map(|u| model.name_of(*u, &unknown, cx).to_string()).collect();
        let body = format!(
            "{} could not be verified against the key directory. Nothing is sent until they are removed.",
            names.join(", ")
        );
        model.toasts.update(cx, |t, cx| t.warning("Unverified member in voice", Some(body.into()), cx));
    } else if phase == VoiceE2eePhase::Failed && prev != Some(VoiceE2eePhase::Failed) {
        model.toasts.update(cx, |t, cx| {
            t.error("Voice encryption failed", Some("Couldn't join the channel's encryption group. Leave and rejoin.".into()), cx)
        });
    }
}

/// A moderator moved us or disconnected us (`useServerEvents.ts`).
pub fn voice_force_notify(model: &Model, p: &VoiceForceNotifyPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let voice = model.live.voice.clone();
    if voice.session.read(cx).server != Some(server) {
        return;
    }
    let prev = voice.session.read(cx).connected();
    let streaming = voice.own_stream.read(cx).is_live();
    let moved = p.action == "moved";
    // The server stops our stream before switching us (a stream is bound to its
    // channel): reconcile, or the encoder keeps running for a stream nobody knows. Then,
    // when we were disconnected, leave (in that order, as the renderer awaited it).
    cx.spawn(async move |_| {
        use decibell_core::api;
        if streaming {
            let _ = core(api::streaming::stop_screen_share(api::streaming::StopScreenShareArgs {
                server_id: prev.as_ref().map(|k| k.0.wire()),
                channel_id: prev.as_ref().map(|k| k.1.to_string()),
            }))
            .await;
        }
        if !moved && let Err(e) = core(api::voice::leave_voice_channel()).await {
            log::error!("leave_voice_channel: {e}");
        }
    })
    .detach();
    if streaming {
        voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx));
    }
    if moved {
        // Our watches were dropped server-side when we left the old channel.
        voice.watching.update(cx, |w, cx| w.clear_watches(cx));
        // The destination's roster arrived before this notice, while the old channel was
        // still the connected one: rebuild from the per-channel rosters. Not optimistic:
        // the server already moved us. The core rebinds the MLS group itself.
        let channel = ChannelId::from(p.channel_id.clone());
        act::enter_channel(model, (server, channel.clone()), false, false, cx);
        let uids: Vec<Uid> = voice.participants.read(cx).uids().collect();
        act::apply_saved_gains(model, uids, cx);
        let name = model
            .community(server, cx)
            .and_then(|c| c.channels.read(cx).get(&channel).map(|c| c.name.clone()))
            .unwrap_or(channel);
        model.toasts.update(cx, |t, cx| t.info("Moved", Some(format!("{} moved you to {name}.", p.actor).into()), cx));
        return;
    }
    act::disconnect(model, cx);
    if model.nav.read(cx).active_view == ActiveView::Voice {
        model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Server, cx));
    }
    model.toasts.update(cx, |t, cx| t.error("Disconnected", Some(format!("{} disconnected you from voice.", p.actor).into()), cx));
}

/// The streams live in a channel (every connected community, every channel). For the
/// connected channel also the start / stop sounds and dropping watches of streams that
/// ended.
pub fn stream_presence_updated(model: &Model, p: &StreamPresenceUpdatedPayload, cx: &mut App) {
    let key = (ServerId::from_wire(&p.server_id), ChannelId::from(p.channel_id.clone()));
    let voice = model.live.voice.clone();
    let streams: Vec<StreamInfo> = p.streams.iter().map(StreamInfo::from).collect();
    let owners = voice.streams.update(cx, |s, cx| s.apply(key.clone(), streams, cx));
    if voice.session.read(cx).connected().as_ref() != Some(&key) {
        return;
    }
    let me = model.auth.read(cx).uid;
    if let Some(prev) = voice.session.read(cx).prev_stream_owners.clone() {
        for _ in owners.iter().filter(|o| !prev.contains(o) && **o != me) {
            play(model, Sound::StreamStart, cx);
        }
        for _ in prev.iter().filter(|o| !owners.contains(o) && **o != me) {
            play(model, Sound::StreamStop, cx);
        }
    }
    voice.session.update(cx, |s, _| s.prev_stream_owners = Some(owners.clone()));
    let gone: Vec<Uid> = voice.watching.read(cx).list().iter().copied().filter(|w| !owners.contains(w)).collect();
    voice.watching.update(cx, |w, cx| {
        // The focus as it was before the removals (the renderer read it first).
        let focused = w.focused;
        for uid in gone {
            w.remove(uid, cx);
        }
        if focused.is_some_and(|f| !owners.contains(&f)) {
            w.set_focused(None, cx);
            // The app leaves window fullscreen when this flips.
            w.set_fullscreen(false, cx);
        }
    });
    act::fetch_missing_thumbnails(model, cx);
}

/// A streamer switched codec, size or frame rate: an info toast, worded for the streamer
/// or a viewer.
pub fn stream_codec_changed(model: &Model, p: &StreamCodecChangedPayload, cx: &mut App) {
    let we_stream = model.auth.read(cx).is_me(p.streamer_uid);
    let name = model.name_of(p.streamer_uid, &p.streamer_username.clone().into(), cx);
    if let Some(text) =
        codec_change_text(p.reason, VideoCodec::from_wire(p.new_codec), p.new_width, p.new_height, p.new_fps, &name, we_stream)
    {
        model.toasts.update(cx, |t, cx| t.info("Codec changed", Some(text.into()), cx));
    }
}

/// No emitter in the core today; kept as the renderer kept it.
pub fn stream_gpu_fallback(model: &Model, p: &StreamGpuFallbackPayload, cx: &mut App) {
    model.toasts.update(cx, |t, cx| t.warning("GPU encoder unavailable", Some(p.error.clone().into()), cx));
}

/// Our capture lost its source (window closed, monitor unplugged, the desktop's own
/// "stop sharing").
pub fn stream_capture_ended(_model: &Model, cx: &mut App) {
    act::end_stream_from_capture(StreamEnd::Source, true, cx);
}

/// A watcher's keyframe request reached us: the native encoder makes the next frame one.
pub fn keyframe_requested(_model: &Model, cx: &mut App) {
    act::force_keyframe(cx);
}

/// Our native encode loop died while streaming.
pub fn native_stream_failed(_model: &Model, p: &NativeStreamFailedPayload, cx: &mut App) {
    act::end_stream_from_capture(StreamEnd::Failed(p.reason.clone()), true, cx);
}

/// No emitter in the core today: re-read the capabilities if one appears.
pub fn caps_refreshed(_model: &Model, _cx: &mut App) {}

/// A stream's poster, kept while posters are wanted and only for a stream still in the
/// session (the race against a stop that removed it).
pub fn stream_thumbnail(model: &Model, t: StreamThumbnail, cx: &mut App) {
    let voice = model.live.voice.clone();
    if !voice.thumbnails.read(cx).wanted() {
        return;
    }
    let in_session = {
        let s = voice.session.read(cx);
        let (connected, in_call) = (s.connected(), s.call_peer.is_some());
        voice.streams.read(cx).session(connected.as_ref(), in_call).iter().any(|s| s.owner_uid == t.owner_uid)
    };
    if in_session {
        voice.thumbnails.update(cx, |th, cx| th.set(t.owner_uid, t.data, cx));
    }
}

/// Drop one community's voice and stream rosters: all of them, or only the channels not
/// in `keep` (`voiceStore.clearServerPresence`).
pub fn clear_server_presence(model: &Model, server: ServerId, keep: Option<&[Channel]>, cx: &mut App) {
    let keep: Option<HashSet<ChannelId>> = keep.map(|k| k.iter().map(|c| c.id.clone()).collect());
    model.live.voice.rosters.update(cx, |r, cx| r.clear_server(server, keep.as_ref(), cx));
    model.live.voice.streams.update(cx, |s, cx| s.clear_server(server, keep.as_ref(), cx));
}

/// Live bitrate: an admin changed the bitrate of the voice channel we're in: retune the
/// running encoder in place, no rejoin.
pub fn channel_updated(model: &Model, server: ServerId, channel: &Channel, previous_bitrate: Option<i32>, cx: &mut App) {
    if channel.kind == ChannelKind::Voice
        && model.live.voice.session.read(cx).is_connected_to(server, &channel.id)
        && previous_bitrate != Some(channel.voice_bitrate_kbps)
    {
        act::set_voice_bitrate(channel.voice_bitrate_kbps, cx);
    }
}
