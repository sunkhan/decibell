//! P2P DM calls: the signals central relays and the core's three outcomes
//! (`features/call/useCallEvents.ts`).

use decibell_core::events::{CallConnectedPayload, CallDroppedPayload, CallFailedPayload, CallSignalPayload};
use gpui::{App, SharedString};

use crate::Model;
use crate::actions::calls::{self as act, call_stream_id, end_call};
use crate::actions::voice::play;
use crate::live::Sound;
use crate::live::calls::{CallPath, CallStatus, IncomingCall, elsewhere_reason};
use crate::live::voice::codecs::VideoCodec;
use crate::live::voice::streams::StreamInfo;

fn toast_info(model: &Model, title: &'static str, body: String, cx: &mut App) {
    model.toasts.update(cx, |t, cx| t.info(title, Some(body.into()), cx));
}

pub fn call_signal(model: &Model, sig: &CallSignalPayload, cx: &mut App) {
    let from = sig.from_uid;
    if from <= 0 {
        return;
    }
    let me = model.auth.read(cx).uid;
    let (status, peer, mine, call_id, is_caller) = {
        let c = model.live.calls.read(cx);
        (c.status, c.peer, c.is(&sig.call_id), c.call_id.clone(), c.is_caller)
    };
    let name = model.name_of(from, &SharedString::from(sig.from.clone()), cx);
    match sig.kind.as_str() {
        "INVITE" => {
            let Some(pub_key) = sig.pub_key.clone() else { return };
            let incoming = IncomingCall {
                call_id: sig.call_id.clone().into(),
                from_uid: from,
                from: sig.from.clone().into(),
                pub_key,
                candidates: sig.candidates.clone(),
                pub_key_sig: sig.pub_key_sig.clone(),
                key_id: sig.key_id,
            };
            // Glare: both rang each other at once. The lower uid keeps its own INVITE
            // (the other side answers it); the higher drops its own and answers theirs.
            if status == CallStatus::Outgoing && peer == Some(from) {
                if me < from {
                    return;
                }
                if let (Some(call_id), Some(peer)) = (call_id, peer) {
                    act::send(model, call_id, peer, "CANCEL", cx);
                }
                model.live.cues.update(cx, |c, cx| c.stop_ring(cx));
                let transport = model.live.calls.read(cx).transport();
                let end = transport.end();
                cx.spawn(async move |_| {
                    let _ = end.await;
                })
                .detach();
                model.live.calls.update(cx, |c, cx| {
                    c.reset(None, cx);
                    c.set_incoming(incoming, cx);
                });
                act::accept_call(cx);
                return;
            }
            // Central answers BUSY only once no device of ours can ring (multi-device),
            // so this never hides another one.
            if status != CallStatus::Idle {
                act::send(model, sig.call_id.clone().into(), from, "BUSY", cx);
                return;
            }
            act::ring_incoming(model, incoming, cx);
        }
        "RINGING" => {
            if mine && status == CallStatus::Outgoing {
                model.live.calls.update(cx, |c, cx| c.set_ringing_acked(cx));
            }
        }
        "ACCEPT" => {
            if mine && status == CallStatus::Outgoing && let Some(pub_key) = sig.pub_key.clone() {
                let accept = IncomingCall {
                    call_id: sig.call_id.clone().into(),
                    from_uid: from,
                    from: sig.from.clone().into(),
                    pub_key,
                    candidates: sig.candidates.clone(),
                    pub_key_sig: sig.pub_key_sig.clone(),
                    key_id: sig.key_id,
                };
                act::connect_after_accept(model, accept, cx);
            }
        }
        "REJECT" => {
            if mine && status == CallStatus::Outgoing {
                end_call(Some("Declined"), false, cx);
            }
        }
        "BUSY" => {
            if mine && status == CallStatus::Outgoing {
                toast_info(model, "Busy", format!("{} is in another call.", name), cx);
                end_call(Some("Busy"), false, cx);
            }
        }
        "CANCEL" => {
            if mine && status == CallStatus::Incoming {
                model.live.cues.update(cx, |c, cx| c.stop_ring(cx));
                model.live.calls.update(cx, |c, cx| c.reset(Some("Missed call".into()), cx));
                toast_info(model, "Missed call", format!("{} tried to call you.", name), cx);
            }
        }
        "HANGUP" => {
            if mine {
                end_call(Some("Call ended"), false, cx);
            }
        }
        // Another device of ours took the call ringing here, or turned it down.
        "ANSWERED_ELSEWHERE" | "DECLINED_ELSEWHERE" => {
            if !mine {
                return;
            }
            let answered = sig.kind == "ANSWERED_ELSEWHERE";
            let reason = elsewhere_reason(&sig.kind).unwrap_or_default();
            if status == CallStatus::Incoming {
                model.live.cues.update(cx, |c, cx| c.stop_ring(cx));
                model.live.calls.update(cx, |c, cx| c.end_incoming_elsewhere(reason.into(), answered, cx));
            } else if status == CallStatus::Connecting && !is_caller {
                // Our answer lost the race to another device's (or that one declined).
                let body: Option<SharedString> =
                    answered.then(|| format!("Your call with {} goes on there.", name).into());
                model.toasts.update(cx, |t, cx| t.info(reason, body, cx));
                end_call(Some(reason), false, cx);
            }
        }
        "PEER_OFFLINE" => {
            if mine {
                toast_info(model, "Not available", format!("{} is offline.", name), cx);
                end_call(Some("Offline"), false, cx);
            }
        }
        "NOT_ALLOWED" => {
            if mine {
                let body = format!("{} only takes calls from friends.", name);
                model.toasts.update(cx, |t, cx| t.warning("Can't call", Some(body.into()), cx));
                end_call(Some("Not allowed"), false, cx);
            }
        }
        "STREAM_START" => {
            let Some(meta) = sig.stream.as_ref().filter(|_| mine && status == CallStatus::Active) else { return };
            let info = StreamInfo {
                stream_id: call_stream_id(from),
                owner_uid: from,
                owner_name: sig.from.clone().into(),
                has_audio: meta.has_audio,
                width: meta.width,
                height: meta.height,
                fps: meta.fps,
                current_codec: VideoCodec::from_wire(meta.codec),
                enforced_codec: VideoCodec::Unknown,
                watcher_count: 0,
            };
            model.live.voice.streams.update(cx, |s, cx| s.call_started(info, cx));
            play(model, Sound::StreamStart, cx);
        }
        "STREAM_STOP" => {
            if !mine {
                return;
            }
            if model.live.voice.streams.update(cx, |s, cx| s.call_stopped(from, cx)) {
                if model.live.voice.watching.read(cx).is_watching(from) {
                    act::unwatch_call_stream(from, cx);
                }
                play(model, Sound::StreamStop, cx);
            }
        }
        _ => {}
    }
}

pub fn call_connected(model: &Model, p: &CallConnectedPayload, cx: &mut App) {
    let path = if p.path == "srflx" { CallPath::Srflx } else { CallPath::Host };
    act::on_call_connected(model, &p.call_id, path, p.verified, cx);
}

pub fn call_failed(model: &Model, p: &CallFailedPayload, cx: &mut App) {
    if !model.live.calls.read(cx).is(&p.call_id) {
        return;
    }
    let body = if p.reason == "no_path" {
        "One of your networks blocks direct connections (symmetric NAT or a strict firewall). Relay support is planned.".to_string()
    } else if !p.detail.is_empty() {
        p.detail.clone()
    } else {
        p.reason.clone()
    };
    model.toasts.update(cx, |t, cx| t.error("Couldn't connect", Some(body.into()), cx));
    end_call(Some("Couldn't connect"), true, cx);
}

pub fn call_dropped(model: &Model, p: &CallDroppedPayload, cx: &mut App) {
    if !model.live.calls.read(cx).is(&p.call_id) {
        return;
    }
    model.toasts.update(cx, |t, cx| t.warning("Call ended", Some("Lost the connection to the other side.".into()), cx));
    end_call(Some("Connection lost"), true, cx);
}
