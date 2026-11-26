//! The voice feature's deterministic fixtures, on top of the shell's (`decibell_shell::fixture`:
//! perf1 signed in to the stack's three communities): the core events a voice channel with N
//! people and some live streams would deliver, fed through the model's real router, plus the
//! user's own actions (join, watch, focus, Go Live open). [`json`] exports the same for the
//! Electron capture pages (`tools/electron-captures/pages/voice-*.tsx`).

use std::rc::Rc;

use decibell_core::events::*;
use decibell_model::live::calls::{CallTransport, ConnectArgs, OutSignal, Prepared};
use decibell_model::live::voice::rosters::VoiceKey;
use decibell_model::live::voice::go_live::STREAM_PRESETS;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{ChannelId, Model, ServerId, UiError, actions};
use decibell_shell::fixture::{self as shell, DAILY_GAMING, ME, ME_NAME};
use futures::future::LocalBoxFuture;
use gpui::{App, SharedString};

use decibell_model::live::voice::{CodecCap, VideoCodec};

use crate::codecs::{EncoderCap, Encoders};
use crate::state::VoiceUi;

/// The posters of the two fixture streams (640 × 360 JPEG, made once with ImageMagick: see
/// `fixtures/README`).
pub const POSTER_CAROL: &[u8] = include_bytes!("../fixtures/poster-carol.jpg");
pub const POSTER_BOB: &[u8] = include_bytes!("../fixtures/poster-bob.jpg");

/// The voice channel the scenes are in.
pub const CHANNEL: &str = "lounge";

/// Who is in voice, in roster order (beyond us): (name, uid, muted, deafened, server muted).
const PEOPLE: [(&str, i64, bool, bool, bool); 24] = [
    ("alice", 1, false, false, false),
    ("bob", 2, true, false, false),
    ("carol", 3, true, true, false),
    ("dave", 4, false, false, true),
    ("perf2", 7, false, false, false),
    ("erin", 5, false, false, false),
    ("perf3", 8, true, false, false),
    ("perf4", 9, false, false, false),
    ("aurora", -1000, false, false, false),
    ("blaze", -1001, false, false, false),
    ("cinder", -1002, true, false, false),
    ("drift", -1003, false, false, false),
    ("ember", -1004, false, false, false),
    ("flux", -1005, false, false, false),
    ("glimmer", -1006, false, true, false),
    ("halo", -1007, false, false, false),
    ("iris", -1008, false, false, false),
    ("jolt", -1009, false, false, false),
    ("kestrel", -1010, false, false, false),
    ("lumen", -1011, false, false, false),
    ("mosaic", -1012, false, false, false),
    ("nova", -1013, false, false, false),
    ("onyx", -2000, false, false, false),
    ("pixel", -2001, false, false, false),
];

/// What a voice screen shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scene {
    /// `people` in the channel (us included), `streams` live (carol's, then bob's), the first
    /// `watched` of them watched.
    Stage { people: usize, streams: usize, watched: usize },
    /// Carol's stream focused, bob's live in the filmstrip, four people.
    Focus,
    /// Carol's stream watched and loaded, back in `#general`: the mini-player floats.
    Mini,
    /// The Go Live dialog open on a preset (`smooth` / `sharp` / `slides` / `saver`), Advanced
    /// open or not, with the probe's answer: hardware (NVENC) or software only (SVT-AV1).
    GoLive { preset: &'static str, advanced: bool, software: bool },
    /// perf2 calling us: the incoming card over `#general`.
    CallIncoming,
    /// In a call with perf2 (connected, verified, 38 ms over the LAN), their DM open.
    CallStage,
}

fn state(name: &str, uid: i64, muted: bool, deafened: bool, server_muted: bool) -> VoiceUserStatePayload {
    VoiceUserStatePayload {
        username: name.into(),
        uid,
        is_muted: muted,
        is_deafened: deafened,
        is_server_muted: server_muted,
        is_server_deafened: false,
        is_suppressed: false,
    }
}

fn roster(channel: &str, people: &[(&str, i64, bool, bool, bool)]) -> CoreEvent {
    CoreEvent::VoicePresenceUpdated(VoicePresenceUpdatedPayload {
        server_id: DAILY_GAMING.to_string(),
        channel_id: channel.into(),
        participants: people.iter().map(|p| p.0.to_string()).collect(),
        participant_uids: people.iter().map(|p| p.1).collect(),
        user_states: people.iter().map(|p| state(p.0, p.1, p.2, p.3, p.4)).collect(),
        user_capabilities: Vec::new(),
    })
}

fn stream(owner: (&str, i64), w: u32, h: u32, fps: u32, codec: u8, audio: bool, watchers: u32) -> StreamInfoPayload {
    StreamInfoPayload {
        stream_id: format!("{}_screen", owner.1),
        owner_username: owner.0.into(),
        owner_uid: owner.1,
        has_audio: audio,
        resolution_width: w,
        resolution_height: h,
        fps,
        current_codec: codec as i32,
        enforced_codec: 0,
        watcher_count: watchers,
    }
}

/// The live streams: carol's 1080p60 H.264 with sound, bob's 1440p120 AV1.
fn streams(count: usize) -> CoreEvent {
    let all = [stream(("carol", 3), 1920, 1080, 60, 1, true, 2), stream(("bob", 2), 2560, 1440, 120, 4, false, 1)];
    CoreEvent::StreamPresenceUpdated(StreamPresenceUpdatedPayload {
        server_id: DAILY_GAMING.to_string(),
        channel_id: CHANNEL.into(),
        streams: all.into_iter().take(count).collect(),
    })
}

/// The scene's people (us first, as the server lists the joiner last: we follow the roster's
/// order, us included at the end).
fn people(n: usize) -> Vec<(&'static str, i64, bool, bool, bool)> {
    let mut list: Vec<_> = PEOPLE.iter().copied().take(n.saturating_sub(1)).collect();
    list.push((ME_NAME, ME, false, false, false));
    list
}

/// The scene's (people, streams, watched).
fn shape(scene: Scene) -> (usize, usize, usize) {
    match scene {
        Scene::Stage { people, streams, watched } => (people, streams, watched),
        Scene::Focus => (4, 2, 1),
        Scene::Mini => (4, 1, 1),
        Scene::GoLive { .. } => (1, 0, 0),
        Scene::CallIncoming | Scene::CallStage => (0, 0, 0),
    }
}

/// The voice events of a scene, after the shell's.
pub fn events(scene: Scene) -> Vec<CoreEvent> {
    let (n, s, _) = shape(scene);
    if n == 0 {
        return Vec::new();
    }
    let mut events = vec![
        // We left Squad (perf2 stays) and are in Lounge.
        roster("squad", &[("perf2", 7, false, false, false)]),
        roster(CHANNEL, &people(n)),
        streams(s),
        CoreEvent::VoiceStateChanged(VoiceStateChangedPayload { is_muted: false, is_deafened: false }),
        CoreEvent::VoicePingUpdated(VoicePingUpdatedPayload { latency_ms: 38 }),
        CoreEvent::VoiceE2eeState(VoiceE2eeStatePayload {
            server_id: DAILY_GAMING.to_string(),
            channel_id: CHANNEL.into(),
            state: "ready".into(),
            epoch: 4,
            members: n as u32,
            unverified: Vec::new(),
        }),
    ];
    // Somebody visible is talking (alice; with one person, nobody).
    if n > 1 {
        events.push(CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid: 1, speaking: true }));
    }
    events
}

/// A call transport whose steps never finish (the screens show a call's states without a core).
struct StillTransport;

impl CallTransport for StillTransport {
    fn config(&self) -> LocalBoxFuture<'static, Result<(bool, Vec<String>), UiError>> {
        Box::pin(async { Ok((true, Vec::new())) })
    }
    fn prepare(&self, _: SharedString, _: i64, _: SharedString) -> LocalBoxFuture<'static, Result<Prepared, UiError>> {
        Box::pin(futures::future::pending())
    }
    fn send(&self, _: OutSignal) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(futures::future::pending())
    }
    fn connect(&self, _: ConnectArgs) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(futures::future::pending())
    }
    fn end(&self) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(futures::future::pending())
    }
    fn watch_stream(&self, _: bool) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(futures::future::pending())
    }
    fn new_call_id(&self) -> SharedString {
        "fixture-call".into()
    }
}

fn invite() -> CoreEvent {
    CoreEvent::CallSignal(CallSignalPayload {
        kind: "INVITE".into(),
        call_id: "fixture-call".into(),
        from: "perf2".into(),
        from_uid: 7,
        to: ME_NAME.into(),
        pub_key: Some("AAAA".into()),
        candidates: Vec::new(),
        stream: None,
        timestamp: 0,
        pub_key_sig: None,
        key_id: 0,
    })
}

/// The probe's answer the Go Live screens show.
pub fn encoder_caps(software: bool) -> Vec<EncoderCap> {
    let cap = |codec, backend: &str, label: &str, hardware, rate| EncoderCap {
        codec,
        hardware,
        backend: SharedString::from(backend.to_string()),
        label: SharedString::from(label.to_string()),
        max_width: 3840,
        max_height: 2160,
        max_fps: if hardware { 120 } else { 60 },
        max_pixel_rate: rate,
    };
    use decibell_model::live::voice::VideoCodec::*;
    if software {
        // SVT-AV1 on this many cores: 1080p at 30 fps.
        vec![cap(Av1, "software", "Software (SVT-AV1)", false, 1920 * 1080 * 30)]
    } else {
        vec![
            cap(H264Hw, "nvenc", "NVENC (NVIDIA)", true, 0),
            cap(H265, "nvenc", "NVENC (NVIDIA)", true, 0),
            cap(Av1, "nvenc", "NVENC (NVIDIA)", true, 0),
        ]
    }
}

/// Fill the model (made without a core) with the shell's fixture and the scene.
pub fn apply(scene: Scene, cx: &mut App) -> Model {
    let model = shell::apply(shell::Scene::Server, cx);
    let server = ServerId::from_central(DAILY_GAMING);
    let lounge: VoiceKey = (server, ChannelId::from(CHANNEL));
    let (n, _, watched) = shape(scene);
    if n > 0 {
        actions::voice::enter_channel(&model, lounge, false, false, cx);
        for event in events(scene) {
            decibell_model::events::route(&model, event, cx);
        }
        // The posters of streams we don't watch.
        let thumbs = model.live.voice.thumbnails.clone();
        thumbs.update(cx, |t, cx| {
            t.set(3, POSTER_CAROL.to_vec(), cx);
            t.set(2, POSTER_BOB.to_vec(), cx);
        });
        for owner in [3, 2].into_iter().take(watched) {
            actions::voice::watch_stream_too(owner, cx);
        }
        model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Voice, cx));
    }
    let encoders = Encoders::global(cx);
    let software = matches!(scene, Scene::GoLive { software: true, .. });
    let caps = encoder_caps(software);
    // What Advanced's codec row lists: the model's codec store (what the core advertises).
    let encode: Vec<CodecCap> = caps
        .iter()
        .map(|c| CodecCap { codec: c.codec, max_width: c.max_width, max_height: c.max_height, max_fps: c.max_fps, hardware: c.hardware })
        .collect();
    let decode: Vec<CodecCap> = [VideoCodec::H264Hw, VideoCodec::H265, VideoCodec::Av1]
        .into_iter()
        .map(|codec| CodecCap { codec, max_width: 3840, max_height: 2160, max_fps: 120, hardware: true })
        .collect();
    model.live.voice.codecs.update(cx, |c, cx| {
        c.set_toggles(true, true, cx);
        c.set_caps(encode, decode, cx);
    });
    encoders.update(cx, |e, cx| e.pin(caps, cx));
    match scene {
        Scene::Focus => actions::voice::focus_stream(3, cx),
        Scene::Mini => {
            actions::voice::focus_stream(3, cx);
            // Back to the grid, then to #general: the loaded stream floats.
            model.live.voice.watching.update(cx, |w, cx| w.set_focused(None, cx));
            model.live.voice.watching.update(cx, |w, cx| w.set_pip(Some(3), cx));
            model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Server, cx));
        }
        Scene::GoLive { preset, advanced, .. } => {
            if let Some(p) = STREAM_PRESETS.iter().find(|p| p.id == preset) {
                actions::voice::update_stream_settings(|s| s.apply_preset(p), cx);
            }
            let ui = VoiceUi::global(cx);
            ui.update(cx, |u, cx| {
                u.advanced_on_open = Some(advanced);
                u.open_go_live(cx)
            });
        }
        Scene::CallIncoming => {
            model.live.calls.update(cx, |c, _| c.set_transport(Rc::new(StillTransport)));
            decibell_model::events::route(&model, invite(), cx);
        }
        Scene::CallStage => {
            model.live.calls.update(cx, |c, _| c.set_transport(Rc::new(StillTransport)));
            decibell_model::events::route(&model, invite(), cx);
            actions::calls::accept_call(cx);
            decibell_model::events::route(
                &model,
                CoreEvent::CallConnected(CallConnectedPayload {
                    call_id: "fixture-call".into(),
                    rtt_ms: 2.,
                    path: "host".into(),
                    verified: true,
                }),
                cx,
            );
            for event in [
                CoreEvent::VoiceStateChanged(VoiceStateChangedPayload { is_muted: false, is_deafened: false }),
                CoreEvent::VoicePingUpdated(VoicePingUpdatedPayload { latency_ms: 38 }),
                CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid: 7, speaking: true }),
            ] {
                decibell_model::events::route(&model, event, cx);
            }
            model.dm.list.update(cx, |l, cx| l.set_active(Some(7), cx));
            model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Dm, cx));
        }
        Scene::Stage { .. } => {}
    }
    model
}

/// The scene as the Electron pages replay it: the shell's events and ours, then what the page
/// sets in the stores itself (`voice`, `watching`, `focused`, `pip`, `view`, `thumbnails` as
/// data URLs, `goLive`, `call`).
pub fn json(scene: Scene, name: &str) -> serde_json::Value {
    use base64::Engine as _;
    let list: Vec<serde_json::Value> = shell::events()
        .iter()
        .chain(events(scene).iter())
        .map(|e| {
            let (name, payload) = e.to_json();
            serde_json::json!({ "event": name, "payload": payload })
        })
        .collect();
    let (n, _, watched) = shape(scene);
    let watching: Vec<i64> = [3i64, 2].into_iter().take(watched).collect();
    let jpeg = |bytes: &[u8]| format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    let (view, focused, pip) = match scene {
        Scene::Focus => ("voice", Some(3), Some(3)),
        Scene::Mini => ("server", None, Some(3)),
        Scene::CallStage => ("dm", None, None),
        Scene::CallIncoming | Scene::GoLive { .. } if n == 0 => ("server", None, None),
        _ => ("voice", None, None),
    };
    let go_live = match scene {
        Scene::GoLive { preset, advanced, software } => serde_json::json!({ "preset": preset, "advanced": advanced, "software": software }),
        _ => serde_json::Value::Null,
    };
    let call = match scene {
        Scene::CallIncoming => serde_json::json!({ "status": "incoming", "peer": 7, "peerName": "perf2" }),
        Scene::CallStage => serde_json::json!({ "status": "active", "peer": 7, "peerName": "perf2", "path": "host", "verified": true, "latency": 38, "speaking": [7] }),
        _ => serde_json::Value::Null,
    };
    serde_json::json!({
        "scene": name,
        "me": { "uid": ME, "username": ME_NAME },
        "events": list,
        "nav": {
            "view": if view == "dm" { "home" } else { "server" },
            "server": DAILY_GAMING.to_string(),
            "channel": "general",
            "voice": if n > 0 { serde_json::json!({ "server": DAILY_GAMING.to_string(), "channel": CHANNEL }) } else { serde_json::json!({ "server": DAILY_GAMING.to_string(), "channel": "squad" }) },
        },
        "voiceView": view,
        "watching": watching,
        "focused": focused,
        "pip": pip,
        "thumbnails": { "3": jpeg(POSTER_CAROL), "2": jpeg(POSTER_BOB) },
        "speaking": if n > 1 { vec![1] } else { Vec::<i64>::new() },
        "goLive": go_live,
        "call": call,
    })
}
