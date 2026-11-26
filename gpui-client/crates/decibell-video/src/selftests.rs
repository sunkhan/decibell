//! Self-tests of stream playback on the real platform: real window, real GPU, real frame loop
//! (`decibell --selftest video-hw,video-idle,…`; `gpui-client/docs/video-owner-checks.md` says
//! what to run on a machine with another GPU). Each opens a window of its own, plays test
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

use crate::codec::{Codec, CodecSupport, DecodeBackend, DecodePath};
use crate::dbv::{FeedOptions, Feeder};
use crate::hub::{Fault, HubCallbacks, STALLED_AFTER, Stream, StreamEvent, VideoHub};
use crate::screens::{Playback, Tile, file_label, stats_line};
use crate::stats::StreamStats;
use crate::view::Fit;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "video-hw",
            about: "the capability probe, then each codec on the decoder it found: every frame presented, the right picture and colours",
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
            about: "a decoder error moves the stream to the next decoder; a lost GPU device is survived on each",
            run: |cx| Box::pin(recover(cx)),
        },
        SelfTest {
            name: "video-switch",
            about: "a stream that changes resolution mid-stream keeps playing",
            run: |cx| Box::pin(switch(cx)),
        },
        SelfTest {
            name: "video-nvdec",
            about: "NVDEC (NVIDIA): each codec decoded through CUDA into the renderer's textures",
            run: |cx| Box::pin(vendor_decoder(cx, DecodeBackend::Nvdec)),
        },
        SelfTest {
            name: "video-vaapi",
            about: "VA-API (AMD, Intel): each codec decoded into DMA-BUFs imported by the renderer's device",
            run: |cx| Box::pin(vendor_decoder(cx, DecodeBackend::Vaapi)),
        },
        SelfTest {
            name: "video-order",
            about: "each decoder failing in turn walks the order down to software, reasons recorded",
            run: |cx| Box::pin(order(cx)),
        },
        SelfTest {
            name: "video-starve",
            about: "a decoder thread held back for 60 ms at 120 fps loses nothing and is back at one frame of latency; held for 400 ms it sheds and asks for a keyframe",
            run: |cx| Box::pin(starve(cx)),
        },
        SelfTest {
            name: "video-stall",
            about: "a stream that stops sending says so after 3 s without a timer or a frame, and resumes with its next picture",
            run: |cx| Box::pin(stall(cx)),
        },
        SelfTest {
            name: "video-fastpath",
            about: "a present for a new picture reuses the frame the renderer kept: same pixels, no scene writes",
            run: |cx| Box::pin(fast_path(cx)),
        },
    ]
}

/// Forces a decoder order for a test and puts back what was forced before (the environment's
/// `DECIBELL_VIDEO_DECODER`, or nothing) when dropped.
struct ForcedOrder {
    hub: VideoHub,
    previous: Option<Vec<DecodeBackend>>,
}

impl ForcedOrder {
    fn new(hub: &VideoHub, order: &[DecodeBackend]) -> Self {
        let previous = hub.forced_decoder_order();
        hub.set_decoder_order(Some(order));
        Self {
            hub: hub.clone(),
            previous,
        }
    }
}

impl Drop for ForcedOrder {
    fn drop(&mut self) {
        self.hub.set_decoder_order(self.previous.as_deref());
    }
}

/// Runs the capability probe and waits for it without blocking the UI thread.
async fn probe(cx: &TestCx, hub: &VideoHub) -> Result<[CodecSupport; 3]> {
    let (sender, receiver) = std::sync::mpsc::channel();
    hub.probe_capabilities(move |result| {
        let _ = sender.send(result);
    });
    let started = Instant::now();
    loop {
        if let Ok(result) = receiver.try_recv() {
            return Ok(result);
        }
        ensure!(
            started.elapsed() < Duration::from_secs(60),
            "the capability probe did not finish"
        );
        cx.sleep(Duration::from_millis(20)).await;
    }
}

fn log_skipped(cx: &TestCx, hub: &VideoHub) {
    for skipped in hub.skipped_decoders() {
        cx.log(format_args!(
            "{} with {}: not used, {}",
            skipped.codec.label(),
            skipped.backend.label(),
            skipped.reason
        ));
    }
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

    /// The window's next frame as the renderer draws it, read back from the GPU. `rebuild`:
    /// the scene is written to the GPU again for it; else the frame the renderer kept is
    /// reused if it can be.
    #[cfg(vk)]
    async fn capture(&self, rebuild: bool) -> Result<gpui_wgpu::CapturedFrame> {
        let receiver = gpui_wgpu::capture_next_surface_frame(rebuild);
        self.cx.app().update(|cx| {
            self.window
                .update(cx, |_, window, _| window.request_present())
                .context("the video test window is closed")
        })?;
        let mut frame = None;
        self.wait("a captured frame", Duration::from_secs(5), || {
            frame = receiver.try_recv().ok();
            frame.is_some()
        })
        .await?;
        frame.context("no captured frame")
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

const CODEC_FILES: [(Codec, &str); 3] = [
    (Codec::H264, "1080p60_h264.dbv"),
    (Codec::Hevc, "1080p60_hevc.dbv"),
    (Codec::Av1, "1080p60_av1.dbv"),
];

/// What a window showed when a stream's last frame was up.
#[cfg(vk)]
type Shot = gpui_wgpu::CapturedFrame;
#[cfg(not(vk))]
type Shot = ();

/// Plays `frames` frames of each of `codecs` with whatever the hub's order gives, expecting
/// the decoder `expect`: every frame in is a picture presented, the right one. Returns the
/// window's last frame per codec.
async fn play_codecs(
    cx: &TestCx,
    codecs: &[Codec],
    expect: DecodeBackend,
    frames: u64,
) -> Result<Vec<(Codec, Shot)>> {
    let names: Vec<&str> = CODEC_FILES
        .iter()
        .filter(|(codec, _)| codecs.contains(codec))
        .map(|(_, name)| *name)
        .collect();
    let Some(files) = streams(cx, &names) else {
        return Ok(Vec::new());
    };
    let mut shots = Vec::new();
    for ((name, file), codec) in names.iter().zip(&files).zip(codecs) {
        let key = format!("selftest-{}-{name}", expect.label());
        let stage = Stage::open(cx, &[key], std::slice::from_ref(file), false)?;
        let played = async {
            cx.settle().await?;
            let feeder = feed(&stage, 0, file, Some(frames))?;
            check_barcodes(&stage, &stage.streams[0], name, 3).await?;
            let playing = stage.streams[0].stats();
            ensure!(
                playing.backend == Some(expect),
                "{name}: decoding with {:?}, expected {expect:?} ({})",
                playing.backend,
                playing.fallback_reason.as_deref().unwrap_or("no failure recorded")
            );
            let stats = stage.drained(&stage.streams[0], &feeder).await?;
            // A vendor decoder takes a few hundred milliseconds to open (a CUDA context, a VA
            // display): the frames that arrive meanwhile are decoded in a burst when it is up,
            // and of a burst only the newest picture reaches the screen.
            let burst = match expect {
                DecodeBackend::Nvdec | DecodeBackend::Vaapi => 40,
                DecodeBackend::VulkanVideo | DecodeBackend::Software => 0,
            };
            ensure_all_presented(
                cx,
                name,
                &stats,
                frames,
                expect.path(),
                frames / 50 + 1 + burst,
            )?;
            // The last frame stays up: what the window shows now is frame `frames - 1`.
            #[cfg(vk)]
            return stage.capture(true).await;
            #[cfg(not(vk))]
            Ok(())
        }
        .await;
        stage.close()?;
        shots.push((*codec, played?));
    }
    Ok(shots)
}

/// The largest difference between two captures of the same frame that two correct decoders may
/// show, as the mean absolute difference per channel (0..255). Hardware and software decoders
/// of these codecs are bit-exact, so the pictures are identical and so are the frames; the
/// margin is for a driver that rounds a copy differently. Swapped chroma planes, a wrong
/// matrix or a shifted plane are tens of levels.
const SAME_PICTURE: f64 = 1.0;

/// Checks that `shots` (a hardware decoder's) show what the software decoder shows for the same
/// frames: the colours are right, not only the luma barcode.
#[cfg(vk)]
async fn same_as_software(
    cx: &TestCx,
    hub: &VideoHub,
    backend: DecodeBackend,
    shots: &[(Codec, Shot)],
    frames: u64,
) -> Result<()> {
    let codecs: Vec<Codec> = shots.iter().map(|(codec, _)| *codec).collect();
    let reference = {
        let _software = ForcedOrder::new(hub, &[DecodeBackend::Software]);
        play_codecs(cx, &codecs, DecodeBackend::Software, frames).await?
    };
    for ((codec, shot), (_, software)) in shots.iter().zip(&reference) {
        ensure!(
            (shot.width, shot.height) == (software.width, software.height)
                && shot.pixels.len() == software.pixels.len(),
            "{}: the two windows differ in size",
            codec.label()
        );
        let difference: u64 = shot
            .pixels
            .iter()
            .zip(&software.pixels)
            .map(|(a, b)| a.abs_diff(*b) as u64)
            .sum();
        let mean = difference as f64 / shot.pixels.len().max(1) as f64;
        // A window that shows no video would compare equal too: the picture must be there.
        let varied = software
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .step_by(997)
            .map(|pixel| pixel[1])
            .collect::<std::collections::HashSet<u8>>()
            .len();
        cx.log(format_args!(
            "{} with {}: the window's last frame differs from software decode's by {mean:.3} of 255 per channel",
            codec.label(),
            backend.label()
        ));
        ensure!(varied > 16, "{}: the captured window shows no picture", codec.label());
        ensure!(
            mean <= SAME_PICTURE,
            "{} with {}: the picture differs from software decode's ({mean:.2} of 255 per channel)",
            codec.label(),
            backend.label()
        );
    }
    Ok(())
}

async fn hardware(cx: TestCx) -> Result<()> {
    let hub = cx.app().update(VideoHub::global);
    cx.log(format_args!(
        "decoder order: {}",
        hub.decoder_order()
            .iter()
            .map(|backend| backend.label())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let started = Instant::now();
    let support = probe(&cx, &hub).await?;
    cx.log(format_args!(
        "probed in {:.0} ms: {}",
        started.elapsed().as_secs_f64() * 1000.0,
        crate::probe::describe(&support)
    ));
    log_skipped(&cx, &hub);
    ensure!(
        support.iter().all(|codec| codec.path.is_some()),
        "a codec has no decoder at all: {support:?}"
    );
    let frames = 180;
    let mut played = false;
    for backend in DecodeBackend::ALL {
        let codecs: Vec<Codec> = support
            .iter()
            .filter(|codec| codec.backend == Some(backend))
            .map(|codec| codec.codec)
            .collect();
        if codecs.is_empty() || backend == DecodeBackend::Software {
            continue;
        }
        played = true;
        let shots = play_codecs(&cx, &codecs, backend, frames).await?;
        #[cfg(vk)]
        same_as_software(&cx, &hub, backend, &shots, frames).await?;
        #[cfg(not(vk))]
        let _ = shots;
    }
    if !played {
        cx.log("SKIPPED: no hardware decoder works on this machine (see video-sw)");
        return Ok(());
    }
    if support[Codec::Hevc.index()].path == Some(DecodePath::Hardware) {
        description_stream(cx).await?;
    }
    Ok(())
}

/// One vendor decoder alone (NVDEC or VA-API), forced: what `video-hw` does for the machine's
/// first choice. Skipped where the decoder does not exist for this GPU, unless
/// `DECIBELL_VIDEO_DECODER` names it: then its absence is the failure.
async fn vendor_decoder(cx: TestCx, backend: DecodeBackend) -> Result<()> {
    let hub = cx.app().update(VideoHub::global);
    let demanded = std::env::var("DECIBELL_VIDEO_DECODER")
        .is_ok_and(|names| names.split(',').any(|name| name.trim() == backend.label()));
    hub.forget_decoder_failures();
    let forced = ForcedOrder::new(&hub, &[backend]);
    let support = probe(&cx, &hub).await?;
    let codecs: Vec<Codec> = support
        .iter()
        .filter(|codec| codec.backend == Some(backend))
        .map(|codec| codec.codec)
        .collect();
    cx.log(format_args!(
        "{} decodes: {}",
        backend.label(),
        if codecs.is_empty() {
            "nothing".to_owned()
        } else {
            codecs
                .iter()
                .map(|codec| codec.label())
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    log_skipped(&cx, &hub);
    let result = async {
        if codecs.is_empty() {
            ensure!(
                !demanded,
                "DECIBELL_VIDEO_DECODER asks for {} and it decodes nothing here",
                backend.label()
            );
            cx.log(format_args!(
                "SKIPPED: {} is not usable on this GPU",
                backend.label()
            ));
            return Ok(());
        }
        let frames = 180;
        let shots = play_codecs(&cx, &codecs, backend, frames).await?;
        drop(forced);
        #[cfg(vk)]
        same_as_software(&cx, &hub, backend, &shots, frames).await?;
        #[cfg(not(vk))]
        let _ = shots;
        Ok(())
    }
    .await;
    hub.forget_decoder_failures();
    result
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
    let result = play_codecs(&cx, &Codec::ALL, DecodeBackend::Software, 120).await;
    hub.set_force_software(false);
    result.map(|_| ())
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

/// The hardware decoders that decode `codec` on this machine, each found by forcing it and
/// probing, in the order's order.
async fn usable_hardware(cx: &TestCx, hub: &VideoHub, codec: Codec) -> Result<Vec<DecodeBackend>> {
    let mut usable = Vec::new();
    for backend in DecodeBackend::ALL {
        if backend == DecodeBackend::Software {
            continue;
        }
        let _forced = ForcedOrder::new(hub, &[backend]);
        if probe(cx, hub).await?[codec.index()].backend == Some(backend) {
            usable.push(backend);
        }
    }
    Ok(usable)
}

fn labels(backends: &[DecodeBackend]) -> String {
    backends
        .iter()
        .map(|backend| backend.label())
        .collect::<Vec<_>>()
        .join(", ")
}

async fn recover(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_h264.dbv"]) else {
        return Ok(());
    };
    let hub = cx.app().update(VideoHub::global);
    hub.forget_decoder_failures();
    let hardware = usable_hardware(&cx, &hub, Codec::H264).await?;
    if hardware.is_empty() {
        cx.log("SKIPPED: no hardware decoder on this GPU, nothing to recover");
        return Ok(());
    }
    cx.log(format_args!(
        "hardware decoders for H.264 here: {}",
        labels(&hardware)
    ));

    // 1. A decoder error: the stream moves to the next decoder of the order and keeps playing.
    // The file loops; it has a keyframe every 4 s, which is how long a recovery can take here
    // (a real streamer answers the keyframe request at once).
    {
        let stage = Stage::open(&cx, &["selftest-recover".into()], &files, false)?;
        let stream = stage.streams[0].clone();
        let _feeder = feed(&stage, 0, &files[0], None)?;
        let moved = async {
            stage
                .wait("hardware playback", Duration::from_secs(10), || {
                    let stats = stream.stats();
                    stats.path == Some(DecodePath::Hardware) && stats.presented > 30
                })
                .await?;
            let before = stream.stats();
            stream.inject(Fault::DecodeError);
            stage
                .wait(
                    "playback on the next decoder after a decoder error",
                    Duration::from_secs(12),
                    || {
                        let stats = stream.stats();
                        stats.backend.is_some()
                            && stats.backend != before.backend
                            && stats.presented > before.presented + 30
                    },
                )
                .await?;
            let after = stream.stats();
            cx.log(format_args!(
                "injected decoder error on {}: playing with {} after {} dropped frames ({} keyframe requests); recorded: {}",
                before.backend.map_or("-", DecodeBackend::label),
                after.backend.map_or("-", DecodeBackend::label),
                after.dropped - before.dropped,
                after.keyframe_requests - before.keyframe_requests,
                after.fallback_reason.as_deref().unwrap_or("nothing"),
            ));
            ensure!(
                after.fallbacks == before.fallbacks + 1,
                "expected one fallback"
            );
            ensure!(
                after.keyframe_requests > before.keyframe_requests,
                "no keyframe was requested"
            );
            ensure!(
                after
                    .fallback_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("injected")),
                "the failure's reason was not recorded"
            );
            Ok(())
        }
        .await;
        stage.close()?;
        moved?;
    }

    // 2. The GPU device is lost: the renderer makes a new one and the stream moves to it, on
    // each hardware decoder in turn.
    for backend in hardware {
        let _forced = ForcedOrder::new(&hub, &[backend]);
        let key = format!("selftest-lost-{}", backend.label());
        let stage = Stage::open(&cx, &[key], &files, false)?;
        let stream = stage.streams[0].clone();
        let _feeder = feed(&stage, 0, &files[0], None)?;
        let survived = async {
            let playing = |after: u64| {
                let stats = stream.stats();
                stats.backend == Some(backend) && stats.presented > after + 30
            };
            stage
                .wait("playback", Duration::from_secs(10), || playing(0))
                .await?;
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
                    "playback on the new device",
                    Duration::from_secs(15),
                    || playing(before.presented + 30),
                )
                .await?;
            let after = stream.stats();
            let frames = stage.frames()?.since(frames_before);
            cx.log(format_args!(
                "{}: injected device loss: {} loss noticed, decoder reopened {} times, playback resumed ({} more pictures presented, window presented {} frames)",
                backend.label(),
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
                after.fallbacks == before.fallbacks,
                "a device loss must not cost the stream its decoder"
            );
            ensure!(
                frames.presented > 60,
                "the window stopped presenting after the device loss"
            );
            // Every window moves to the new device at its next frame: make the app's main
            // window do it now, while this one is still open.
            let main_before = cx.frames()?;
            cx.update(|window, _| window.refresh())?;
            cx.settle().await?;
            let main_frames = cx.frames()?.since(main_before);
            ensure!(
                main_frames.drawn >= 1,
                "the main window did not draw after the device loss"
            );
            Ok(())
        }
        .await;
        stage.close()?;
        survived?;
    }
    ensure!(
        hub.skipped_decoders().is_empty(),
        "a recovery gave a decoder up for the session: {:?}",
        hub.skipped_decoders()
    );
    Ok(())
}

async fn order(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_h264.dbv"]) else {
        return Ok(());
    };
    let hub = cx.app().update(VideoHub::global);
    hub.forget_decoder_failures();
    cx.log(format_args!(
        "this machine's order: {}",
        labels(&hub.decoder_order())
    ));
    // Every decoder that works here, in the order's order: on NVIDIA with the NVDEC-backed VA
    // driver installed that is all four, although the machine's own order has no VA-API.
    let mut walk = usable_hardware(&cx, &hub, Codec::H264).await?;
    walk.push(DecodeBackend::Software);
    hub.forget_decoder_failures();
    cx.log(format_args!("walking: {}", labels(&walk)));
    let _forced = ForcedOrder::new(&hub, &walk);
    let stage = Stage::open(&cx, &["selftest-order".into()], &files, false)?;
    let stream = stage.streams[0].clone();
    let _feeder = feed(&stage, 0, &files[0], None)?;
    let walked = async {
        let mut presented = 0;
        for (step, backend) in walk.iter().enumerate() {
            stage
                .wait(
                    &format!("playback with {}", backend.label()),
                    Duration::from_secs(15),
                    || {
                        let stats = stream.stats();
                        stats.backend == Some(*backend) && stats.presented > presented + 40
                    },
                )
                .await?;
            let stats = stream.stats();
            presented = stats.presented;
            ensure!(
                stats.fallbacks == step as u64,
                "{} fallbacks before failing {}",
                stats.fallbacks,
                backend.label()
            );
            if *backend == DecodeBackend::Software {
                break;
            }
            stream.inject(Fault::DecodeError);
            stage
                .wait("the failure to be noticed", Duration::from_secs(5), || {
                    stream.stats().fallbacks == step as u64 + 1
                })
                .await?;
            let reason = stream.stats().fallback_reason;
            cx.log(format_args!(
                "{} failed -> recorded {:?}",
                backend.label(),
                reason.as_deref().unwrap_or("nothing")
            ));
            ensure!(
                reason.as_deref().is_some_and(|reason| reason
                    .starts_with(&format!("{}: ", backend.label()))
                    && reason.contains("injected")),
                "the reason for giving {} up was not recorded",
                backend.label()
            );
        }
        let stats = stream.stats();
        cx.log(format_args!(
            "ended on {} after {} fallbacks, {} decoder opens",
            stats.backend.map_or("-", DecodeBackend::label),
            stats.fallbacks,
            stats.decoder_opens
        ));
        ensure!(
            stats.backend == Some(DecodeBackend::Software),
            "the walk did not end on software"
        );
        ensure!(
            stats.decoder_opens == walk.len() as u64,
            "{} decoders opened for {} steps: one was tried twice",
            stats.decoder_opens,
            walk.len()
        );
        // These decoders failed after half a second of pictures: that costs the stream the
        // decoder, not everybody (a failure in the first pictures would; `select.rs`).
        ensure!(
            hub.skipped_decoders().is_empty(),
            "a late failure gave a decoder up for the session"
        );
        Ok(())
    }
    .await;
    stage.close()?;
    walked
}

/// The UI thread's allocations per presented video frame that `video-fastpath` allows when it
/// can count them (run under `tools/alloc-count.c`). Measured: 139 with the kept frame, 280
/// when every present writes the scene again; what is left is wgpu's own per-frame work
/// (acquire, encoder, pass, submit, present) and the driver's.
const ALLOCATIONS_PER_PRESENT: u64 = 170;

/// The calling thread's allocation count, when the process runs under `alloc-count.so`.
fn allocation_count() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn dlsym(handle: *mut std::ffi::c_void, name: *const std::ffi::c_char)
            -> *mut std::ffi::c_void;
        }
        // Safety: a lookup in the process's own symbols; the function, when present, takes no
        // arguments and returns the thread's counter.
        unsafe {
            let symbol = dlsym(std::ptr::null_mut(), c"decibell_alloc_count".as_ptr());
            if symbol.is_null() {
                return None;
            }
            let count: unsafe extern "C" fn() -> std::ffi::c_ulong = std::mem::transmute(symbol);
            Some(count() as u64)
        }
    }
    #[cfg(not(target_os = "linux"))]
    None
}

#[cfg(not(vk))]
async fn fast_path(cx: TestCx) -> Result<()> {
    cx.log("SKIPPED: the renderer of this platform draws no video surfaces yet");
    Ok(())
}

#[cfg(vk)]
async fn fast_path(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_hevc.dbv"]) else {
        return Ok(());
    };
    if std::env::var("DECIBELL_VIDEO_FASTPATH").is_ok_and(|value| value == "0") {
        cx.log("SKIPPED: DECIBELL_VIDEO_FASTPATH=0 turns the kept frame off");
        return Ok(());
    }
    // One stream in two places (tile and mini-player), with ordinary elements over it.
    let stage = Stage::open(&cx, &["selftest-fastpath".into()], &files, true)?;
    let stream = stage.streams[0].clone();
    let checked = async {
        cx.settle().await?;
        let feeder = feed(&stage, 0, &files[0], Some(90))?;
        stage.drained(&stream, &feeder).await?;

        // The same picture drawn both ways: the scene written to the GPU for the frame, and
        // the frame the renderer kept encoded again.
        let compare = |what: &str, built: &Shot, reused: &Shot| -> Result<()> {
            ensure!(!built.reused, "{what}: the first capture was not built from the scene");
            ensure!(
                reused.reused,
                "{what}: the second capture did not reuse the kept frame"
            );
            ensure!(
                (built.width, built.height) == (reused.width, reused.height),
                "{what}: the two captures differ in size"
            );
            let different = built
                .pixels
                .iter()
                .zip(&reused.pixels)
                .filter(|(a, b)| a != b)
                .count();
            let varied = built
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .step_by(997)
                .map(|pixel| pixel[1])
                .collect::<std::collections::HashSet<u8>>()
                .len();
            cx.log(format_args!(
                "{what}: {}x{} built from the scene and from the kept frame: {different} of {} bytes differ",
                built.width,
                built.height,
                built.pixels.len()
            ));
            ensure!(varied > 16, "{what}: the captured window shows no picture");
            ensure!(different == 0, "{what}: the two paths drew different frames");
            Ok(())
        };
        let built = stage.capture(true).await?;
        let reused = stage.capture(false).await?;
        compare("window", &built, &reused)?;

        // Something else changes mid-stream (a resize: new layout, new scene, new size): the
        // next frame is built, and the one after reuses it again.
        let frames_before = gpui_wgpu::frame_stats();
        stage.cx.app().update(|cx| {
            stage
                .window
                .update(cx, |_, window, _| window.resize(size(px(1120.), px(700.))))
        })?;
        stage.cx.sleep(Duration::from_millis(400)).await;
        ensure!(
            gpui_wgpu::frame_stats().built > frames_before.built,
            "a resized window was not drawn from its scene"
        );
        let built = stage.capture(true).await?;
        let reused = stage.capture(false).await?;
        compare("after a resize", &built, &reused)?;
        ensure!(
            (built.width, built.height) != (0, 0),
            "empty capture after the resize"
        );

        // Then a stream plays: every picture is a present of the kept frame.
        let frames: u64 = 240;
        let before = gpui_wgpu::frame_stats();
        let presented_before = stream.stats().presented;
        let allocations_before = allocation_count();
        let feeder = feed(&stage, 0, &files[0], Some(frames))?;
        // One timer for the whole run: polling would be the test's own work on this thread.
        stage
            .cx
            .sleep(Duration::from_millis(frames * 1000 / 60 + 400))
            .await;
        let allocations = allocation_count()
            .zip(allocations_before)
            .map(|(after, before)| after - before);
        let after = gpui_wgpu::frame_stats();
        ensure!(feeder.finished(), "the feeder did not finish in time");
        let presented = stream.stats().presented - presented_before;
        let (built, reused) = (after.built - before.built, after.reused - before.reused);
        let mean_us = |ns: u64, count: u64| ns as f64 / 1000.0 / count.max(1) as f64;
        cx.log(format_args!(
            "{presented} pictures presented in {} frames: {reused} reused the kept frame ({:.0} us each on the UI thread), {built} were built ({:.0} us each over the whole run)",
            built + reused,
            mean_us(after.reused_ns - before.reused_ns, reused),
            mean_us(after.built_ns, after.built),
        ));
        ensure!(
            presented >= frames - frames / 20,
            "{presented} of {frames} pictures presented"
        );
        // Another window taking the focus is a frame of its own: a few may be built.
        ensure!(
            built <= 4,
            "{built} frames wrote the scene to the GPU while only pictures changed"
        );
        ensure!(
            reused >= presented.saturating_sub(4),
            "{reused} of {presented} presents reused the kept frame"
        );
        match allocations {
            Some(allocations) => {
                let per_present = allocations / (built + reused).max(1);
                cx.log(format_args!(
                    "UI thread: {allocations} allocations for {} presents: {per_present} each (budget {ALLOCATIONS_PER_PRESENT})",
                    built + reused
                ));
                ensure!(
                    per_present <= ALLOCATIONS_PER_PRESENT,
                    "{per_present} allocations per present, over the budget of {ALLOCATIONS_PER_PRESENT}"
                );
            }
            None => cx.log(format_args!(
                "allocation budget ({ALLOCATIONS_PER_PRESENT} per present) not checked: run under crates/decibell-video/tools/alloc-count.c (see the file)"
            )),
        }
        Ok(())
    }
    .await;
    stage.close()?;
    checked
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

/// What the queue did around one gap of the decoder thread, sampled every half millisecond
/// from a thread of its own (the UI thread's sleeps are too coarse for it).
struct Gap {
    /// The deepest the queue got.
    max_queue: usize,
    /// Frames that arrived between the end of the gap and the first moment the queue was
    /// empty again (the stream back at its one-frame latency). `None`: it never was.
    frames_to_baseline: Option<u64>,
    /// And how long that took.
    back_after: Duration,
}

fn watch_gap(stream: &Stream, gap: Duration, watch: Duration) -> std::thread::JoinHandle<Gap> {
    let stream = stream.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut result = Gap {
            max_queue: 0,
            frames_to_baseline: None,
            back_after: Duration::ZERO,
        };
        let mut received_at_end = None;
        while started.elapsed() < watch {
            let stats = stream.stats();
            result.max_queue = result.max_queue.max(stats.queue);
            if started.elapsed() >= gap {
                let at_end = *received_at_end.get_or_insert(stats.received);
                if stats.queue == 0 && result.frames_to_baseline.is_none() {
                    result.frames_to_baseline = Some(stats.received - at_end);
                    result.back_after = started.elapsed() - gap;
                }
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        result
    })
}

/// The queue's depth in time (docs/video-design.md §0.4): the decoder thread of a 1440p120
/// stream is held back as a busy machine holds it back. 60 ms (seven frames, more than twice
/// the three the queue used to hold): nothing is shed, no keyframe is asked for, the backlog
/// is decoded at the decoder's speed and the stream is back at its one-frame latency within a
/// few frames. 400 ms: the queue overflows at 100 ms, sheds and asks for a keyframe as it
/// always did, and the stream goes on with the streamer's answer.
async fn starve(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1440p120_hevc.dbv"]) else {
        return Ok(());
    };
    let stage = Stage::open(&cx, &["selftest-starve".into()], &files, false)?;
    let stream = stage.streams[0].clone();
    let result = async {
        cx.settle().await?;
        let mut feeder = feed(&stage, 0, &files[0], None)?;
        // The streamer: a keyframe request is answered with a keyframe.
        stage.hub.set_callbacks(HubCallbacks {
            request_keyframe: Box::new({
                let answer = feeder.keyframe_answer();
                move |_| answer()
            }),
            ..Default::default()
        });
        stage
            .wait("the stream to play", Duration::from_secs(20), || {
                let stats = stream.stats();
                stats.published >= 360 && stats.queue == 0
            })
            .await?;
        let start = stream.stats();
        ensure!(
            start.dropped == 0,
            "{} frames dropped before the test began: the machine is too busy for it",
            start.dropped
        );
        cx.log(format_args!(
            "1440p120 HEVC on {}: decode p50 {:.2} ms, arrival to picture p50 {:.2} ms",
            start.backend.map_or("?", DecodeBackend::label),
            start.decode_ms_p50,
            start.latency_ms_p50
        ));

        // Short gaps: ridden out.
        const SHORT: Duration = Duration::from_millis(60);
        // Back at an empty queue within this many frames after the gap (100 ms of them).
        const BASELINE_WITHIN: u64 = 12;
        for round in 1..=3 {
            let before = stream.stats();
            let watching = watch_gap(&stream, SHORT, Duration::from_millis(400));
            stream.hold_decoder(SHORT);
            cx.sleep(Duration::from_millis(500)).await;
            let gap = watching
                .join()
                .map_err(|_| anyhow::anyhow!("the sampling thread panicked"))?;
            let after = stream.stats();
            cx.log(format_args!(
                "60 ms gap {round}: queue up to {}, {} dropped, {} keyframe requests, {} pictures overtaken; queue empty again {:.1} ms and {} frames after the gap",
                gap.max_queue,
                after.dropped - before.dropped,
                after.keyframe_requests - before.keyframe_requests,
                after.replaced - before.replaced,
                gap.back_after.as_secs_f64() * 1000.0,
                gap.frames_to_baseline.map_or("?".into(), |n| n.to_string()),
            ));
            ensure!(
                gap.max_queue >= 5,
                "the queue only got {} deep: the decoder thread was not held",
                gap.max_queue
            );
            ensure!(
                after.dropped == before.dropped,
                "a 60 ms gap shed {} frames",
                after.dropped - before.dropped
            );
            ensure!(
                after.keyframe_requests == before.keyframe_requests,
                "a 60 ms gap asked for a keyframe"
            );
            ensure!(after.errors == 0, "{} decoder errors", after.errors);
            let Some(frames) = gap.frames_to_baseline else {
                bail!("the queue was never empty again in the 340 ms after the gap");
            };
            ensure!(
                frames <= BASELINE_WITHIN,
                "back at one frame of latency only {frames} frames after the gap (at most {BASELINE_WITHIN})"
            );
            ensure!(
                after.decoded - before.decoded == after.received - before.received - after.queue as u64 + before.queue as u64,
                "frames went missing: {} received, {} decoded",
                after.received - before.received,
                after.decoded - before.decoded
            );
        }

        // A long gap: shed at the queue's depth, a keyframe asked for, the stream goes on.
        let before = stream.stats();
        let watching = watch_gap(&stream, Duration::from_millis(400), Duration::from_millis(1500));
        stream.hold_decoder(Duration::from_millis(400));
        cx.sleep(Duration::from_millis(1600)).await;
        let gap = watching
            .join()
            .map_err(|_| anyhow::anyhow!("the sampling thread panicked"))?;
        stage
            .wait("the stream to go on after the long gap", Duration::from_secs(10), || {
                let stats = stream.stats();
                stats.published >= before.published + 240 && stats.queue == 0
            })
            .await?;
        let after = stream.stats();
        let asked = after.keyframe_requests - before.keyframe_requests;
        cx.log(format_args!(
            "400 ms gap: queue up to {} (100 ms of frames), {} frames shed, {asked} keyframe requests; playing again on the same decoder ({} opens)",
            gap.max_queue,
            after.dropped - before.dropped,
            after.decoder_opens,
        ));
        ensure!(
            after.dropped > before.dropped,
            "a 400 ms gap shed nothing"
        );
        // 100 ms at 120 fps is 12 frames; a frame more or less with the arrival jitter.
        ensure!(
            (10..=14).contains(&gap.max_queue),
            "the queue held {} frames when it overflowed (100 ms at 120 fps is 12)",
            gap.max_queue
        );
        ensure!(
            (1..=2).contains(&asked),
            "{asked} keyframe requests for one long gap"
        );
        ensure!(
            (after.errors, after.decoder_opens) == (0, before.decoder_opens),
            "the decoder did not survive: {} errors, {} opens",
            after.errors,
            after.decoder_opens
        );
        feeder.stop();
        Ok(())
    }
    .await;
    stage.hub.set_callbacks(HubCallbacks::default());
    stage.close()?;
    result
}

/// A stream whose frames stop arriving: after `STALLED_AFTER` the hub says so (once), asks for
/// a keyframe and frees the decoder; nothing is drawn and no thread runs while it waits; the
/// streamer's next keyframe brings the picture back and the hub says that too.
async fn stall(cx: TestCx) -> Result<()> {
    let Some(files) = streams(&cx, &["1080p60_h264.dbv"]) else {
        return Ok(());
    };
    let stage = Stage::open(&cx, &["selftest-stall".into()], &files, false)?;
    let stream = stage.streams[0].clone();
    let events: std::sync::Arc<parking_lot::Mutex<Vec<StreamEvent>>> = Default::default();
    let result = async {
        cx.settle().await?;
        let mut feeder = feed(&stage, 0, &files[0], None)?;
        stage.hub.set_callbacks(HubCallbacks {
            request_keyframe: Box::new({
                let answer = feeder.keyframe_answer();
                move |_| answer()
            }),
            event: Box::new({
                let events = events.clone();
                move |_, event| events.lock().push(event)
            }),
            ..Default::default()
        });
        let count = |wanted: &StreamEvent| events.lock().iter().filter(|e| *e == wanted).count();
        stage
            .wait("the stream to play", Duration::from_secs(20), || {
                stream.stats().presented >= 120
            })
            .await?;
        ensure!(!stream.is_stalled(), "stalled while playing");

        // The streamer stops sending.
        feeder.pause(true);
        let paused = Instant::now();
        let requests = stream.stats().keyframe_requests;
        stage
            .wait("the stall", STALLED_AFTER + Duration::from_secs(3), || {
                stream.is_stalled()
            })
            .await?;
        let after = paused.elapsed();
        cx.log(format_args!(
            "no frames: stalled after {:.2} s (the threshold is {} s), {} keyframe request",
            after.as_secs_f64(),
            STALLED_AFTER.as_secs(),
            stream.stats().keyframe_requests - requests
        ));
        ensure!(
            after >= STALLED_AFTER - Duration::from_millis(100),
            "stalled after only {after:?}"
        );
        ensure!(
            count(&StreamEvent::Stalled) == 1,
            "{} Stalled events",
            count(&StreamEvent::Stalled)
        );
        ensure!(
            stream.stats().keyframe_requests == requests + 1,
            "the stall did not ask for a keyframe"
        );
        stage
            .wait("the decoder thread to exit", Duration::from_secs(2), || {
                stage.hub.decoder_threads() == 0
            })
            .await?;
        ensure!(
            stream.picture_size() == Some((1920, 1080)),
            "the last picture is gone"
        );
        // Stalled costs nothing: no frame, no decoder thread, for as long as it lasts.
        let mut quiet = None;
        for _ in 0..3 {
            let before = stage.frames()?;
            cx.sleep(Duration::from_secs(2)).await;
            let frames = stage.frames()?.since(before);
            quiet = Some(frames);
            if frames.drawn == 0 && frames.presented == 0 {
                break;
            }
        }
        let quiet = quiet.context("no measurement")?;
        cx.log(format_args!(
            "stalled for 2 s: {} frames drawn, {} presented, {} decoder threads, {} Stalled events in all",
            quiet.drawn,
            quiet.presented,
            stage.hub.decoder_threads(),
            count(&StreamEvent::Stalled)
        ));
        ensure!(
            quiet.drawn == 0 && quiet.presented == 0,
            "the stalled stream's window drew {} and presented {} frames in 2 s",
            quiet.drawn,
            quiet.presented
        );
        ensure!(count(&StreamEvent::Stalled) == 1, "the stall was reported again");

        // The streamer is back: its frames are refused until the keyframe that is asked for.
        let published = stream.stats().published;
        feeder.pause(false);
        let resumed = Instant::now();
        stage
            .wait("pictures again", Duration::from_secs(5), || !stream.is_stalled())
            .await?;
        cx.log(format_args!(
            "frames again: a picture {:.0} ms later, {} decoder opens in all",
            resumed.elapsed().as_secs_f64() * 1000.0,
            stream.stats().decoder_opens
        ));
        ensure!(
            count(&StreamEvent::Resumed) == 1,
            "{} Resumed events",
            count(&StreamEvent::Resumed)
        );
        stage
            .wait("the stream to play again", Duration::from_secs(5), || {
                stream.stats().published >= published + 60
            })
            .await?;
        ensure!(
            count(&StreamEvent::Stalled) == 1 && !stream.is_stalled(),
            "stalled again while playing"
        );
        feeder.stop();
        Ok(())
    }
    .await;
    stage.hub.set_callbacks(HubCallbacks::default());
    stage.close()?;
    result
}
