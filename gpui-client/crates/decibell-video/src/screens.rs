//! `decibell --screen video-test --dbv a.dbv[,b.dbv] [--mirror]`: replays `.dbv` files at their
//! recorded timing through the real hub path (queue, decoder thread, hardware or software
//! decode, present per picture) into ordinary GPUI elements: a tile per stream with a label and
//! a translucent disc drawn over the video, rounded corners, and with `--mirror` stream 0 a
//! second time in a floating mini-player (one decoder, two places).
//!
//! It is the measurement target (`gpui-client/HANDOFF.md` §4, "Stream playback") and a manual
//! check, not a capture: it reads files named on the command line.
//!
//! - `DECIBELL_VIDEO_STATS=<seconds>`: a thread prints every stream's counters on stderr at
//!   that period (the UI is not woken to report them).
//! - `DECIBELL_VIDEO_TEST_FRAMES=<n>`: each stream ends after n frames (then nothing may draw).
//! - `DECIBELL_VIDEO_DECODER=vulkan|nvdec|vaapi|software`: decode with that decoder and no
//!   other (`select.rs`). `DECIBELL_VIDEO_FASTPATH=0`: every present writes the scene to the
//!   GPU again. `DECIBELL_VIDEO_TRACE=<file>`: per-frame timestamps when a stream ends.

use std::path::{Path, PathBuf};
use std::time::Duration;

use decibell_harness::Screen;
use decibell_ui::{ActiveTheme as _, Label};
use gpui::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div, px,
};

use crate::dbv::{FeedOptions, Feeder};
use crate::hub::{Stream, VideoHub, Viewer};
use crate::view::{Fit, VideoView};

pub fn screens() -> Vec<Screen> {
    vec![Screen {
        name: "video-test",
        about: "Plays .dbv test streams: --dbv a.dbv[,b.dbv] [--mirror]",
        build: |_, cx| build_from_command_line(cx),
    }]
}

/// A stream on the test stage.
pub(crate) struct Tile {
    pub viewer: Viewer,
    pub label: SharedString,
    pub fit: Fit,
}

/// The test stage: a row of stream tiles with GPUI elements over each, and optionally the
/// first stream again in a floating mini-player. Also what the self-tests show.
pub(crate) struct Playback {
    pub tiles: Vec<Tile>,
    pub mini: Option<Viewer>,
    pub message: Option<SharedString>,
    /// Feeds the tiles for as long as the view lives.
    pub feeders: Vec<Feeder>,
}

impl Render for Playback {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tiles = self.tiles.iter().map(|tile| {
            div()
                .relative()
                .flex_1()
                .h_full()
                .child(
                    VideoView::new(&tile.viewer)
                        .fit(tile.fit)
                        .rounded(theme.radius_xl)
                        .backdrop(theme.color_bg_darkest),
                )
                // Ordinary elements over the video: a label, and a translucent disc.
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded(theme.radius_md)
                                .bg(theme.color_bg_darkest.opacity(0.7))
                                .child(
                                    Label::new(tile.label.clone())
                                        .size(13.)
                                        .weight(600.)
                                        .color(theme.color_text_bright),
                                ),
                        )
                        .child(
                            div()
                                .size_16()
                                .rounded_full()
                                .bg(theme.color_accent.opacity(0.6))
                                .border_2()
                                .border_color(theme.color_text_bright),
                        ),
                )
        });
        let mini = self.mini.as_ref().map(|viewer| {
            div()
                .absolute()
                .right_8()
                .bottom_8()
                .w(px(384.))
                .h(px(216.))
                .rounded(theme.radius_lg)
                .border_2()
                .border_color(theme.color_accent)
                .child(
                    VideoView::new(viewer)
                        .rounded(theme.radius_lg)
                        .backdrop(theme.color_bg_darkest),
                )
        });
        div()
            .relative()
            .size_full()
            .bg(theme.color_bg_primary)
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .children(self.message.clone().map(|message| {
                Label::new(message)
                    .size(13.)
                    .color(theme.color_text_secondary)
            }))
            .child(div().flex().flex_1().min_h_0().gap_4().children(tiles))
            .children(mini)
    }
}

/// `--dbv a,b` and `--mirror` from the process's arguments (`DECIBELL_VIDEO_DBV` also works).
fn command_line() -> (Vec<PathBuf>, bool) {
    let args: Vec<String> = std::env::args().collect();
    let files = args
        .iter()
        .position(|arg| arg == "--dbv")
        .and_then(|index| args.get(index + 1).cloned())
        .or_else(|| std::env::var("DECIBELL_VIDEO_DBV").ok())
        .map(|list| list.split(',').map(PathBuf::from).collect())
        .unwrap_or_default();
    (files, args.iter().any(|arg| arg == "--mirror"))
}

fn build_from_command_line(cx: &mut App) -> AnyView {
    let (files, mirror) = command_line();
    let hub = VideoHub::global(cx);
    let frames = std::env::var("DECIBELL_VIDEO_TEST_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok());
    let mut playback = Playback {
        tiles: Vec::new(),
        mini: None,
        message: files.is_empty().then(|| {
            "No stream: decibell --screen video-test --dbv a.dbv[,b.dbv] [--mirror]".into()
        }),
        feeders: Vec::new(),
    };
    let mut streams = Vec::new();
    for (index, path) in files.iter().enumerate() {
        let key = format!("dbv{index}");
        let stream = hub.stream(&key);
        playback.tiles.push(Tile {
            viewer: stream.viewer(),
            label: file_label(path),
            fit: Fit::Contain,
        });
        match Feeder::start(
            hub.clone(),
            &key,
            path,
            FeedOptions {
                frames,
                unpaced: false,
            },
        ) {
            Ok(feeder) => playback.feeders.push(feeder),
            Err(error) => playback.message = Some(format!("{error:#}").into()),
        }
        streams.push(stream);
    }
    if mirror {
        playback.mini = streams.first().map(Stream::viewer);
    }
    if let Some(seconds) = std::env::var("DECIBELL_VIDEO_STATS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|seconds| *seconds > 0.0)
    {
        print_stats_every(Duration::from_secs_f64(seconds), hub, streams);
    }
    cx.new(|_| playback).into()
}

pub(crate) fn file_label(path: &Path) -> SharedString {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
        .into()
}

/// One line per stream per period on stderr, from a thread of its own.
fn print_stats_every(period: Duration, hub: VideoHub, streams: Vec<Stream>) {
    let spawned = std::thread::Builder::new()
        .name("video-stats".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            loop {
                std::thread::sleep(period);
                for stream in &streams {
                    eprintln!(
                        "video-stats t={:.1} {} {}",
                        started.elapsed().as_secs_f64(),
                        stream.key(),
                        stats_line(&hub, stream)
                    );
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("video: starting the stats thread: {error}");
    }
}

pub(crate) fn stats_line(hub: &VideoHub, stream: &Stream) -> String {
    let stats = stream.stats();
    format!(
        "codec={} path={} backend={} size={}x{} received={} decoded={} published={} presented={} replaced={} dropped={} errors={} opens={} fallbacks={} device_losses={} keyframe_requests={} queue={} decode_ms_p50={:.2} decode_ms_p99={:.2} latency_ms_p50={:.2} latency_ms_p99={:.2} present_interval_ms_p50={:.2} present_interval_ms_p99={:.2} threads={} textures={} present_wakes={} packet_copies={}{}",
        stats.codec.map_or("-", |codec| codec.label()),
        stats.path.map_or("-", |path| path.label()),
        stats.backend.map_or("-", |backend| backend.label()),
        stats.width,
        stats.height,
        stats.received,
        stats.decoded,
        stats.published,
        stats.presented,
        stats.replaced,
        stats.dropped,
        stats.errors,
        stats.decoder_opens,
        stats.fallbacks,
        stats.device_losses,
        stats.keyframe_requests,
        stats.queue,
        stats.decode_ms_p50,
        stats.decode_ms_p99,
        stats.latency_ms_p50,
        stats.latency_ms_p99,
        stats.present_interval_ms_p50,
        stats.present_interval_ms_p99,
        hub.decoder_threads(),
        hub.live_textures(),
        hub.present_wakes(),
        crate::packet::copies(),
        renderer_frames(),
    )
}

/// How the renderer drew the process's frames: built from the scene, or encoded again from the
/// frame it kept (a present for a new picture), with the mean time of each on the UI thread.
fn renderer_frames() -> String {
    #[cfg(vk)]
    {
        let frames = gpui_wgpu::frame_stats();
        let mean_us = |ns: u64, count: u64| ns as f64 / 1000.0 / count.max(1) as f64;
        format!(
            " frames_built={} frames_reused={} built_us={:.0} reused_us={:.0}",
            frames.built,
            frames.reused,
            mean_us(frames.built_ns, frames.built),
            mean_us(frames.reused_ns, frames.reused),
        )
    }
    #[cfg(not(vk))]
    String::new()
}
