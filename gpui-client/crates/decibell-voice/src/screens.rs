//! The voice feature's screens, drawn from [`crate::fixture`] (no network, no clock): the whole
//! window, as the shell's `main-*` screens. `decibell-capture --screen <name>`; live with
//! `decibell --screen <name>` (there, a watched stream plays a `.dbv` test stream through the real
//! hub when `vendor/streams` exists; a capture shows the player's first state).

use decibell_harness::Screen;
use decibell_shell::Shell;
use decibell_ui::AssumeClientDecorations;
use gpui::{AnyView, App, AppContext as _};

use crate::fixture::{self, Scene};

fn capturing(cx: &App) -> bool {
    cx.has_global::<AssumeClientDecorations>()
}

/// Captures render one frame on a clock that doesn't run: entrances show their end.
fn settle_motion(cx: &mut App) {
    if capturing(cx) {
        cx.set_reduce_motion(true);
    }
}

/// Live (not a capture): the watched fixture streams play `.dbv` test files through the hub.
fn feed_watched(cx: &mut App) {
    if capturing(cx) {
        return;
    }
    let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/streams"));
    let model = decibell_model::Model::global(cx).clone();
    let watched: Vec<i64> = model.live.voice.watching.read(cx).list().to_vec();
    let hub = decibell_video::VideoHub::global(cx);
    for (uid, file) in watched.into_iter().zip(["1080p60_h264.dbv", "1440p120_av1.dbv"]) {
        let path = dir.join(file);
        if !path.exists() {
            log::info!("[voice] {} missing: the screen shows the poster", path.display());
            continue;
        }
        match decibell_video::dbv::Feeder::start(hub.clone(), &uid.to_string(), &path, Default::default()) {
            // The screen lives as long as the process.
            Ok(feeder) => std::mem::forget(feeder),
            Err(e) => log::warn!("[voice] feeding {}: {e:#}", path.display()),
        }
    }
}

fn shell(scene: Scene, cx: &mut App) -> AnyView {
    settle_motion(cx);
    if cx.try_global::<decibell_model::Model>().is_none() {
        decibell_model::Model::init(cx, None);
    }
    crate::init(cx);
    fixture::apply(scene, cx);
    feed_watched(cx);
    cx.new(Shell::new).into()
}

fn call_stage(cx: &mut App) -> AnyView {
    crate::calls::host_stage_in_dm_stand_in(cx);
    shell(Scene::CallStage, cx)
}

macro_rules! screen {
    ($name:literal, $about:literal, $scene:expr) => {
        Screen { name: $name, about: $about, build: |_, cx| shell($scene, cx) }
    };
}

pub fn screens() -> Vec<Screen> {
    vec![
        screen!("voice-stage-1", "Voice: alone in Lounge (your tile, the invite / share prompt)", Scene::Stage { people: 1, streams: 0, watched: 0 }),
        screen!("voice-stage-4", "Voice: four people in Lounge, alice speaking", Scene::Stage { people: 4, streams: 0, watched: 0 }),
        screen!("voice-stage-9", "Voice: nine people, the best-fit grid", Scene::Stage { people: 9, streams: 0, watched: 0 }),
        screen!("voice-stage-25", "Voice: 25 people: the size floor and \"+N more\"", Scene::Stage { people: 25, streams: 0, watched: 0 }),
        screen!("voice-stage-live", "Voice: two live streams nobody watches (posters) over the people row", Scene::Stage { people: 4, streams: 2, watched: 0 }),
        screen!("voice-stage-stream", "Voice: one stream watched in its tile, four people", Scene::Stage { people: 4, streams: 1, watched: 1 }),
        screen!("voice-stage-streams", "Voice: two streams watched (Watch too), nine people", Scene::Stage { people: 9, streams: 2, watched: 2 }),
        screen!("voice-focus", "Voice: carol's stream focused, the filmstrip below", Scene::Focus),
        screen!("voice-mini", "The mini-player over #general (carol's stream loaded)", Scene::Mini),
        screen!("go-live", "Go Live: Smooth, hardware encoders (NVENC)", Scene::GoLive { preset: "smooth", advanced: false, software: false }),
        screen!("go-live-sharp", "Go Live: Sharp", Scene::GoLive { preset: "sharp", advanced: false, software: false }),
        screen!("go-live-slides", "Go Live: Slides", Scene::GoLive { preset: "slides", advanced: false, software: false }),
        screen!("go-live-saver", "Go Live: Data saver", Scene::GoLive { preset: "saver", advanced: false, software: false }),
        screen!("go-live-advanced", "Go Live: Advanced open (resolution, frame rate, bitrate, codec)", Scene::GoLive { preset: "smooth", advanced: true, software: false }),
        screen!("go-live-software", "Go Live: only a software encoder (SVT-AV1): Smooth and Sharp greyed", Scene::GoLive { preset: "slides", advanced: true, software: true }),
        screen!("call-incoming", "perf2 calling: the incoming card", Scene::CallIncoming),
        Screen { name: "call-stage", about: "In a call with perf2: the call stage over the DM (connected, LAN, verified)", build: |_, cx| call_stage(cx) },
    ]
}
