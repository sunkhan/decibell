//! Voice and streams: joining, leaving, switching, mute / deafen, per-user volume, the
//! audio settings, watching streams, Go Live, codec probes (`features/voice/voiceActions.ts`,
//! `streaming/joinVoiceChannel.ts`, `streaming/applyVoicePrefs.ts`, `streaming/streamEnded.ts`,
//! `stage/streamActions.ts`, `userGain.ts`, `streamAudioFilter.ts`, `useStreamThumbnails.ts`,
//! `CaptureSourcePicker.tsx`'s Go Live, `codecSettingsStore.ts`, `encoderProbe.ts`,
//! `voiceStore.enterChannel` / `disconnect`).

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use decibell_core::api::{self, CoreError};
use futures::future::LocalBoxFuture;
use gpui::{App, AsyncApp, SharedString, Task};

use crate::Model;
use crate::call::call;
use crate::error::UiError;
use crate::ids::{ChannelId, ServerId, Uid};
use crate::live::voice::codecs::{self, CodecCap, DecodeReport, EncoderCap, VideoCodec};
use crate::live::voice::devices::{AudioDevice, DeviceList, StreamAudioApp};
use crate::live::voice::gain;
use crate::live::voice::go_live::{StreamAudioMode, StreamSettings, normalize_app_id};
use crate::live::voice::rosters::{VoiceKey, VoiceUserState};
use crate::live::voice::session::Participant;
use crate::live::voice::streams::{LiveStream, OwnStreamPhase};
use crate::live::Sound;
use crate::stores::navigation::ActiveView;
use crate::stores::permissions::{has_bits, perm};

/// Run a core call from async code: "not running" instead of calling into a core that
/// was never started (the core panics on a call before `init`).
pub(crate) async fn core<T>(request: impl Future<Output = Result<T, CoreError>>) -> Result<T, UiError> {
    if !crate::core::is_running() {
        return Err(UiError::not_running());
    }
    request.await.map_err(UiError::from)
}

/// A fire-and-forget core call whose failure is only logged (`.catch(console.error)`).
pub(crate) fn fire(what: &'static str, request: impl Future<Output = Result<(), CoreError>> + 'static, cx: &mut App) {
    call(cx, request, move |result, _| {
        if let Err(e) = result {
            log::debug!("{what}: {e}");
        }
    });
}

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

pub(crate) fn play(model: &Model, sound: Sound, cx: &mut App) {
    model.live.cues.update(cx, |c, cx| c.play(sound, cx));
}

// ── Wiring the stores to the core ───────────────────────────────────────────

/// `InputLevel`'s switch: the core's `voice_input_level` reporting.
pub(crate) fn set_level_reporting(on: bool) {
    api::voice::set_input_level_reporting(api::voice::SetInputLevelReportingArgs { enabled: on });
}

/// `AudioDevices`' switch: the core's device notifications (`audio_devices_changed`, from
/// the platform's own; its listener thread exists only while they are on). Nothing to
/// switch without a core (screens drawn from fixtures).
pub(crate) fn watch_audio_devices(on: bool) -> bool {
    if !crate::core::is_running() {
        return false;
    }
    api::audio::watch_devices(on);
    true
}

pub(crate) fn list_audio_devices() -> LocalBoxFuture<'static, Result<DeviceList, UiError>> {
    Box::pin(async {
        let list = core(api::settings::list_audio_devices()).await?;
        let map = |d: api::settings::AudioDevice| AudioDevice { name: d.name.into(), label: d.label.into() };
        Ok((list.inputs.into_iter().map(map).collect(), list.outputs.into_iter().map(map).collect()))
    })
}

pub(crate) fn list_stream_audio_apps(
    source_id: Option<String>,
) -> LocalBoxFuture<'static, Result<(bool, Vec<StreamAudioApp>), UiError>> {
    Box::pin(async move {
        let list = core(api::stream_audio::list_stream_audio_apps(api::stream_audio::ListStreamAudioAppsArgs { source_id })).await?;
        let apps = list
            .apps
            .into_iter()
            .map(|a| StreamAudioApp { id: a.id.into(), name: a.name.into(), active: a.active, owns_window_source: a.owns_window_source })
            .collect();
        Ok((list.supported, apps))
    })
}

// ── The session ─────────────────────────────────────────────────────────────

/// Make `(server, channel)` the connected channel (`voiceStore.enterChannel`): a join, a
/// switch, or a moderator's move. The old channel's session state (MLS state, speaking
/// rings) goes and the participants are seeded from the channel's roster in the same
/// update, so nothing of the old channel shows under the new one. `optimistic` (our own
/// join or switch) also shows us in the new channel and out of the old one before the
/// server's broadcasts do, `suppressed` being our no-SPEAK badge meanwhile.
pub fn enter_channel(model: &Model, key: VoiceKey, optimistic: bool, suppressed: bool, cx: &mut App) {
    let me = model.auth.read(cx).uid;
    let voice = model.live.voice.clone();
    let (from, muted, deafened) = {
        let s = voice.session.read(cx);
        (s.connected(), s.muted, s.deafened)
    };
    let changed = from.as_ref() != Some(&key);
    if optimistic && changed && me > 0 {
        // A moderator's mute / deafen is the member's: it follows us.
        let was = from.as_ref().and_then(|f| voice.rosters.read(cx).channel(f).and_then(|c| c.states.get(&me).copied()));
        let self_state = VoiceUserState {
            muted,
            deafened,
            server_muted: was.is_some_and(|w| w.server_muted),
            server_deafened: was.is_some_and(|w| w.server_deafened),
            suppressed,
        };
        let now = cx.background_executor().now();
        voice.rosters.update(cx, |r, cx| r.queue_switch(from.as_ref(), &key, me, self_state, now, cx));
    }
    voice.session.update(cx, |s, cx| {
        s.enter(&key, cx);
        // The join / leave and stream sounds compare against the new channel from here.
        s.prev_participants = None;
        s.prev_stream_owners = None;
    });
    seed_participants(model, &key, cx);
    if changed {
        // Speaking rings come from the engine: a new one starts silent; one that carries
        // on through a move doesn't announce us again while we talk.
        let keep = (!optimistic && me > 0).then_some(me);
        voice.speaking.update(cx, |s, cx| s.reset(keep, cx));
        voice.thumbnails.update(cx, |t, _| t.requested.clear());
    }
    let roster: HashSet<Uid> = voice.rosters.read(cx).roster(&key).iter().copied().collect();
    let owners: HashSet<Uid> = voice.streams.read(cx).in_channel(&key).map(|s| s.owner_uid).collect();
    voice.session.update(cx, |s, _| {
        s.prev_participants = Some(roster);
        s.prev_stream_owners = Some(owners);
    });
    fetch_missing_thumbnails(model, cx);
}

/// The participants of `key` as the rosters show them.
pub(crate) fn seed_participants(model: &Model, key: &VoiceKey, cx: &mut App) {
    let list: Vec<Participant> = {
        let rosters = model.live.voice.rosters.read(cx);
        let Some(channel) = rosters.channel(key) else {
            model.live.voice.participants.update(cx, |p, cx| p.clear(cx));
            return;
        };
        channel
            .roster
            .iter()
            .map(|uid| Participant { uid: *uid, name: model.name_of(*uid, &SharedString::default(), cx), state: channel.state(*uid) })
            .collect()
    };
    model.live.voice.participants.update(cx, |p, cx| p.set(list, cx));
}

/// Leave the session's state (`voiceStore.disconnect`): out of the channel's roster now
/// (a no-op when the server already took us out: a moderator's disconnect, a DM call),
/// the session, its participants, speaking rings, watches, posters and stats go. Every
/// channel's roster and stream list stays: they mirror the communities, not our session.
pub fn disconnect(model: &Model, cx: &mut App) {
    let me = model.auth.read(cx).uid;
    let voice = model.live.voice.clone();
    voice.stats.update(cx, |s, cx| s.clear(cx));
    let (leaving, muted, deafened) = {
        let s = voice.session.read(cx);
        (s.connected().filter(|_| me > 0), s.muted, s.deafened)
    };
    if let Some(key) = leaving {
        let now = cx.background_executor().now();
        let state = VoiceUserState { muted, deafened, ..Default::default() };
        voice.rosters.update(cx, |r, cx| r.queue_leave(&key, me, state, now, cx));
    }
    voice.thumbnails.update(cx, |t, cx| t.clear(cx));
    voice.session.update(cx, |s, cx| s.disconnect(cx));
    voice.participants.update(cx, |p, cx| p.clear(cx));
    voice.speaking.update(cx, |s, cx| s.reset(None, cx));
    voice.watching.update(cx, |w, cx| w.reset(cx));
    voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx));
    voice.streams.update(cx, |s, cx| s.clear_call(cx));
    voice.rosters.update(cx, |r, _| r.clear_capabilities());
}

/// Sidebar: a voice channel clicked (`handleVoiceChannelClick`). The one we're in just
/// shows the voice view; without CONNECT_VOICE (the server's resolved bits; none sent =
/// no gate) it's refused with a toast; otherwise join and show the voice view at once.
pub fn click_voice_channel(server: ServerId, channel: ChannelId, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.live.voice.session.read(cx).is_connected_to(server, &channel) {
        model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Voice, cx));
        return;
    }
    let my_permissions = model.community(server, cx).and_then(|c| c.channels.read(cx).get(&channel).map(|c| c.my_permissions));
    if my_permissions.is_some_and(|p| p != 0 && !has_bits(p, perm::CONNECT_VOICE)) {
        model.toasts.update(cx, |t, cx| t.error("You don't have permission to join this voice channel.", None, cx));
        return;
    }
    join_voice_channel(server, channel, cx).detach();
    model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Voice, cx));
}

/// Join (or switch to) a voice channel (`joinVoiceChannel`). Strict E2EE: without keys
/// loaded the passphrase prompt opens instead. A DM call is hung up first (one engine,
/// one mic). The switch shows at once; the core is called after. A mid-stream switch
/// ends the stream, or carries it along within one community when "Take stream with me"
/// is on. A failed join puts us back out of voice and is only logged, as in the renderer.
pub fn join_voice_channel(server: ServerId, channel: ChannelId, cx: &mut App) -> Task<Result<(), UiError>> {
    let Some(model) = model(cx) else { return Task::ready(Ok(())) };
    if !voice_keys_ready(&model, cx) {
        return Task::ready(Ok(()));
    }
    let call_active = model.live.calls.read(cx).status != crate::live::calls::CallStatus::Idle;
    cx.spawn(async move |cx| {
        if call_active {
            super::calls::end_call_async(&model, Some("Joined a voice channel"), true, cx).await;
        }
        let plan = cx.update(|cx| begin_join(&model, server, channel.clone(), cx));
        let JoinPlan { prev, switching, was_streaming, take_along, bitrate } = plan;
        if was_streaming && switching && !take_along {
            stop_stream_engine(prev.as_ref()).await;
            cx.update(|cx| model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx)));
        }
        // The core reads config.json to pick the devices when the engine starts: a device
        // changed inside the save debounce must be on disk first.
        super::settings::flush_saved(&model, cx).await;
        let joined = core(api::voice::join_voice_channel(api::voice::JoinVoiceChannelArgs {
            server_id: server.wire(),
            channel_id: channel.to_string(),
            voice_bitrate_kbps: bitrate,
        }))
        .await;
        if let Err(e) = joined {
            log::error!("join_voice_channel: {e}");
            if was_streaming {
                stop_stream_engine(prev.as_ref()).await;
                cx.update(|cx| model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx)));
            }
            cx.update(|cx| disconnect(&model, cx));
            return Err(e);
        }
        cx.update(|cx| apply_voice_prefs(&model, cx));
        if take_along {
            carry_stream(&model, server, channel, cx).await;
        }
        Ok(())
    })
}

/// "No keys → no voice": the voice-channel join path's E2EE gate. Opens the prompt (or
/// says the keys are still loading) and returns false when the keys aren't ready.
pub fn voice_keys_ready(model: &Model, cx: &mut App) -> bool {
    use crate::live::e2ee::{E2eeStatus, PassphraseMode};
    let status = model.live.e2ee.read(cx).status;
    match status {
        E2eeStatus::Ready => true,
        E2eeStatus::Unavailable => {
            model.toasts.update(cx, |t, cx| {
                t.warning("Voice unavailable", Some("Your encryption keys are still loading. Try again in a moment.".into()), cx)
            });
            false
        }
        E2eeStatus::Locked | E2eeStatus::NotSetUp => {
            let locked = status == E2eeStatus::Locked;
            let title = if locked { "Unlock encryption to join voice" } else { "Set up encryption to join voice" };
            model.toasts.update(cx, |t, cx| {
                t.info(title, Some("Voice channels are end-to-end encrypted with your identity keys.".into()), cx)
            });
            let mode = if locked { PassphraseMode::Unlock } else { PassphraseMode::Setup };
            model.live.e2ee.update(cx, |e, cx| e.open(mode, cx));
            false
        }
    }
}

struct JoinPlan {
    prev: Option<VoiceKey>,
    switching: bool,
    was_streaming: bool,
    take_along: bool,
    bitrate: Option<i32>,
}

/// The synchronous half of a join: capture the stream state, show the switch, stop the
/// old channel's watches.
fn begin_join(model: &Model, server: ServerId, channel: ChannelId, cx: &mut App) -> JoinPlan {
    let voice = model.live.voice.clone();
    let prev = voice.session.read(cx).connected();
    // Channel ids are slugs: the same id on another server is a switch too.
    let switching = prev.as_ref().is_some_and(|p| p.0 != server || p.1 != channel);
    let was_streaming = voice.own_stream.read(cx).is_live() && prev.is_some();
    // Only a same-community switch while streaming, and only when opted in.
    let take_along = was_streaming
        && switching
        && prev.as_ref().is_some_and(|p| p.0 == server)
        && model.settings.read(cx).get().take_stream_on_channel_switch;
    let ch = model.community(server, cx).and_then(|c| c.channels.read(cx).get(&channel).cloned());
    // Without SPEAK there the server shows a "No speak" badge on us: predict it.
    let suppressed = ch.as_ref().is_some_and(|c| !has_bits(c.my_permissions, perm::SPEAK));
    play(model, Sound::Connect, cx);
    enter_channel(model, (server, channel), true, suppressed, cx);
    if switching && let Some(p) = &prev {
        stop_watches_in(model, p, cx);
        voice.watching.update(cx, |w, cx| w.clear_watches(cx));
    }
    JoinPlan { prev, switching, was_streaming, take_along, bitrate: ch.map(|c| c.voice_bitrate_kbps) }
}

/// Best-effort `stop_watching` for every watched stream of `key` but our own (un-awaited:
/// leaving drops the watches server-side anyway).
fn stop_watches_in(model: &Model, key: &VoiceKey, cx: &mut App) {
    let me = model.auth.read(cx).uid;
    let watching: Vec<Uid> = model.live.voice.watching.read(cx).list().to_vec();
    for uid in watching.into_iter().filter(|u| *u != me) {
        let name = model.name_of(uid, &SharedString::default(), cx).to_string();
        fire(
            "stop_watching",
            api::streaming::stop_watching(api::streaming::StopWatchingArgs {
                server_id: key.0.wire(),
                channel_id: key.1.to_string(),
                target_uid: uid,
                target_username: Some(name),
            }),
            cx,
        );
    }
}

/// Stop our screen share's engine, announced in `key` (none: wherever it is).
async fn stop_stream_engine(key: Option<&VoiceKey>) {
    let _ = core(api::streaming::stop_screen_share(api::streaming::StopScreenShareArgs {
        server_id: key.map(|k| k.0.wire()),
        channel_id: key.map(|k| k.1.to_string()),
    }))
    .await;
}

/// Carry the live stream into the channel just joined (`StreamCapture.retarget`): the
/// core re-points the senders and re-announces; capture and the encoder keep running.
/// On failure the stream ends.
async fn carry_stream(model: &Model, server: ServerId, channel: ChannelId, cx: &mut AsyncApp) {
    let (live, settings) = cx.update(|cx| {
        (model.live.voice.own_stream.read(cx).live().cloned(), StreamSettings::of(model.settings.read(cx).get()))
    });
    let Some(live) = live else {
        cx.update(|cx| model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx)));
        return;
    };
    let moved = core(api::streaming::move_stream_to_channel(api::streaming::MoveStreamToChannelArgs {
        server_id: server.wire(),
        channel_id: channel.to_string(),
        fps: live.fps,
        width: live.width,
        height: live.height,
        video_bitrate_kbps: settings.video_bitrate_kbps,
        share_audio: live.has_audio,
        initial_codec: live.codec.wire(),
        // The enforcement it was announced with at the start (Auto: none), not the codec
        // it encodes in: a carry must not lock an Auto stream.
        enforced_codec: live.enforced.wire(),
    }))
    .await;
    match moved {
        Ok(()) => cx.update(|cx| {
            model.live.voice.own_stream.update(cx, |o, cx| {
                o.set_phase(OwnStreamPhase::Live(LiveStream { server: Some(server), channel: Some(channel), ..live }), cx)
            })
        }),
        Err(e) => {
            log::error!("[joinVoiceChannel] failed to carry stream to new channel: {e}");
            let key = (server, channel);
            stop_stream_engine(Some(&key)).await;
            cx.update(|cx| model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx)));
        }
    }
}

/// Push the preferences a freshly started engine does not read from the config itself:
/// the voice-activity threshold, the DSP switches, the separate stream output
/// (`applyVoicePrefs`). Shared by the community join and the DM call. The devices are
/// not re-sent: the engine opened the saved ones (a re-send was an audible pop).
pub fn apply_voice_prefs(model: &Model, cx: &mut App) {
    let s = model.settings.read(cx).get().clone();
    let threshold = s.voice_threshold_db.unwrap_or(-50.0);
    fire(
        "set_voice_threshold",
        api::voice::set_voice_threshold(api::voice::SetVoiceThresholdArgs {
            threshold_db: if threshold <= -60.0 { -96.0 } else { threshold },
        }),
        cx,
    );
    if s.separate_stream_output {
        // Enabling the split rebuilds the voice output on the default device: the chosen
        // devices go after it.
        let (output, stream_output) = (s.output_device.clone(), s.stream_output_device.clone());
        cx.spawn(async move |_| {
            let _ = core(api::voice::set_separate_stream_output(api::voice::SetSeparateStreamOutputArgs {
                enabled: true,
                device: stream_output.clone(),
            }))
            .await;
            let _ = core(api::voice::set_output_device(api::voice::SetOutputDeviceArgs { name: output })).await;
            let _ = core(api::voice::set_stream_output_device(api::voice::SetStreamOutputDeviceArgs { name: stream_output })).await;
        })
        .detach();
    }
    fire("set_aec_enabled", api::voice::set_aec_enabled(api::voice::SetAecEnabledArgs { enabled: s.aec_enabled }), cx);
    fire(
        "set_noise_suppression_level",
        api::voice::set_noise_suppression_level(api::voice::SetNoiseSuppressionLevelArgs { level: s.noise_suppression_level }),
        cx,
    );
    fire("set_agc_enabled", api::voice::set_agc_enabled(api::voice::SetAgcEnabledArgs { enabled: s.agc_enabled }), cx);
}

// ── Mic, headphones, leaving ────────────────────────────────────────────────

/// The mic button. Unmuting while deafened also undeafens (Discord's rule).
pub fn toggle_mute(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let (muted, deafened) = {
        let s = model.live.voice.session.read(cx);
        (s.muted, s.deafened)
    };
    if deafened {
        play(&model, Sound::Undeafen, cx);
        set_deafen(false, cx);
        set_mute(false, cx);
    } else {
        play(&model, if muted { Sound::Unmute } else { Sound::Mute }, cx);
        set_mute(!muted, cx);
    }
}

/// The headphones button.
pub fn toggle_deafen(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let deafened = model.live.voice.session.read(cx).deafened;
    play(&model, if deafened { Sound::Undeafen } else { Sound::Deafen }, cx);
    set_deafen(!deafened, cx);
}

/// Our mute, without the sound (the core keeps it for the next session; the answer is
/// `voice_state_changed`).
pub fn set_mute(muted: bool, cx: &mut App) {
    fire("set_voice_mute", api::voice::set_voice_mute(api::voice::SetVoiceMuteArgs { muted }), cx);
}

pub fn set_deafen(deafened: bool, cx: &mut App) {
    fire("set_voice_deafen", api::voice::set_voice_deafen(api::voice::SetVoiceDeafenArgs { deafened }), cx);
}

/// Leave the community voice channel (`leaveVoiceChannel`): our stream ends first (the
/// dock that could stop it goes with the session), watches are dropped (best effort),
/// the core leaves, the session goes, and the server view shows.
pub fn leave_voice_channel(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    play(&model, Sound::Disconnect, cx);
    let connected = model.live.voice.session.read(cx).connected();
    let streaming = model.live.voice.own_stream.read(cx).is_live();
    if let Some(key) = &connected {
        stop_watches_in(&model, key, cx);
    }
    let key = connected.clone();
    cx.spawn(async move |_| {
        if streaming {
            stop_stream_engine(key.as_ref()).await;
        }
        if let Err(e) = core(api::voice::leave_voice_channel()).await {
            log::error!("leave_voice_channel: {e}");
        }
    })
    .detach();
    disconnect(&model, cx);
    model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Server, cx));
}

/// The leave hotkey: hang up (or cancel) a DM call, else leave the voice channel. A
/// ringing incoming call is decline's job.
pub fn leave_voice_or_call(cx: &mut App) {
    use crate::live::calls::CallStatus;
    let Some(model) = model(cx) else { return };
    match model.live.calls.read(cx).status {
        CallStatus::Outgoing => super::calls::end_call(Some("Cancelled"), true, cx),
        CallStatus::Connecting | CallStatus::Active => super::calls::end_call(Some("Call ended"), true, cx),
        _ if model.live.voice.session.read(cx).channel.is_some() => leave_voice_channel(cx),
        _ => {}
    }
}

/// A moderator's action on a member in voice (permissions v3: MOVE_MEMBERS in their
/// channel, VIEW on the destination; the server checks and refuses through
/// `mod_action_responded`). `action`: "server_mute" | "server_unmute" | "server_deafen"
/// | "server_undeafen" | "move" | "disconnect".
pub fn voice_mod(server: ServerId, uid: Uid, action: &'static str, to_channel: Option<ChannelId>, cx: &mut App) {
    // The user menu's wording (`UserContextMenu.tsx`).
    voice_mod_or_toast(server, uid, action, to_channel, "Voice moderation failed", cx);
}

/// A member dragged onto another voice channel (`memberDrag.ts`): the "move" of [`voice_mod`],
/// with the drag's own failure toast.
pub fn drag_move_member(server: ServerId, uid: Uid, to_channel: ChannelId, cx: &mut App) {
    voice_mod_or_toast(server, uid, "move", Some(to_channel), "Couldn't move member", cx);
}

fn voice_mod_or_toast(
    server: ServerId,
    uid: Uid,
    action: &'static str,
    to_channel: Option<ChannelId>,
    failure: &'static str,
    cx: &mut App,
) {
    crate::call::call_or_toast(
        cx,
        failure,
        api::community::voice_mod(api::community::VoiceModArgs {
            server_id: server.wire(),
            username: None,
            uid,
            action: action.to_string(),
            channel_id: to_channel.map(|c| c.to_string()),
        }),
    );
}

// ── Per-user volume ─────────────────────────────────────────────────────────

/// Push `uid`'s effective gain to the engine (no-op without a session: the saved value
/// is replayed when one starts).
pub fn push_user_gain(model: &Model, uid: Uid, cx: &mut App) {
    if uid <= 0 || !model.live.voice.session.read(cx).media_session_active() {
        return;
    }
    let g = gain::effective_gain(model.settings.read(cx).get(), uid);
    fire("set_user_volume", api::voice::set_user_volume(api::voice::SetUserVolumeArgs { uid, gain: g }), cx);
}

/// Replay saved volume / local mute for every listed user that has one (a roster change,
/// a call connecting): someone muted earlier stays muted.
pub fn apply_saved_gains(model: &Model, uids: impl IntoIterator<Item = Uid>, cx: &mut App) {
    let saved: Vec<Uid> = {
        let s = model.settings.read(cx).get();
        uids.into_iter().filter(|u| gain::has_saved_gain(s, *u)).collect()
    };
    for uid in saved {
        push_user_gain(model, uid, cx);
    }
}

/// The user context menu's volume slider (dB, saved).
pub fn set_user_volume(uid: Uid, db: f64, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.settings.update(cx, |s, cx| s.change(|v| gain::set_user_db(v, uid, db), cx));
    push_user_gain(&model, uid, cx);
}

/// The user context menu's "Mute" (local, saved).
pub fn toggle_local_mute(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.settings.update(cx, |s, cx| {
        s.change(|v| {
            gain::toggle_local_mute(v, uid);
        }, cx)
    });
    push_user_gain(&model, uid, cx);
}

// ── Audio settings (Settings → Audio) ───────────────────────────────────────

fn change(model: &Model, f: impl FnOnce(&mut decibell_core::config::AppSettings), cx: &mut App) {
    model.settings.update(cx, |s, cx| s.change(f, cx));
}

pub fn set_input_device(name: Option<String>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.input_device = name.clone(), cx);
    fire("set_input_device", api::voice::set_input_device(api::voice::SetInputDeviceArgs { name }), cx);
}

pub fn set_output_device(name: Option<String>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.output_device = name.clone(), cx);
    fire("set_output_device", api::voice::set_output_device(api::voice::SetOutputDeviceArgs { name }), cx);
}

pub fn set_separate_stream_output(enabled: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.separate_stream_output = enabled, cx);
    let device = model.settings.read(cx).get().stream_output_device.clone();
    fire(
        "set_separate_stream_output",
        api::voice::set_separate_stream_output(api::voice::SetSeparateStreamOutputArgs { enabled, device }),
        cx,
    );
}

pub fn set_stream_output_device(name: Option<String>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.stream_output_device = name.clone(), cx);
    fire("set_stream_output_device", api::voice::set_stream_output_device(api::voice::SetStreamOutputDeviceArgs { name }), cx);
}

pub fn set_stream_stereo(enabled: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.stream_stereo = enabled, cx);
    fire("set_stream_stereo", api::voice::set_stream_stereo(api::voice::SetStreamStereoArgs { enabled }), cx);
}

pub fn set_aec_enabled(enabled: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.aec_enabled = enabled, cx);
    fire("set_aec_enabled", api::voice::set_aec_enabled(api::voice::SetAecEnabledArgs { enabled }), cx);
}

pub fn set_noise_suppression_level(level: u8, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.noise_suppression_level = level, cx);
    fire(
        "set_noise_suppression_level",
        api::voice::set_noise_suppression_level(api::voice::SetNoiseSuppressionLevelArgs { level }),
        cx,
    );
}

pub fn set_agc_enabled(enabled: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    change(&model, |s| s.agc_enabled = enabled, cx);
    fire("set_agc_enabled", api::voice::set_agc_enabled(api::voice::SetAgcEnabledArgs { enabled }), cx);
}

/// The threshold slider (dB, −60..0, half-dB steps). The core takes −96 for the floor
/// ("always open"). Saved through the settings debounce, so a drag is one write.
pub fn set_voice_threshold(db: f64, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let db = ((db.clamp(-60.0, 0.0)) * 2.0).round() / 2.0;
    change(&model, |s| s.voice_threshold_db = Some(db), cx);
    fire(
        "set_voice_threshold",
        api::voice::set_voice_threshold(api::voice::SetVoiceThresholdArgs { threshold_db: if db <= -60.0 { -96.0 } else { db } }),
        cx,
    );
}

/// Retune the running encoder for a new channel bitrate (`m3::channel_updated`).
pub fn set_voice_bitrate(kbps: i32, cx: &mut App) {
    fire("set_voice_bitrate", api::voice::set_voice_bitrate(api::voice::SetVoiceBitrateArgs { bitrate_kbps: kbps }), cx);
}

// ── Watching streams ────────────────────────────────────────────────────────

/// Subscribe to `uid`'s stream if we aren't already. Our own stream is the self-preview
/// (the core fans our encoder's frames out locally): never a server watch.
fn start_watching(model: &Model, uid: Uid, cx: &mut App) -> bool {
    let Some(key) = model.live.voice.session.read(cx).connected() else { return false };
    if !model.live.voice.watching.read(cx).is_watching(uid) {
        if uid != model.auth.read(cx).uid {
            let name = model.name_of(uid, &SharedString::default(), cx).to_string();
            fire(
                "watch_stream",
                api::streaming::watch_stream(api::streaming::WatchStreamArgs {
                    server_id: key.0.wire(),
                    channel_id: key.1.to_string(),
                    target_uid: uid,
                    target_username: Some(name),
                }),
                cx,
            );
        }
        model.live.voice.watching.update(cx, |w, cx| w.add(uid, cx));
    }
    true
}

/// Watch (if needed) and show it large; other watched streams keep playing.
pub fn focus_stream(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if start_watching(&model, uid, cx) {
        model.live.voice.watching.update(cx, |w, cx| w.set_focused(Some(uid), cx));
    }
}

/// Watch alongside what already plays, staying on the grid.
pub fn watch_stream_too(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    start_watching(&model, uid, cx);
}

/// Stop every other watched stream, then focus this one.
pub fn switch_to_stream(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let others: Vec<Uid> = model.live.voice.watching.read(cx).list().iter().copied().filter(|u| *u != uid).collect();
    for other in others {
        stop_watching_stream(other, cx);
    }
    focus_stream(uid, cx);
}

pub fn stop_watching_stream(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if uid != model.auth.read(cx).uid
        && let Some(key) = model.live.voice.session.read(cx).connected()
    {
        let name = model.name_of(uid, &SharedString::default(), cx).to_string();
        fire(
            "stop_watching",
            api::streaming::stop_watching(api::streaming::StopWatchingArgs {
                server_id: key.0.wire(),
                channel_id: key.1.to_string(),
                target_uid: uid,
                target_username: Some(name),
            }),
            cx,
        );
    }
    model.live.voice.watching.update(cx, |w, cx| w.remove(uid, cx));
}

/// A view started drawing `uid`'s stream: the first one switches the core's frame sink
/// on, so the video hub receives only what is displayed.
pub fn show_stream(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.live.voice.stream_views.update(cx, |v, _| v.retain(uid)) && crate::core::is_running() {
        api::streaming::set_stream_frame_sink(api::streaming::SetStreamFrameSinkArgs { key: uid.to_string(), enabled: true });
    }
}

/// The view stopped drawing it: the last one switches the sink off.
pub fn hide_stream(uid: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.live.voice.stream_views.update(cx, |v, _| v.release(uid)) && crate::core::is_running() {
        api::streaming::set_stream_frame_sink(api::streaming::SetStreamFrameSinkArgs { key: uid.to_string(), enabled: false });
    }
}

/// The voice view mounted: keep posters (and fetch the missing ones) while it is shown.
pub fn retain_thumbnails(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.live.voice.thumbnails.update(cx, |t, _| t.retain()) {
        fetch_missing_thumbnails(&model, cx);
    }
}

pub fn release_thumbnails(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.voice.thumbnails.update(cx, |t, _| t.release());
}

/// Any session stream without a poster gets one fetch of the server's cached copy, once
/// per stream per channel visit (posters are pushed every 15 s; a newcomer would stare at
/// an avatar meanwhile).
pub fn fetch_missing_thumbnails(model: &Model, cx: &mut App) {
    if !model.live.voice.thumbnails.read(cx).wanted() {
        return;
    }
    let Some(key) = model.live.voice.session.read(cx).connected() else { return };
    let missing: Vec<(Uid, SharedString)> = {
        let thumbs = model.live.voice.thumbnails.read(cx);
        model
            .live
            .voice
            .streams
            .read(cx)
            .in_channel(&key)
            .filter(|s| thumbs.get(s.owner_uid).is_none() && !thumbs.requested.contains(&s.stream_id))
            .map(|s| (s.owner_uid, s.stream_id.clone()))
            .collect()
    };
    for (owner, stream_id) in missing {
        model.live.voice.thumbnails.update(cx, |t, _| t.requested.insert(stream_id.clone()));
        let (model, server) = (model.clone(), key.0);
        call(
            cx,
            api::streaming::fetch_stream_thumbnail(api::streaming::FetchStreamThumbnailArgs { server_id: server.wire(), uid: owner }),
            move |result, cx| {
                let Ok(res) = result else { return };
                if res.jpeg.is_empty() {
                    return; // nothing cached yet: a push follows
                }
                // A push may have landed meanwhile: it's at least as new.
                let still = model.live.voice.thumbnails.read(cx).get(owner).is_none()
                    && model.live.voice.session.read(cx).connected().is_some_and(|k| {
                        model.live.voice.streams.read(cx).in_channel(&k).any(|s| s.stream_id == stream_id)
                    });
                if still {
                    model.live.voice.thumbnails.update(cx, |t, cx| t.set(owner, res.jpeg, cx));
                }
            },
        );
    }
}

/// The profile popup's live preview: the server's cached poster of `uid`'s stream.
pub fn fetch_stream_thumbnail(server: ServerId, uid: Uid, cx: &mut App, done: impl FnOnce(Option<Vec<u8>>, &mut App) + 'static) {
    call(
        cx,
        api::streaming::fetch_stream_thumbnail(api::streaming::FetchStreamThumbnailArgs { server_id: server.wire(), uid }),
        move |result, cx| done(result.ok().map(|r| r.jpeg).filter(|j| !j.is_empty()), cx),
    );
}

// ── Go Live ─────────────────────────────────────────────────────────────────

/// Change the Go Live settings (saved through `Settings`).
pub fn update_stream_settings(f: impl FnOnce(&mut StreamSettings), cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let mut st = StreamSettings::of(model.settings.read(cx).get());
    f(&mut st);
    change(&model, |s| st.store(s), cx);
}

/// While live, the same filter goes to the running capture so it re-plans at once.
fn push_live_audio_filter(model: &Model, cx: &mut App) {
    if !model.live.voice.own_stream.read(cx).is_live() {
        return;
    }
    let st = StreamSettings::of(model.settings.read(cx).get());
    fire(
        "set_stream_audio_filter",
        api::stream_audio::set_stream_audio_filter(api::stream_audio::SetStreamAudioFilterArgs {
            mode: st.audio_mode.as_setting().to_string(),
            apps: st.audio_apps,
        }),
        cx,
    );
}

pub fn set_stream_audio_mode(mode: StreamAudioMode, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    update_stream_settings(|s| s.audio_mode = mode, cx);
    push_live_audio_filter(&model, cx);
}

pub fn toggle_stream_audio_app(id: &str, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let norm = normalize_app_id(id);
    if norm.is_empty() {
        return;
    }
    update_stream_settings(
        |s| {
            if let Some(i) = s.audio_apps.iter().position(|a| *a == norm) {
                s.audio_apps.remove(i);
            } else {
                s.audio_apps.push(norm);
            }
        },
        cx,
    );
    push_live_audio_filter(&model, cx);
}

/// Whether this session can pick stream-audio apps (native capture is the only path
/// here; macOS has no per-app stream audio).
pub fn can_pick_stream_audio_apps() -> bool {
    !cfg!(target_os = "macos")
}

/// Go Live (`handleGoLive` on the native path): the core opens capture (the portal's
/// picker on Linux; `OwnStream::source_id` on Windows) and the encoder, and announces
/// the stream in the connected channel, or to the DM call's peer. The dialog shows
/// `OwnStream::error` on refusal.
pub fn go_live(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let (key, in_call) = {
        let s = model.live.voice.session.read(cx);
        (s.connected(), s.call_peer.is_some())
    };
    if key.is_none() && !in_call {
        return;
    }
    let (include_cursor, source_id, attempt) = model.live.voice.own_stream.update(cx, |o, cx| {
        o.attempt += 1;
        o.set_error(None, cx);
        o.set_phase(OwnStreamPhase::Starting, cx);
        (o.include_cursor, o.source_id.clone(), o.attempt)
    });
    if cfg!(target_os = "windows") && source_id.is_none() {
        model.live.voice.own_stream.update(cx, |o, cx| {
            o.set_error(Some("Pick a screen or window to share first.".into()), cx);
            o.set_phase(OwnStreamPhase::Idle, cx);
        });
        return;
    }
    let st = StreamSettings::of(model.settings.read(cx).get());
    let share_audio = can_pick_stream_audio_apps() && st.share_audio;
    // Auto doesn't pin the codec, so a machine without a hardware encoder for it still
    // streams, in software (`StreamSettings::start_codecs`).
    let first_probed = model.live.voice.codecs.read(cx).encoders.first().map(|e| e.codec);
    let (initial, enforced) = st.start_codecs(first_probed);
    let (width, height) = st.resolution.request_size();
    let args = api::streaming::StartScreenShareArgs {
        server_id: key.as_ref().map(|k| k.0.wire()),
        channel_id: key.as_ref().map(|k| k.1.to_string()),
        fps: st.fps,
        width,
        height,
        video_bitrate_kbps: st.video_bitrate_kbps,
        share_audio,
        audio_bitrate_kbps: st.audio_bitrate_kbps,
        initial_codec: initial.wire(),
        enforced_codec: enforced.wire(),
        source_id,
        native_encode: Some(true),
        include_cursor: Some(include_cursor),
        detail: Some(st.detail()),
        audio_mode: Some(st.audio_mode.as_setting().to_string()),
        audio_apps: Some(st.audio_apps.clone()),
    };
    let fps = st.fps;
    let unnamed = st.start_codec();
    call(cx, api::streaming::start_screen_share(args), move |result, cx| {
        let current = model.live.voice.own_stream.read(cx).attempt == attempt
            && model.live.voice.own_stream.read(cx).phase == OwnStreamPhase::Starting;
        match result {
            Ok(size) if current => {
                let live = LiveStream {
                    // What the stream really encodes in, as the core reports it (Auto's
                    // pick, or the software encoder an unpinned start fell through to).
                    codec: match VideoCodec::from_wire(size.codec as i32) {
                        VideoCodec::Unknown => unnamed,
                        started => started,
                    },
                    enforced,
                    width: size.width,
                    height: size.height,
                    fps,
                    has_audio: share_audio,
                    encoder: size.encoder.into(),
                    server: key.as_ref().map(|k| k.0),
                    channel: key.as_ref().map(|k| k.1.clone()),
                };
                model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Live(live.clone()), cx));
                super::calls::announce_stream_start(&model, &live, cx);
                play(&model, Sound::StreamStart, cx);
            }
            Ok(_) => {
                // The session ended while starting (a stop, a failure event): don't
                // resurrect it as live.
                cx.spawn(async move |_| stop_stream_engine(key.as_ref()).await).detach();
                model.live.voice.own_stream.update(cx, |o, cx| {
                    o.set_error(Some("Stream ended while starting".into()), cx)
                });
            }
            Err(e) => model.live.voice.own_stream.update(cx, |o, cx| {
                o.set_error(Some(e.js_string()), cx);
                o.set_phase(OwnStreamPhase::Idle, cx);
            }),
        }
    });
}

/// "Stop sharing" (the user panel, the dock): the stream ends and is announced as ended.
pub fn stop_stream(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    play(&model, Sound::StreamStop, cx);
    let key = model.live.voice.session.read(cx).connected();
    cx.spawn(async move |_| stop_stream_engine(key.as_ref()).await).detach();
    model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx));
    super::calls::announce_stream_stop(&model, cx);
}

/// Why a stream ended without the user pressing Stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEnd {
    /// The shared window closed, the monitor went, the desktop's "stop sharing".
    Source,
    /// The capture or the encoder broke (the core's reason, logged).
    Failed(String),
}

/// The one end-of-stream path for an end the user didn't ask for (`endStreamFromCapture`):
/// the engine is stopped, the LIVE state clears, fullscreen of our own stream is left, a
/// DM-call peer hears STREAM_STOP, and the user sees why.
pub fn end_stream_from_capture(end: StreamEnd, notify: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if let StreamEnd::Failed(reason) = &end {
        log::error!("[stream] stream failed: {reason}");
    }
    let was_live = !matches!(model.live.voice.own_stream.read(cx).phase, OwnStreamPhase::Idle);
    cx.spawn(async move |_| stop_stream_engine(None).await).detach();
    if !was_live {
        return;
    }
    model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx));
    let me = model.auth.read(cx).uid;
    model.live.voice.watching.update(cx, |w, cx| {
        if w.focused == Some(me) && w.fullscreen {
            w.set_focused(None, cx);
            w.set_fullscreen(false, cx);
        }
    });
    super::calls::announce_stream_stop(&model, cx);
    play(&model, Sound::StreamStop, cx);
    if !notify {
        return;
    }
    model.toasts.update(cx, |t, cx| match end {
        StreamEnd::Source => t.info("Stream ended", Some("The shared screen or window is no longer available.".into()), cx),
        StreamEnd::Failed(_) => {
            t.error("Stream stopped", Some("The GPU encoder or screen capture stopped. Try going live again.".into()), cx)
        }
    });
}

// ── Codec capabilities ──────────────────────────────────────────────────────

fn cap_values(caps: &[CodecCap]) -> Vec<api::streaming::CodecCapValue> {
    caps.iter()
        .map(|c| api::streaming::CodecCapValue {
            codec: c.codec.wire(),
            max_width: c.max_width,
            max_height: c.max_height,
            max_fps: c.max_fps,
        })
        .collect()
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
async fn probe_encoders() -> Result<Vec<EncoderCap>, UiError> {
    let raw = core(api::streaming::probe_native_encoders()).await?;
    Ok(raw
        .into_iter()
        .map(|c| EncoderCap {
            codec: VideoCodec::from_wire(c.codec),
            max_width: c.max_width,
            max_height: c.max_height,
            max_fps: c.max_fps,
            hardware: c.hardware,
            backend: c.backend.into(),
            label: c.label.into(),
            max_pixel_rate: c.max_pixel_rate,
        })
        .collect())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
async fn probe_encoders() -> Result<Vec<EncoderCap>, UiError> {
    Ok(Vec::new())
}

/// The re-probe after a cached boot (the cache used to live forever, so a GPU swap kept
/// offering codecs that no longer opened): once, this long after the load.
pub const ENCODER_REPROBE_DELAY: Duration = Duration::from_secs(15);

/// Ask the decoder probe (the video crate's: a real probe, off the UI thread, kept per GPU
/// and driver), keep its answer and ship it to the core. Returns what is advertised.
async fn refresh_decode_caps(model: &Model, force: bool, cx: &mut AsyncApp) -> Vec<CodecCap> {
    let probe = cx.update(|cx| model.live.voice.codecs.read(cx).decoder_probe.clone());
    let report = match &probe {
        Some(p) => p.probe(force).await,
        None => DecodeReport::default(),
    };
    let (decode, encode) = cx.update(|cx| {
        model.live.voice.codecs.update(cx, |c, cx| {
            c.set_decode(report, cx);
            // The advertised encoders follow what decodes: shipped again once they are loaded.
            (c.decode.clone(), c.loaded.then(|| c.encode.clone()))
        })
    });
    let _ = core(api::streaming::set_decoder_caps(api::streaming::SetDecoderCapsArgs { decoder_caps: cap_values(&decode) })).await;
    if let Some(encode) = encode {
        let _ = core(api::streaming::set_encoder_caps(api::streaming::SetEncoderCapsArgs { encoder_caps: cap_values(&encode) })).await;
    }
    decode
}

/// The decode capabilities alone: what the app runs once its first frame is on screen (and
/// again when the GPU device was replaced or a decoder was given up), so a voice join
/// advertises what was really probed. Cheap when nothing changed (the video crate keeps
/// the answer); opens no encoder.
pub fn load_decode_caps(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    cx.spawn(async move |cx| {
        refresh_decode_caps(&model, false, cx).await;
    })
    .detach();
}

/// Ship the decode capabilities the store holds to the core (its caps are in memory: after
/// every start of the core). Nothing while the store has none.
pub fn ship_decode_caps(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let decode = model.live.voice.codecs.read(cx).decode.clone();
    if decode.is_empty() {
        return;
    }
    fire(
        "set_decoder_caps",
        async move {
            api::streaming::set_decoder_caps(api::streaming::SetDecoderCapsArgs { decoder_caps: cap_values(&decode) }).await.map(|_| ())
        },
        cx,
    );
}

/// The one loader of the codec capabilities: the decode probe's answer, the encoders (the
/// `ui.json` cache, else the core's probe, off the UI thread), the encoders we can also
/// decode shipped to the core with the decode caps, and Settings → Codecs' toggles.
/// `force`: Settings' "Refresh codec capabilities" (both probes run again).
///
/// Never at start: the encoder probe opens encoder sessions. Whoever needs the caps first
/// calls this (the first voice session, the Go Live dialog, Settings → Codecs); callers
/// while a load runs share it, and a forced one asked for meanwhile runs after it.
pub fn init_codec_caps(force: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if !model.live.voice.codecs.update(cx, |c, cx| c.begin_load(force, cx)) {
        return;
    }
    let cached: Option<Vec<EncoderCap>> =
        (!force).then(|| model.layout.read(cx).extra(codecs::ENCODER_CAPS_KEY)).flatten().filter(|v: &Vec<EncoderCap>| !v.is_empty());
    model.layout.update(cx, |l, cx| {
        for key in codecs::STALE_CAPS_KEYS {
            l.set_extra::<()>(key, None, cx);
        }
    });
    cx.spawn(async move |cx| {
        refresh_decode_caps(&model, force, cx).await;
        let was_cached = cached.is_some();
        let encoders = match cached {
            Some(caps) => caps,
            None => match probe_encoders().await {
                Ok(caps) => {
                    cx.update(|cx| model.layout.update(cx, |l, cx| l.set_extra(codecs::ENCODER_CAPS_KEY, Some(&caps), cx)));
                    caps
                }
                Err(e) => {
                    log::error!("[encoderProbe/native] probe failed: {e}");
                    Vec::new()
                }
            },
        };
        let toggles = core(api::streaming::get_codec_settings()).await;
        let (encode, again) = cx.update(|cx| {
            model.live.voice.codecs.update(cx, |c, cx| {
                if let Ok(t) = &toggles {
                    c.set_toggles(t.use_av1, t.use_h265, cx);
                }
                c.set_encoders(encoders, cx);
                (c.encode.clone(), c.end_load(cx))
            })
        });
        let _ = core(api::streaming::set_encoder_caps(api::streaming::SetEncoderCapsArgs { encoder_caps: cap_values(&encode) })).await;
        if again {
            cx.update(|cx| init_codec_caps(true, cx));
            return;
        }
        if was_cached {
            // This session keeps the caps it loaded; the next one gets the fresh probe.
            cx.background_executor().timer(ENCODER_REPROBE_DELAY).await;
            if let Ok(fresh) = probe_encoders().await {
                cx.update(|cx| model.layout.update(cx, |l, cx| l.set_extra(codecs::ENCODER_CAPS_KEY, Some(&fresh), cx)));
            }
        }
    })
    .detach();
}

/// Settings → Codecs' AV1 / H.265 switches (`setUseAv1` / `setUseH265`): optimistic, the
/// core persists them and re-filters its advertisement; rolled back on failure.
pub fn set_codec_toggles(use_av1: bool, use_h265: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let (prev_av1, prev_h265) = {
        let c = model.live.voice.codecs.read(cx);
        (c.use_av1, c.use_h265)
    };
    model.live.voice.codecs.update(cx, |c, cx| c.set_toggles(use_av1, use_h265, cx));
    call(
        cx,
        api::streaming::set_codec_settings(api::streaming::CodecSettingsValue { use_av1, use_h265 }),
        move |result, cx| {
            if let Err(e) = result {
                log::error!("[codecSettings] {e}");
                model.live.voice.codecs.update(cx, |c, cx| c.set_toggles(prev_av1, prev_h265, cx));
                model.toasts.update(cx, |t, cx| t.error("Failed to update codec setting", None, cx));
            }
        },
    );
}

/// `keyframe_requested`: the native encoder owns keyframes (`force_keyframe`).
pub fn force_keyframe(cx: &mut App) {
    fire("force_keyframe", api::streaming::force_keyframe(), cx);
}
