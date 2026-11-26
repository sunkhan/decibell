//! Self-tests of stream playback on the real platform: real window, real GPU, real frame loop
//! (`decibell --selftest video-hw,video-idle,…`). Each opens a window of its own, plays test
//! streams from `gpui-client/vendor/streams` (`tools/streamgen/make-streams.sh`;
//! `DECIBELL_VIDEO_STREAMS` points elsewhere) through the hub, and closes it. Without the
//! streams a test says so and passes: they are 600 MB and not in the repository.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx};
use gpui::{
    AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, px, size,
};

use crate::codec::DecodePath;
use crate::dbv::{FeedOptions, Feeder};
use crate::hub::{Fault, Stream, VideoHub};
use crate::screens::{Playback, Tile, file_label, stats_line};
use crate::stats::StreamStats;
use crate::view::Fit;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "video-hw",
            about: "each codec decodes on the GPU: every frame in is a picture presented, the right one",
            run: |cx| Box::pin(hardware(cx)),
        },
        SelfTest {
            name: "video-idle",
            about: "after a stream ends: no draws, no presents, no decoder thread, no textures",
            run: |cx| Box::pin(idle(cx)),
        },
        SelfTest {
            name: "video-two",
            about: "two streams at once, one also in a mini-player: all frames of both presented",
            run: |cx| Box::pin(two(cx)),
        },
        SelfTest {
            name: "video-sw",
            about: "the software path: each codec decoded on the CPU and uploaded",
            run: |cx| Box::pin(software(cx)),
        },
        SelfTest {
            name: "video-recover",
            about: "a decoder error falls back to software, a lost GPU device is survived",
            run: |cx| Box::pin(recover(cx)),
        },
        SelfTest {
            name: "video-switch",
            about: "a stream that changes resolution mid-stream keeps playing",
            run: |cx| Box::pin(switch(cx)),
        },
    ]
}

/// The test streams' folder, when it has them.
fn streams_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("DECIBELL_VIDEO_STREAMS").map(PathBuf::from),
        // target/<profile>/decibell -> gpui-client/vendor/streams
        std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.parent()?.parent()?.join("vendor/streams"))),
        std::env::current_dir()
            .ok()
            .map(|dir| dir.join("vendor/streams")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join("1080p60_h264.dbv").is_file())
}

/// `None` (after saying why) when the test streams are not on this machine.
fn streams(cx: &TestCx, names: &[&str]) -> Option<Vec<PathBuf>> {
    let Some(dir) = streams_dir() else {
        cx.log(
            "SKIPPED: no test streams in vendor/streams (run tools/streamgen/make-streams.sh, \
             or set DECIBELL_VIDEO_STREAMS)",
        );
        return None;
    };
    let paths: Vec<PathBuf> = names.iter().map(|name| dir.join(name)).collect();
    if let Some(missing) = paths.iter().find(|path| !path.is_file()) {
        cx.log(format_args!("SKIPPED: {} is missing", missing.display()));
        return None;
    }
    Some(paths)
}

/// A test window showing streams.
struct Stage {
    cx: TestCx,
    hub: VideoHub,
    window: WindowHandle<Playback>,
    streams: Vec<Stream>,
}

impl Stage {
    /// Opens the window with one tile per key (and stream 0 in a mini-player with `mirror`).
    /// Nothing is fed yet.
    fn open(cx: &TestCx, keys: &[String], labels: &[PathBuf], mirror: bool) -> Result<Stage> {
        let (hub, streams, window) = cx.app().update(|cx| -> Result<_> {
            let hub = VideoHub::global(cx);
            let streams: Vec<Stream> = keys.iter().map(|key| hub.stream(key)).collect();
            let playback = Playback {
                tiles: streams
                    .iter()
                    .zip(labels)
                    .map(|(stream, path)| Tile {
                        viewer: stream.viewer(),
                        label: file_label(path),
                        fit: Fit::Contain,
                    })
                    .collect(),
                mini: mirror.then(|| streams[0].viewer()),
                message: None,
                feeders: Vec::new(),
            };
            let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Decibell video self-test".into()),
                        ..Default::default()
                    }),
                    // A stream keeps its frame rate in an unfocused window.
                    inactive_frame_interval: None,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| playback),
            )?;
            Ok((hub, streams, window))
        })?;
        Ok(Stage {
            cx: cx.clone(),
            hub,
            window,
            streams,
        })
    }

    fn frames(&self) -> Result<Frames> {
        self.cx.app().update(|cx| {
            self.window
                .update(cx, |_, window, _| Frames {
                    drawn: window.frames_drawn(),
                    presented: window.frames_presented(),
                })
                .context("the video test window is closed")
        })
    }

    /// Waits until `done` says so; fails after `timeout`.
    async fn wait(
        &self,
        what: &str,
        timeout: Duration,
        mut done: impl FnMut() -> bool,
    ) -> Result<()> {
        let started = Instant::now();
        while !done() {
            if started.elapsed() > timeout {
                for stream in &self.streams {
                    self.cx.log(format_args!(
                        "{}: {}",
                        stream.key(),
                        stats_line(&self.hub, stream)
                    ));
                }
                bail!("timed out after {timeout:?} waiting for {what}");
            }
            self.cx.sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    /// Waits until every picture of `stream` that was published has either been presented or
    /// been overtaken, and the decoder has nothing left.
    async fn drained(&self, stream: &Stream, feeder: &Feeder) -> Result<StreamStats> {
        self.wait("the feeder to finish", Duration::from_secs(60), || {
            feeder.finished()
        })
        .await?;
        let mut last = stream.stats();
        let mut quiet = Instant::now();
        self.wait("the stream to drain", Duration::from_secs(10), || {
            let now = stream.stats();
            if now != last {
                last = now;
                quiet = Instant::now();
            }
            last.queue == 0 && quiet.elapsed() > Duration::from_millis(300)
        })
        .await?;
        Ok(last)
    }

    /// Ends every stream and closes the window.
    fn close(self) -> Result<()> {
        for stream in &self.streams {
            self.hub.remove(stream.key());
        }
        self.cx.app().update(|cx| {
            self.window
                .update(cx, |_, window, _| window.remove_window())
                .context("the video test window is closed")
        })
    }
}

fn feed(stage: &Stage, index: usize, path: &Path, frames: Option<u64>) -> Result<Feeder> {
    Feeder::start(
        stage.hub.clone(),
        stage.streams[index].key(),
        path,
        FeedOptions {
            frames,
            unpaced: false,
        },
    )
}

/// Every frame that went in came out as a picture, and every picture reached the screen or was
/// overtaken by the next one before a frame was drawn (at most `max_replaced` of them).
fn ensure_all_presented(
    cx: &TestCx,
    name: &str,
    stats: &StreamStats,
    frames: u64,
    path: DecodePath,
    max_replaced: u64,
) -> Result<()> {
    cx.log(format_args!(
        "{name}: {frames} frames in, {} decoded, {} presented, {} replaced, {} dropped; decode p50 {:.2} ms p99 {:.2} ms; presented every {:.2} ms (p99 {:.2})",
        stats.decoded,
        stats.presented,
        stats.replaced,
        stats.dropped,
        stats.decode_ms_p50,
        stats.decode_ms_p99,
        stats.present_interval_ms_p50,
        stats.present_interval_ms_p99,
    ));
    ensure!(stats.errors == 0, "{name}: {} decoder errors", stats.errors);
    ensure!(
        stats.received == frames,
        "{name}: {} of {frames} frames arrived",
        stats.received
    );
    ensure!(
        stats.dropped == 0,
        "{name}: {} frames dropped",
        stats.dropped
    );
    ensure!(
        stats.decoded == frames,
        "{name}: {} of {frames} frames decoded",
        stats.decoded
    );
    ensure!(
        stats.published == frames,
        "{name}: {} of {frames} pictures published",
        stats.published
    );
    ensure!(
        stats.replaced <= max_replaced,
        "{name}: {} pictures never reached a frame (at most {max_replaced} may be overtaken)",
        stats.replaced
    );
    ensure!(
        stats.path == Some(path) || stats.path.is_none(),
        "{name}: decoded on the {:?} path",
        stats.path
    );
    Ok(())
}

/// Reads the barcode of pictures as they are published and checks each against its frame.
async fn check_barcodes(stage: &Stage, stream: &Stream, name: &str, count: usize) -> Result<()> {
    for _ in 0..count {
        let capture = stream.capture_luma_row(1.0 / 36.0);
        let mut row = None;
        stage
            .wait("a captured picture", Duration::from_secs(5), || {
                row = capture.try_recv().ok();
                row.is_some()
            })
            .await?;
        let row = row.context("no capture")?;
        // Frame n (0-based) is the n+1th packet as long as nothing was shed, which the caller
        // asserts afterwards.
        let number = crate::decode::barcode(&row.bytes);
        ensure!(
            number as u64 == row.serial - 1,
            "{name}: the picture of frame {} carries barcode {number}",
            row.serial - 1
        );
        stage.cx.sleep(Duration::from_millis(150)).await;
    }
    stage.cx.log(format_args!(
        "{name}: {count} pictures read back from the texture carry their frame's barcode"
    ));
    Ok(())
}

async fn play_each_codec(cx: TestCx, path: DecodePath, frames: u64) -> Result<()> {
    let names = ["1080p60_h264.dbv", "1080p60_hevc.dbv", "1080p60_av1.dbv"];
    let Some(files) = streams(&cx, &names) else {
        return Ok(());
    };
    for (name, file) in names.iter().zip(&files) {
        let key = format!("selftest-{}-{name}", path.label());
        let stage = Stage::open(&cx, &[key], std::slice::from_ref(file), false)?;
        cx.settle().await?;
        let feeder = feed(&stage, 0, file, Some(frames))?;
        check_barcodes(&stage, &stage.streams[0], name, 3).await?;
        let playing = stage.streams[0].stats();
        ensure!(
            playing.path == Some(path),
            "{name}: decoding on the {:?} path, expected {path:?}",
            playing.path
        );
        let stats = stage.drained(&stage.streams[0], &feeder).await?;
        let result = ensure_all_presented(&cx, name, &stats, frames, path, frames / 50 + 1);
        stage.close()?;
        result?;
    }
    Ok(())
}

async fn hardware(cx: TestCx) -> Result<()> {
    let hub = cx.app().update(VideoHub::global);
    let support = hub.capabilities();
    cx.log(format_args!("decode support: {support:?}"));
    if support
        .iter()
        .any(|codec| codec.path != Some(DecodePath::Hardware))
    {
        cx.log("SKIPPED: this GPU does not decode all three codecs (see video-sw)");
        return Ok(());
    }
    play_each_codec(cx.clone(), DecodePath::Hardware, 180).await?;
    description_stream(cx).await
}

/// HEVC as the streamer sends it (length-prefixed NAL units, parameter sets in the keyframes'
/// hvcC description), on the hardware decoder.
async fn description_stream(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_hevc.dbv"]) else {
        return Ok(());
    };
    let recording =
        std::env::temp_dir().join(format!("decibell-selftest-hvcc-{}.dbv", std::process::id()));
    let frames = 120;
    crate::dbv::write_hvcc_recording(&files[0], &recording, frames as usize)?;
    let stage = Stage::open(
        &cx,
        &["selftest-hvcc".into()],
        std::slice::from_ref(&recording),
        false,
    )?;
    let played = async {
        // The window's first frame comes first: pictures published before it are overtaken.
        cx.settle().await?;
        let feeder = feed(&stage, 0, &recording, Some(frames))?;
        check_barcodes(&stage, &stage.streams[0], "hevc + hvcC", 2).await?;
        let stats = stage.drained(&stage.streams[0], &feeder).await?;
        ensure_all_presented(
            &cx,
            "hevc with an hvcC description",
            &stats,
            frames,
            DecodePath::Hardware,
            frames / 50 + 1,
        )
    }
    .await;
    std::fs::remove_file(&recording).ok();
    stage.close()?;
    played
}

async fn software(cx: TestCx) -> Result<()> {
    let hub = cx.app().update(VideoHub::global);
    hub.set_force_software(true);
    let result = play_each_codec(cx, DecodePath::Software, 120).await;
    hub.set_force_software(false);
    result
}

async fn idle(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_hevc.dbv"]) else {
        return Ok(());
    };
    let stage = Stage::open(&cx, &["selftest-idle".into()], &files, false)?;
    let stream = stage.streams[0].clone();
    let feeder = feed(&stage, 0, &files[0], Some(120))?;
    let stats = stage.drained(&stream, &feeder).await?;
    ensure!(
        stats.decoded == 120,
        "{} of 120 frames decoded",
        stats.decoded
    );
    ensure!(
        stage.hub.decoder_threads() == 1,
        "expected the stream's decoder thread"
    );
    ensure!(
        stage.hub.live_textures() > 0,
        "expected the stream's textures"
    );

    // A stream that stops sending without being removed: its decoder thread leaves by itself.
    stage
        .wait(
            "the idle decoder thread to exit",
            Duration::from_secs(5),
            || stage.hub.decoder_threads() == 0,
        )
        .await?;
    cx.log("no frames for 2 s: the decoder thread exited, the last picture stays up");
    ensure!(
        stream.picture_size() == Some((1920, 1080)),
        "the last picture is gone"
    );

    // The stream ends: everything of it goes.
    stage.hub.remove(stream.key());
    stage
        .wait(
            "the stream's textures to be freed",
            Duration::from_secs(5),
            || stage.hub.live_textures() == 0,
        )
        .await?;
    cx.log("stream removed: 0 decoder threads, 0 textures");

    // And then nothing may happen. Another window taking the focus is a frame, hence the retry.
    let seconds = 5;
    let mut frames = Frames {
        drawn: 0,
        presented: 0,
    };
    for _ in 0..3 {
        stage.cx.sleep(Duration::from_millis(500)).await;
        let before = stage.frames()?;
        let stats_before = stream.stats();
        stage.cx.sleep(Duration::from_secs(seconds)).await;
        frames = stage.frames()?.since(before);
        ensure!(
            stream.stats() == stats_before,
            "the ended stream's counters moved"
        );
        if frames.drawn == 0 && frames.presented == 0 {
            break;
        }
    }
    cx.log(format_args!(
        "{seconds} s after the stream ended: {} frames drawn, {} presented, {} decoder threads, {} textures",
        frames.drawn,
        frames.presented,
        stage.hub.decoder_threads(),
        stage.hub.live_textures()
    ));
    let result = (|| {
        ensure!(
            frames.drawn == 0,
            "{} frames drawn with no stream",
            frames.drawn
        );
        ensure!(
            frames.presented == 0,
            "{} frames presented with no stream",
            frames.presented
        );
        ensure!(
            stage.hub.decoder_threads() == 0,
            "a decoder thread is alive"
        );
        ensure!(stage.hub.live_textures() == 0, "textures are alive");
        Ok(())
    })();
    stage.close()?;
    result
}

async fn two(cx: TestCx) -> Result<()> {
    let names = ["1440p120_hevc.dbv", "1080p60_av1.dbv"];
    let Some(files) = streams(&cx, &names) else {
        return Ok(());
    };
    let keys = ["selftest-two-a".to_string(), "selftest-two-b".to_string()];
    let stage = Stage::open(&cx, &keys, &files, true)?;
    cx.settle().await?;
    let before = stage.frames()?;
    let (frames_a, frames_b) = (480, 240);
    let feeder_a = feed(&stage, 0, &files[0], Some(frames_a))?;
    let feeder_b = feed(&stage, 1, &files[1], Some(frames_b))?;
    stage
        .wait("both decoder threads", Duration::from_secs(5), || {
            stage.hub.decoder_threads() == 2
        })
        .await?;
    // Resizing reconfigures the swapchain (wgpu waits for the whole device to idle there) while
    // both decoders submit: what the device-wide queue lock exists for.
    let resizes: u64 = 12;
    for step in 0..resizes {
        stage.cx.sleep(Duration::from_millis(120)).await;
        let grown = step % 2 == 0;
        stage.cx.app().update(|cx| {
            stage.window.update(cx, |_, window, _| {
                window.resize(if grown {
                    size(px(1376.), px(774.))
                } else {
                    size(px(1280.), px(720.))
                })
            })
        })?;
    }
    let stats_a = stage.drained(&stage.streams[0], &feeder_a).await?;
    let stats_b = stage.drained(&stage.streams[1], &feeder_b).await?;
    let frames = stage.frames()?.since(before);
    cx.log(format_args!(
        "window: {} frames drawn (layout + paint; {resizes} resizes), {} presented, for {} pictures",
        frames.drawn,
        frames.presented,
        frames_a + frames_b
    ));
    let support = stage.hub.capabilities();
    let path = |index: usize| support[index].path.unwrap_or(DecodePath::Software);
    let result = (|| {
        // Two unsynchronised streams: a picture of one can arrive while the present for the
        // other is being prepared, and both then share a frame; none is lost.
        ensure_all_presented(&cx, names[0], &stats_a, frames_a, path(1), frames_a / 20)?;
        ensure_all_presented(&cx, names[1], &stats_b, frames_b, path(2), frames_b / 20)?;
        // Each resize is a layout and a paint (two when the compositor answers in two steps);
        // the pictures themselves must cost none.
        ensure!(
            frames.drawn <= 2 * resizes + 4,
            "the window was laid out and painted {} times for {resizes} resizes",
            frames.drawn
        );
        ensure!(
            frames.presented <= frames_a + frames_b + 2 * resizes + 8,
            "{} presents for {} pictures",
            frames.presented,
            frames_a + frames_b
        );
        Ok(())
    })();
    stage.close()?;
    result
}

async fn recover(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_h264.dbv"]) else {
        return Ok(());
    };
    let stage = Stage::open(&cx, &["selftest-recover".into()], &files, false)?;
    let stream = stage.streams[0].clone();
    if stage.hub.capabilities()[0].path != Some(DecodePath::Hardware) {
        cx.log("SKIPPED: no hardware decode on this GPU, nothing to fall back from");
        return stage.close();
    }
    // The file loops; it has a keyframe every 4 s, which is how long a recovery can take here
    // (a real streamer answers the keyframe request at once).
    let _feeder = feed(&stage, 0, &files[0], None)?;
    let playing = |path: DecodePath, after: u64| {
        let stats = stream.stats();
        stats.path == Some(path) && stats.presented > after + 30
    };
    stage
        .wait("hardware playback", Duration::from_secs(10), || {
            playing(DecodePath::Hardware, 0)
        })
        .await?;

    // 1. A decoder error: the stream moves to software and keeps playing.
    let before = stream.stats();
    stream.inject(Fault::DecodeError);
    stage
        .wait(
            "software playback after a decoder error",
            Duration::from_secs(12),
            || playing(DecodePath::Software, before.presented),
        )
        .await?;
    let after = stream.stats();
    cx.log(format_args!(
        "injected decoder error: fell back to software ({} fallback, {} keyframe requests), playing again after {} dropped frames",
        after.fallbacks - before.fallbacks,
        after.keyframe_requests - before.keyframe_requests,
        after.dropped - before.dropped
    ));
    ensure!(
        after.fallbacks == before.fallbacks + 1,
        "expected one fallback"
    );
    ensure!(
        after.keyframe_requests > before.keyframe_requests,
        "no keyframe was requested"
    );

    // 2. The GPU device is lost: the renderer makes a new one, the stream moves to it.
    stream.set_software_only(false);
    let before = stream.stats();
    let frames_before = stage.frames()?;
    stream.inject(Fault::DeviceLost);
    stage
        .wait(
            "the device loss to be noticed",
            Duration::from_secs(5),
            || stream.stats().device_losses > before.device_losses,
        )
        .await?;
    stage
        .wait(
            "hardware playback on the new device",
            Duration::from_secs(15),
            || {
                playing(
                    DecodePath::Hardware,
                    stream.stats().presented.min(before.presented),
                ) && stream.stats().presented > before.presented + 60
            },
        )
        .await?;
    let after = stream.stats();
    let frames = stage.frames()?.since(frames_before);
    cx.log(format_args!(
        "injected device loss: {} loss noticed, decoder reopened {} times, hardware playback resumed ({} more pictures presented, window presented {} frames)",
        after.device_losses - before.device_losses,
        after.decoder_opens - before.decoder_opens,
        after.presented - before.presented,
        frames.presented
    ));
    ensure!(
        after.device_losses == before.device_losses + 1,
        "expected one device loss"
    );
    ensure!(
        frames.presented > 60,
        "the window stopped presenting after the device loss"
    );
    // Every window moves to the new device at its next frame: make the app's main window do it
    // now, while this one is still open.
    let main_before = cx.frames()?;
    cx.update(|window, _| window.refresh())?;
    cx.settle().await?;
    let main_frames = cx.frames()?.since(main_before);
    cx.log(format_args!(
        "the main window drew {} frames on the new device",
        main_frames.drawn
    ));
    ensure!(
        main_frames.drawn >= 1,
        "the main window did not draw after the device loss"
    );
    stage.close()
}

async fn switch(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["switch_hevc.dbv"]) else {
        return Ok(());
    };
    let stage = Stage::open(&cx, &["selftest-switch".into()], &files, false)?;
    let stream = stage.streams[0].clone();
    cx.settle().await?;
    // 1080p, 1440p, 2160p: 4 s each at 60 fps.
    let frames = 720;
    let feeder = feed(&stage, 0, &files[0], Some(frames))?;
    let mut sizes: Vec<(u32, u32)> = Vec::new();
    stage
        .wait("the stream to finish", Duration::from_secs(30), || {
            if let Some(size) = stream.picture_size()
                && sizes.last() != Some(&size)
            {
                sizes.push(size);
            }
            feeder.finished()
        })
        .await?;
    let stats = stage.drained(&stream, &feeder).await?;
    cx.log(format_args!(
        "picture sizes seen: {sizes:?}; decoders opened: {}",
        stats.decoder_opens
    ));
    let path = stage.hub.capabilities()[1]
        .path
        .unwrap_or(DecodePath::Software);
    let result = (|| {
        ensure_all_presented(&cx, "switch_hevc.dbv", &stats, frames, path, frames / 50)?;
        ensure!(
            sizes == [(1920, 1080), (2560, 1440), (3840, 2160)],
            "expected the three sizes in order"
        );
        Ok(())
    })();
    stage.close()?;
    result
}
