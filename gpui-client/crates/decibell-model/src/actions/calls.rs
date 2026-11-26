//! The P2P DM call's imperative half (`features/call/callActions.ts`): what the UI and the
//! signal handler (`events::calls`) trigger. State lives in `live::calls::Calls`; the
//! media session in the voice session + the core (`call_prepare` / `call_connect` /
//! `call_end`); central only relays signals.
//!
//! ```text
//! caller                               callee
//! start_call ─ prepare ─ INVITE ─────▶ incoming, RINGING, ring
//!      ◀─ RINGING                       accept_call ─ prepare
//!      ◀─ ACCEPT {key, candidates} ───  ─ ACCEPT, connect
//! connect (punch)                       (punch)
//!      ◀════ sealed UDP, both sockets ══▶
//! call_connected                        call_connected
//! ```
//!
//! A DM call and a community voice channel are mutually exclusive (one engine, one mic):
//! starting or accepting a call leaves the channel first; joining a channel ends the call.

use decibell_core::api;
use decibell_core::events::{CallCandidatePayload, CallStreamMetaPayload};
use futures::future::LocalBoxFuture;
use gpui::{App, AsyncApp, EventEmitter, SharedString};

use super::voice::{apply_saved_gains, apply_voice_prefs, core, disconnect, play};
use crate::Model;
use crate::error::UiError;
use crate::ids::Uid;
use crate::live::Sound;
use crate::live::calls::{CallPath, CallStatus, CallTransport, Calls, ConnectArgs, IncomingCall, OutSignal, Prepared};
use crate::live::presence::DisplayStatus;
use crate::live::voice::VoiceUserState;
use crate::live::voice::codecs::VideoCodec;
use crate::live::voice::session::Participant;
use crate::live::voice::streams::{LiveStream, OwnStreamPhase, StreamInfo};
use crate::stores::navigation::ActiveView;

/// Asks the DM model to open a conversation (accepting a call lands on the caller's DM):
/// M2's store subscribes (`dmStore.setActiveDmUser`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowConversation(pub Uid);

impl EventEmitter<ShowConversation> for Calls {}

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// The stream id of an in-call screen share (community streams carry the server's).
pub fn call_stream_id(owner: Uid) -> SharedString {
    format!("call:{owner}").into()
}

fn candidate_args(c: &[CallCandidatePayload]) -> Vec<api::call::CallCandidateArg> {
    c.iter()
        .map(|c| api::call::CallCandidateArg { socket: c.socket.clone(), kind: c.kind.clone(), ip: c.ip.clone(), port: c.port })
        .collect()
}

/// [`CallTransport`] over the core's `api::call`.
pub struct CoreCalls;

impl CallTransport for CoreCalls {
    fn config(&self) -> LocalBoxFuture<'static, Result<(bool, Vec<String>), UiError>> {
        Box::pin(async {
            let c = core(api::call::get_call_config()).await?;
            Ok((c.call_signaling, c.stun_servers))
        })
    }

    fn prepare(&self, call_id: SharedString, peer_uid: Uid, peer_name: SharedString) -> LocalBoxFuture<'static, Result<Prepared, UiError>> {
        Box::pin(async move {
            let r = core(api::call::call_prepare(api::call::CallPrepareArgs {
                call_id: call_id.to_string(),
                peer: peer_name.to_string(),
                peer_uid,
            }))
            .await?;
            Ok(Prepared {
                pub_key: r.pub_key,
                candidates: r
                    .candidates
                    .into_iter()
                    .map(|c| CallCandidatePayload { socket: c.socket, kind: c.kind, ip: c.ip, port: c.port })
                    .collect(),
            })
        })
    }

    fn send(&self, s: OutSignal) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async move {
            core(api::call::send_call_signal(api::call::SendCallSignalArgs {
                call_id: s.call_id.to_string(),
                to: s.to_name.to_string(),
                to_uid: s.to_uid,
                kind: s.kind.to_string(),
                pub_key: s.pub_key,
                candidates: s.candidates.as_deref().map(candidate_args),
                stream: s.stream.map(|m| api::call::CallStreamMetaArg {
                    codec: m.codec,
                    width: m.width,
                    height: m.height,
                    fps: m.fps,
                    has_audio: m.has_audio,
                }),
            }))
            .await
        })
    }

    fn connect(&self, a: ConnectArgs) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async move {
            core(api::call::call_connect(api::call::CallConnectArgs {
                call_id: a.call_id.to_string(),
                peer: a.peer_name.to_string(),
                peer_uid: a.peer_uid,
                remote_pub_key: a.remote_pub_key,
                remote_candidates: candidate_args(&a.remote_candidates),
                voice_bitrate_kbps: None,
                remote_pub_key_sig: a.remote_pub_key_sig,
                remote_key_id: a.remote_key_id,
            }))
            .await
        })
    }

    fn end(&self) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async { core(api::call::call_end()).await })
    }

    fn watch_stream(&self, watch: bool) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async move { core(api::call::call_watch_stream(api::call::CallWatchStreamArgs { watch })).await })
    }

    fn new_call_id(&self) -> SharedString {
        let mut b = [0u8; 16];
        let _ = getrandom::getrandom(&mut b);
        // UUID v4 layout, as `crypto.randomUUID()`.
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]).into()
    }
}

/// Send a signal, un-awaited (`sendSignal(...).catch(() => {})`).
pub(crate) fn send(model: &Model, call_id: SharedString, to: Uid, kind: &'static str, cx: &mut App) {
    send_with(model, call_id, to, kind, None, None, None, cx);
}

#[allow(clippy::too_many_arguments)]
fn send_with(
    model: &Model,
    call_id: SharedString,
    to: Uid,
    kind: &'static str,
    pub_key: Option<String>,
    candidates: Option<Vec<CallCandidatePayload>>,
    stream: Option<CallStreamMetaPayload>,
    cx: &mut App,
) {
    let transport = model.live.calls.read(cx).transport();
    let to_name = model.name_of(to, &SharedString::default(), cx);
    let sent = transport.send(OutSignal { call_id, to_uid: to, to_name, kind, pub_key, candidates, stream });
    cx.spawn(async move |_| {
        let _ = sent.await;
    })
    .detach();
}

/// Ask central whether it relays calls (`get_call_config`; the core stores it before it
/// emits `login_succeeded`, so this is race-free there).
pub fn refresh_config(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let config = model.live.calls.read(cx).transport().config();
    cx.spawn(async move |cx| {
        if let Ok((signaling, stun)) = config.await {
            cx.update(|cx| {
                model.live.calls.update(cx, |c, cx| c.set_config(signaling, stun.into_iter().map(Into::into).collect(), cx))
            });
        }
    })
    .detach();
}

/// Leave a community voice channel (and end its stream) so the engine is free for the
/// call. No-op when not in one.
async fn leave_community_voice_if_any(model: &Model, cx: &mut AsyncApp) {
    let (connected, streaming) = cx.update(|cx| {
        (model.live.voice.session.read(cx).connected(), model.live.voice.own_stream.read(cx).is_live())
    });
    let Some(key) = connected else { return };
    if streaming {
        let _ = core(api::streaming::stop_screen_share(api::streaming::StopScreenShareArgs {
            server_id: Some(key.0.wire()),
            channel_id: Some(key.1.to_string()),
        }))
        .await;
        cx.update(|cx| model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx)));
    }
    let _ = core(api::voice::leave_voice_channel()).await;
    cx.update(|cx| disconnect(model, cx));
}

/// Ring `peer`. The answer arrives as signals.
pub fn start_call(peer: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let (status, signaling, transport) = {
        let c = model.live.calls.read(cx);
        (c.status, c.call_signaling, c.transport())
    };
    if status != CallStatus::Idle {
        return;
    }
    if !signaling {
        model.toasts.update(cx, |t, cx| {
            t.warning("Calls unavailable", Some("The server you're signed into doesn't support calls yet.".into()), cx)
        });
        return;
    }
    let me = model.auth.read(cx).uid;
    if me <= 0 || peer <= 0 || me == peer {
        return;
    }
    let call_id = transport.new_call_id();
    model.live.calls.update(cx, |c, cx| c.start_outgoing(call_id.clone(), peer, cx));
    let name = model.name_of(peer, &SharedString::default(), cx);
    cx.spawn(async move |cx| {
        leave_community_voice_if_any(&model, cx).await;
        cx.update(|cx| model.settings.update(cx, |s, _| s.flush()));
        let result: Result<(), UiError> = async {
            let prep = transport.prepare(call_id.clone(), peer, name.clone()).await?;
            if !cx.update(|cx| model.live.calls.read(cx).is(&call_id)) {
                // Cancelled while gathering candidates.
                let _ = transport.end().await;
                return Ok(());
            }
            transport
                .send(OutSignal {
                    call_id: call_id.clone(),
                    to_uid: peer,
                    to_name: name.clone(),
                    kind: "INVITE",
                    pub_key: Some(prep.pub_key),
                    candidates: Some(prep.candidates),
                    stream: None,
                })
                .await?;
            cx.update(|cx| {
                model.live.cues.update(cx, |c, cx| c.start_ring(cx));
                model.live.calls.update(cx, |c, cx| {
                    c.arm_ring(|cx| end_call(Some("No answer"), true, cx), cx);
                });
            });
            Ok(())
        }
        .await;
        if let Err(e) = result {
            cx.update(|cx| model.toasts.update(cx, |t, cx| t.error("Couldn't start the call", Some(e.js_string()), cx)));
            end_call_async(&model, None, false, cx).await;
        }
    })
    .detach();
}

/// Answer the pending INVITE.
pub fn accept_call(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let (incoming, transport) = {
        let c = model.live.calls.read(cx);
        (c.incoming.clone().filter(|_| c.status == CallStatus::Incoming), c.transport())
    };
    let Some(inc) = incoming else { return };
    model.live.cues.update(cx, |c, cx| c.stop_ring(cx));
    model.live.calls.update(cx, |c, cx| {
        c.set_connecting(cx);
        // Land on the caller's DM, where the call panel is.
        cx.emit(ShowConversation(inc.from_uid));
    });
    model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Dm, cx));
    let name = model.name_of(inc.from_uid, &inc.from, cx);
    cx.spawn(async move |cx| {
        leave_community_voice_if_any(&model, cx).await;
        cx.update(|cx| model.settings.update(cx, |s, _| s.flush()));
        let result: Result<(), UiError> = async {
            let prep = transport.prepare(inc.call_id.clone(), inc.from_uid, name.clone()).await?;
            if !cx.update(|cx| model.live.calls.read(cx).is(&inc.call_id)) {
                let _ = transport.end().await;
                return Ok(());
            }
            transport
                .send(OutSignal {
                    call_id: inc.call_id.clone(),
                    to_uid: inc.from_uid,
                    to_name: name.clone(),
                    kind: "ACCEPT",
                    pub_key: Some(prep.pub_key),
                    candidates: Some(prep.candidates),
                    stream: None,
                })
                .await?;
            transport
                .connect(ConnectArgs {
                    call_id: inc.call_id.clone(),
                    peer_uid: inc.from_uid,
                    peer_name: name.clone(),
                    remote_pub_key: inc.pub_key.clone(),
                    remote_candidates: inc.candidates.clone(),
                    remote_pub_key_sig: inc.pub_key_sig.clone(),
                    remote_key_id: (inc.key_id != 0).then_some(inc.key_id),
                })
                .await
        }
        .await;
        if let Err(e) = result {
            cx.update(|cx| model.toasts.update(cx, |t, cx| t.error("Couldn't connect the call", Some(e.js_string()), cx)));
            end_call_async(&model, None, true, cx).await;
        }
    })
    .detach();
}

/// Decline the pending INVITE.
pub fn decline_call(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.live.calls.read(cx).status == CallStatus::Incoming {
        end_call(None, true, cx);
    }
}

/// The caller's side of an ACCEPT: start punching with the callee's key + candidates.
/// `accept` is the callee's ACCEPT, in the same shape as an INVITE.
pub(crate) fn connect_after_accept(model: &Model, accept: IncomingCall, cx: &mut App) {
    let IncomingCall { call_id, from_uid: peer, pub_key, candidates, pub_key_sig, key_id, .. } = accept;
    {
        let c = model.live.calls.read(cx);
        if c.status != CallStatus::Outgoing || !c.is(&call_id) {
            return;
        }
    }
    model.live.cues.update(cx, |c, cx| c.stop_ring(cx));
    model.live.calls.update(cx, |c, cx| c.set_connecting(cx));
    let transport = model.live.calls.read(cx).transport();
    let name = model.name_of(peer, &SharedString::default(), cx);
    let model = model.clone();
    cx.spawn(async move |cx| {
        let connected = transport
            .connect(ConnectArgs {
                call_id,
                peer_uid: peer,
                peer_name: name,
                remote_pub_key: pub_key,
                remote_candidates: candidates,
                remote_pub_key_sig: pub_key_sig,
                remote_key_id: (key_id != 0).then_some(key_id),
            })
            .await;
        if let Err(e) = connected {
            cx.update(|cx| model.toasts.update(cx, |t, cx| t.error("Couldn't connect the call", Some(e.js_string()), cx)));
            end_call_async(&model, None, true, cx).await;
        }
    })
    .detach();
}

/// The core reports the punch succeeded and the P2P engine is up.
pub(crate) fn on_call_connected(model: &Model, call_id: &str, path: CallPath, verified: bool, cx: &mut App) {
    let peer = {
        let c = model.live.calls.read(cx);
        if !c.is(call_id) {
            return;
        }
        let Some(peer) = c.peer else { return };
        peer
    };
    let me = model.auth.read(cx).uid;
    let now = cx.background_executor().now();
    model.live.calls.update(cx, |c, cx| c.set_active(path, verified, now, cx));
    model.live.voice.session.update(cx, |s, cx| s.enter_call(peer, cx));
    let participants = [me, peer]
        .into_iter()
        .map(|uid| Participant { uid, name: model.name_of(uid, &SharedString::default(), cx), state: VoiceUserState::default() })
        .collect();
    model.live.voice.participants.update(cx, |p, cx| p.set(participants, cx));
    play(model, Sound::Connect, cx);
    apply_voice_prefs(model, cx);
    // A call has no presence to replay saved gains on: do it here.
    apply_saved_gains(model, [peer], cx);
}

/// End whatever call state we're in: ringing out, ringing in, punching, or active.
/// Idempotent. `reason` shows briefly in the DM panel.
pub fn end_call(reason: Option<&'static str>, notify_peer: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    cx.spawn(async move |cx| end_call_async(&model, reason, notify_peer, cx).await).detach();
}

pub(crate) async fn end_call_async(model: &Model, reason: Option<&str>, notify_peer: bool, cx: &mut AsyncApp) {
    let reason: Option<SharedString> = reason.map(|r| r.to_string().into());
    let (was_live, transport, streaming) = cx.update(|cx| {
        let (status, call_id, peer, transport) = {
            let c = model.live.calls.read(cx);
            (c.status, c.call_id.clone(), c.peer, c.transport())
        };
        if status == CallStatus::Idle {
            return (None, transport, false);
        }
        model.live.cues.update(cx, |c, cx| c.stop_ring(cx));
        let kind = match status {
            CallStatus::Outgoing => "CANCEL",
            CallStatus::Incoming => "REJECT",
            _ => "HANGUP",
        };
        if notify_peer && let (Some(call_id), Some(peer)) = (call_id, peer) {
            send(model, call_id, peer, kind, cx);
        }
        model.live.calls.update(cx, |c, cx| c.reset(reason.clone(), cx));
        let live = matches!(status, CallStatus::Active | CallStatus::Connecting);
        (Some(live), transport, model.live.voice.own_stream.read(cx).is_live())
    });
    let Some(was_live) = was_live else { return };
    if was_live && streaming {
        let _ = core(api::streaming::stop_screen_share(api::streaming::StopScreenShareArgs { server_id: None, channel_id: None })).await;
        cx.update(|cx| model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx)));
    }
    let _ = transport.end().await;
    if was_live {
        cx.update(|cx| {
            disconnect(model, cx);
            play(model, Sound::CallEnd, cx);
        });
    }
}

/// An incoming INVITE (`useCallEvents`' INVITE branch, minus glare): ring every device of
/// ours — silently under Do Not Disturb (owner decision: the call still shows; unanswered
/// it is a missed call).
pub(crate) fn ring_incoming(model: &Model, call: crate::live::calls::IncomingCall, cx: &mut App) {
    let (call_id, from_uid, from) = (call.call_id.clone(), call.from_uid, call.from.clone());
    model.live.calls.update(cx, |c, cx| c.set_incoming(call, cx));
    send(model, call_id.clone(), from_uid, "RINGING", cx);
    if model.live.presence.own.read(cx).display() != DisplayStatus::Dnd {
        model.live.cues.update(cx, |c, cx| {
            c.start_ring(cx);
            c.flash(cx);
        });
    }
    let m = model.clone();
    model.live.calls.update(cx, |c, cx| {
        c.arm_ring(
            move |cx| {
                m.live.cues.update(cx, |c, cx| c.stop_ring(cx));
                m.live.calls.update(cx, |c, cx| c.reset(Some("Missed call".into()), cx));
                let name = m.name_of(from_uid, &from, cx);
                m.toasts.update(cx, |t, cx| t.info("Missed call", Some(format!("{name} tried to call you.").into()), cx));
            },
            cx,
        )
    });
}

// ── In-call screen share ────────────────────────────────────────────────────

/// Our stream started inside the active call: tell the peer and list our own entry (the
/// self-preview tile). No-op outside a call.
pub(crate) fn announce_stream_start(model: &Model, live: &LiveStream, cx: &mut App) {
    let (call_id, peer) = {
        let c = model.live.calls.read(cx);
        if c.status != CallStatus::Active {
            return;
        }
        let (Some(id), Some(peer)) = (c.call_id.clone(), c.peer) else { return };
        (id, peer)
    };
    let meta = CallStreamMetaPayload {
        codec: live.codec.wire() as i32,
        width: live.width,
        height: live.height,
        fps: live.fps,
        has_audio: live.has_audio,
    };
    send_with(model, call_id, peer, "STREAM_START", None, None, Some(meta), cx);
    let me = model.auth.read(cx).uid;
    let name = model.auth.read(cx).username.clone().unwrap_or_default();
    let info = StreamInfo {
        stream_id: call_stream_id(me),
        owner_uid: me,
        owner_name: name,
        has_audio: live.has_audio,
        width: live.width,
        height: live.height,
        fps: live.fps,
        current_codec: live.codec,
        enforced_codec: VideoCodec::Unknown,
        watcher_count: 0,
    };
    model.live.voice.streams.update(cx, |s, cx| s.call_started(info, cx));
}

/// Our stream ended inside the active call. No-op outside a call.
pub(crate) fn announce_stream_stop(model: &Model, cx: &mut App) {
    let (call_id, peer) = {
        let c = model.live.calls.read(cx);
        if c.status != CallStatus::Active {
            return;
        }
        let (Some(id), Some(peer)) = (c.call_id.clone(), c.peer) else { return };
        (id, peer)
    };
    send(model, call_id, peer, "STREAM_STOP", cx);
    let me = model.auth.read(cx).uid;
    model.live.voice.streams.update(cx, |s, cx| {
        s.call_stopped(me, cx);
    });
    model.live.voice.watching.update(cx, |w, cx| w.remove(me, cx));
}

/// Open `owner`'s stream in the DM's full view (the peer's is gated through natively).
pub fn watch_call_stream(owner: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let me = model.auth.read(cx).uid;
    if owner != me && !model.live.voice.watching.read(cx).is_watching(owner) {
        let watch = model.live.calls.read(cx).transport().watch_stream(true);
        cx.spawn(async move |_| {
            let _ = watch.await;
        })
        .detach();
    }
    model.live.voice.watching.update(cx, |w, cx| {
        w.add(owner, cx);
        w.set_focused(Some(owner), cx);
    });
}

/// Leave `owner`'s stream (full view + mini player).
pub fn unwatch_call_stream(owner: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if owner != model.auth.read(cx).uid {
        let watch = model.live.calls.read(cx).transport().watch_stream(false);
        cx.spawn(async move |_| {
            let _ = watch.await;
        })
        .detach();
    }
    model.live.voice.watching.update(cx, |w, cx| w.remove(owner, cx));
}
