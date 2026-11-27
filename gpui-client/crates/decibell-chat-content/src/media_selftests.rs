//! Self-tests of the playback engine on the real renderer: a window of their own with one
//! `VideoView` on a `decibell_media::Player`, the frame the window drew read back from the GPU.
//! For what a headless test cannot show: that the surface's draw turns a rotated file, which
//! decoder a codec gets on this machine and that its pixels are right.
//!
//! Never audible: every player here renders into `TestOutput::null`.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx};
use decibell_media::{Open, Phase, Player, TestOutput};
use decibell_video::{Fit, VideoHub, VideoView};
use gpui::{
    AppContext as _, Bounds, IntoElement, ParentElement as _, Render, Styled as _, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, div,
    px, rgb, size,
};

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "media-rotation",
            about: "files with a display matrix (90 / 180 / 270): the window's frame read back, the stored picture's marked corners are where the turned picture has them and its box has the turned aspect ratio",
            run: |cx| Box::pin(check_rotation(cx)),
        },
        SelfTest {
            name: "media-10bit",
            about: "10-bit HEVC, AV1 and VP9 files, and PQ / HLG ones: each on every decoder of the order against software, the drawn grey against the SDR value (for HDR: against the conversion's own formula)",
            run: |cx| Box::pin(check_ten_bit(cx)),
        },
        SelfTest {
            name: "media-output-device",
            about: "attachment sound follows Settings → Audio (two PipeWire null sinks, nothing audible): the voice output device with the separate stream output off, the stream-output device with it on, moved while playing; the clip's clicks arrive on the sink's monitor and no stream of ours is on the desktop's default device",
            run: |cx| Box::pin(check_output_device(cx)),
        },
        SelfTest {
            name: "media-vp9-hw",
            about: "a VP9 file on each hardware decoder of this machine's order: the lazy probe, the first picture against software pixel for pixel, the file played to its end; then the order as it is (which decoder a VP9 file gets)",
            run: |cx| Box::pin(check_vp9_hw(cx)),
        },
    ]
}

/// Mean and largest absolute difference between two frames of the same window.
fn difference(a: &Shot, b: &Shot) -> Result<(f64, u8)> {
    ensure!((a.width, a.height) == (b.width, b.height), "the two frames differ in size");
    let (mut sum, mut worst) = (0u64, 0u8);
    for (x, y) in a.rgba.iter().zip(&b.rgba) {
        let d = x.abs_diff(*y);
        sum += d as u64;
        worst = worst.max(d);
    }
    Ok((sum as f64 / a.rgba.len().max(1) as f64, worst))
}

/// What one decoder did with a file.
struct Played {
    shot: Shot,
    shown: u64,
    late: u64,
    backend: decibell_video::DecodeBackend,
    first_picture: Duration,
}

/// Opens `bytes` with exactly the decoders `order` (the machine's own when `None`), takes the
/// window's frame with the first picture up, then plays the file to its end.
async fn play_with(cx: &TestCx, key: &str, bytes: &[u8], order: Option<&[decibell_video::DecodeBackend]>, pictures: u64) -> Result<Played> {
    let hub = cx.app().update(VideoHub::global);
    hub.set_decoder_order(order);
    let stage = Stage::open(cx, key, bytes.to_vec(), false, (640., 520.))?;
    let result = async {
        let first_picture = stage.wait("the first picture", Duration::from_secs(10), |player| player.status().has_picture).await?;
        cx.sleep(Duration::from_millis(300)).await;
        let shot = stage.shot().await?;
        ensure!(shot.picture().is_some(), "the window shows no picture");
        stage.player.play();
        stage.wait("the end of the file", Duration::from_secs(20), |player| player.status().phase == Phase::Ended).await?;
        let stats = stage.player.stats();
        let backend = stage.player.status().video.and_then(|video| video.backend).context("no decoder reported")?;
        ensure!(stats.shown + 1 >= pictures, "{} of {pictures} pictures shown ({} late)", stats.shown, stats.late);
        anyhow::Ok(Played { shot, shown: stats.shown, late: stats.late, backend, first_picture })
    }
    .await;
    stage.close();
    hub.set_decoder_order(None);
    result
}

/// `bytes` on every hardware decoder of the machine's order, each against software; then with
/// the order as it is. Returns the decoder the file gets by itself.
async fn check_on_every_decoder(cx: &TestCx, name: &str, codec: decibell_video::Codec, bytes: &[u8], pictures: u64) -> Result<decibell_video::DecodeBackend> {
    Ok(check_decoders(cx, name, codec, bytes, pictures, true).await?.0)
}

/// As [`check_on_every_decoder`]; `strict`: a hardware decoder that fails must have been given
/// up for the codec with a reason (a 10-bit file's failure is the file's own and is only
/// logged). Also returns the software decoder's frame.
async fn check_decoders(cx: &TestCx, name: &str, codec: decibell_video::Codec, bytes: &[u8], pictures: u64, strict: bool) -> Result<(decibell_video::DecodeBackend, Shot)> {
    use decibell_video::{DecodeBackend, DecodePath};
    let hub = cx.app().update(VideoHub::global);
    let order = hub.decoder_order();
    let software = play_with(cx, &format!("{name}:software"), bytes, Some(&[DecodeBackend::Software]), pictures).await.context("software")?;
    cx.log(format_args!("{name}: software: {} pictures shown, {} late, first picture after {:.0} ms", software.shown, software.late, software.first_picture.as_secs_f64() * 1e3));
    let mut worked = 0;
    for backend in order.iter().copied().filter(|backend| backend.path() == DecodePath::Hardware) {
        match play_with(cx, &format!("{name}:{}", backend.label()), bytes, Some(&[backend]), pictures).await {
            Ok(played) => {
                let (mean, worst) = difference(&played.shot, &software.shot)?;
                cx.log(format_args!(
                    "{name}: {}: {} pictures shown, {} late, first picture after {:.0} ms; against software: mean difference {mean:.3}, largest {worst} (of 255)",
                    backend.label(),
                    played.shown,
                    played.late,
                    played.first_picture.as_secs_f64() * 1e3
                ));
                ensure!(played.backend == backend, "{name}: asked for {}, got {}", backend.label(), played.backend.label());
                ensure!(mean < 1.0 && worst <= 12, "{name}: {} does not draw what software draws (mean {mean:.3}, largest {worst})", backend.label());
                worked += 1;
            }
            Err(error) => {
                // A decoder this GPU does not have for the codec is given up with its reason; a
                // decoder that opened and drew something else is a failure.
                let reason = hub.skip_reason(codec, backend);
                cx.log(format_args!("{name}: {}: not on this machine: {}", backend.label(), reason.clone().unwrap_or_else(|| format!("{error:#}"))));
                ensure!(!strict || reason.is_some(), "{name}: {} failed without being given up: {error:#}", backend.label());
            }
        }
    }
    let own = play_with(cx, &format!("{name}:own"), bytes, None, pictures).await.context("the machine's own order")?;
    cx.log(format_args!("{name}: by itself the file is decoded with {} ({} of {} hardware decoders work)", own.backend.label(), worked, order.iter().filter(|b| b.path() == DecodePath::Hardware).count()));
    Ok((own.backend, software.shot))
}

/// What the renderer's `surface_to_sdr` gives for a grey whose gamma-encoded value is `e`
/// (`transfer`: 1 PQ, 2 HLG), as an 8-bit value: the stated approximation, computed here.
fn expected_sdr_grey(e: f64, transfer: u32) -> f64 {
    let light = if transfer == 1 {
        let p = e.powf(1. / 78.84375);
        10000. * ((p - 0.8359375).max(0.) / (18.8515625 - 18.6875 * p)).powf(1. / 0.1593017578125) / 203.
    } else {
        let scene = if e <= 0.5 { e * e / 3. } else { (((e - 0.55991073) / 0.17883277).exp() + 0.28466892) / 12. };
        scene * scene.max(1e-6).powf(0.2) * (1000. / 203.)
    };
    // A grey stays a grey through the primaries' matrix (its rows sum to one).
    let peak: f64 = 1000. / 203.;
    let mapped = (light * (1. + light / (peak * peak)) / (1. + light)).clamp(0., 1.);
    let encoded = if mapped <= 0.0031308 { mapped * 12.92 } else { 1.055 * mapped.powf(1. / 2.4) - 0.055 };
    encoded * 255.
}

async fn check_ten_bit(cx: TestCx) -> Result<()> {
    use decibell_video::Codec;
    let hub = cx.app().update(VideoHub::global);
    // The test picture: a grey field of code 96 (8-bit, limited range) under the barcode, whose
    // cells are black in the first picture. As R'G'B' that grey is (96 - 16) / 219.
    let grey = (96. - 16.) / 219.;
    let files: [(&str, Codec, &[u8], u32); 5] = [
        ("hevc10.mp4", Codec::Hevc, include_bytes!("../../decibell-media/tests/assets/hevc10.mp4"), 0),
        ("av1_10.mp4", Codec::Av1, include_bytes!("../../decibell-media/tests/assets/av1_10.mp4"), 0),
        ("vp9_p2.webm", Codec::Vp9, include_bytes!("../../decibell-media/tests/assets/vp9_p2.webm"), 0),
        ("hevc10_pq.mp4", Codec::Hevc, include_bytes!("../../decibell-media/tests/assets/hevc10_pq.mp4"), 1),
        ("hevc10_hlg.mp4", Codec::Hevc, include_bytes!("../../decibell-media/tests/assets/hevc10_hlg.mp4"), 2),
    ];
    let mut by_itself = Vec::new();
    for (name, codec, bytes, transfer) in files {
        let (own, shot) = check_decoders(&cx, name, codec, bytes, 50, false).await?;
        by_itself.push(format!("{name} {}", own.label()));
        let picture = shot.picture().context("no picture")?;
        let (field, cells) = (shot.luma(picture, 0.5, 0.75), shot.luma(picture, 0.5, 0.25));
        let expected = match transfer {
            0 => grey * 255.,
            transfer => expected_sdr_grey(grey, transfer),
        };
        cx.log(format_args!(
            "{name}: the grey field is drawn at {field:.1} of 255 (expected {expected:.1}: {}), the barcode's black cells at {cells:.1}",
            match transfer {
                0 => "the SDR value as it is",
                1 => "PQ on BT.2020 brought to the SDR target",
                _ => "HLG on BT.2020 brought to the SDR target",
            }
        ));
        ensure!((field as f64 - expected).abs() <= 6., "{name}: the grey field is {field:.1}, expected {expected:.1}");
        ensure!(cells < 12., "{name}: black is drawn at {cells:.1}");
    }
    cx.log(format_args!("by itself: {}; 16-bit plane textures on this device: {}", by_itself.join(", "), hub.deep_planes()));
    Ok(())
}

async fn check_vp9_hw(cx: TestCx) -> Result<()> {
    let own = check_on_every_decoder(&cx, "vp9_hw.webm", decibell_video::Codec::Vp9, include_bytes!("../../decibell-media/tests/assets/vp9_hw.webm"), 50).await?;
    let hub = cx.app().update(VideoHub::global);
    let hardware = hub.decoder_order().into_iter().any(|backend| backend.path() == decibell_video::DecodePath::Hardware);
    if hardware && own == decibell_video::DecodeBackend::Software {
        cx.log("NOTE: this machine has hardware decoders and none of them decodes VP9: the file is decoded in software");
    }
    Ok(())
}

/// What is behind the picture: a colour no test picture has.
const BACKDROP: u32 = 0xff00ff;

struct StageView {
    viewer: Option<decibell_video::Viewer>,
}

impl Render for StageView {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(BACKDROP))
            .children(self.viewer.as_ref().map(|viewer| VideoView::new(viewer).fit(Fit::Contain)))
    }
}

/// A window showing one player's picture.
pub(crate) struct Stage {
    cx: TestCx,
    window: WindowHandle<StageView>,
    pub player: Player,
}

/// A frame of the window as the renderer drew it, as RGB.
pub(crate) struct Shot {
    pub width: u32,
    pub height: u32,
    rgba: Vec<u8>,
    bgr: bool,
}

impl Shot {
    pub fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 4) as usize;
        let p = &self.rgba[at..at + 3];
        if self.bgr { [p[2], p[1], p[0]] } else { [p[0], p[1], p[2]] }
    }

    fn is_backdrop(&self, x: u32, y: u32) -> bool {
        let [r, g, b] = self.rgb(x, y);
        r > 200 && g < 60 && b > 200
    }

    /// The picture's box: everything that is not the backdrop (left, top, width, height).
    pub fn picture(&self) -> Option<(u32, u32, u32, u32)> {
        let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..self.height {
            for x in 0..self.width {
                if !self.is_backdrop(x, y) {
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
            }
        }
        (left <= right && top <= bottom).then(|| (left, top, right - left + 1, bottom - top + 1))
    }

    /// The mean luma (0..255) of a small square around a point given as fractions of `picture`.
    pub fn luma(&self, picture: (u32, u32, u32, u32), fx: f32, fy: f32) -> f32 {
        let (cx, cy) = (picture.0 as f32 + picture.2 as f32 * fx, picture.1 as f32 + picture.3 as f32 * fy);
        let mut sum = 0.;
        let mut count = 0.;
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let [r, g, b] = self.rgb((cx as i32 + dx).max(0) as u32, (cy as i32 + dy).max(0) as u32);
                sum += 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                count += 1.;
            }
        }
        sum / count
    }
}

impl Stage {
    /// Opens the window and a player on `bytes` (paused on its first picture unless `play`).
    pub fn open(cx: &TestCx, key: &str, bytes: Vec<u8>, play: bool, window_size: (f32, f32)) -> Result<Stage> {
        let (window, player) = cx.app().update(|cx| -> Result<_> {
            let hub = VideoHub::global(cx);
            let player = Player::open(Open {
                source: Box::new(std::io::Cursor::new(bytes)),
                video: Some(hub),
                key: key.into(),
                output: TestOutput::null(48_000),
                start_at: 0,
                play,
                volume: 1.0,
                muted: false,
                on_change: Box::new(|| {}),
            });
            let viewer = player.viewer().cloned();
            let bounds = Bounds::centered(None, size(px(window_size.0), px(window_size.1)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions { title: Some("Decibell media self-test".into()), ..Default::default() }),
                    // A file keeps its frame rate in an unfocused window.
                    inactive_frame_interval: None,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| StageView { viewer }),
            )?;
            Ok((window, player))
        })?;
        Ok(Stage { cx: cx.clone(), window, player })
    }

    pub async fn wait(&self, what: &str, limit: Duration, mut done: impl FnMut(&Player) -> bool) -> Result<Duration> {
        let started = Instant::now();
        loop {
            if done(&self.player) {
                return Ok(started.elapsed());
            }
            if let Phase::Failed(reason) = self.player.status().phase {
                bail!("{what}: the player failed: {reason}");
            }
            ensure!(started.elapsed() < limit, "{what}: not within {limit:?}");
            self.cx.sleep(Duration::from_millis(20)).await;
        }
    }

    /// The window's next frame, read back from the GPU.
    #[cfg(target_os = "linux")]
    pub async fn shot(&self) -> Result<Shot> {
        let receiver = gpui_wgpu::capture_next_surface_frame(true);
        self.cx.app().update(|cx| self.window.update(cx, |_, window, _| window.request_present()).context("the media test window is closed"))?;
        let started = Instant::now();
        loop {
            if let Ok(frame) = receiver.try_recv() {
                let bgr = matches!(frame.format, gpui_wgpu::wgpu::TextureFormat::Bgra8Unorm | gpui_wgpu::wgpu::TextureFormat::Bgra8UnormSrgb);
                return Ok(Shot { width: frame.width, height: frame.height, rgba: frame.pixels, bgr });
            }
            ensure!(started.elapsed() < Duration::from_secs(5), "no captured frame");
            self.cx.sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn shot(&self) -> Result<Shot> {
        bail!("reading a window's frame back is built for the wgpu renderer only")
    }

    pub fn close(self) {
        let Stage { cx, window, player } = self;
        drop(player);
        let _ = cx.app().update(|cx| window.update(cx, |_, window, _| window.remove_window()));
    }
}

/// The stored test picture (`tests/assets/make.py`, `mark=True`): grey, the barcode over its top
/// half, its bottom-left 16 x 16 white and its bottom-right 16 x 16 black. Where those two marks
/// and the barcode's side are in the drawn picture, per file.
async fn check_rotation(cx: TestCx) -> Result<()> {
    struct Case {
        name: &'static str,
        bytes: &'static [u8],
        /// Width over height of the drawn picture.
        aspect: f32,
        /// Centre of the white mark, the black mark, a point of plain grey: fractions of the box.
        white: (f32, f32),
        black: (f32, f32),
        grey: (f32, f32),
    }
    // 8 px in from the corner of a 128 x 72 picture.
    let (near_x, near_y) = (8. / 128., 8. / 72.);
    let cases = [
        // Turned a quarter clockwise: stored bottom-left is drawn top-left, stored bottom-right
        // bottom-left... of a 72 x 128 box.
        Case {
            name: "rot90.mp4",
            bytes: include_bytes!("../../decibell-media/tests/assets/rot90.mp4"),
            aspect: 72. / 128.,
            white: (near_y, near_x),
            black: (near_y, 1. - near_x),
            grey: (0.35, 0.5),
        },
        Case {
            name: "rot180.mp4",
            bytes: include_bytes!("../../decibell-media/tests/assets/rot180.mp4"),
            aspect: 128. / 72.,
            white: (1. - near_x, near_y),
            black: (near_x, near_y),
            grey: (0.5, 0.35),
        },
        Case {
            name: "rot270.mkv",
            bytes: include_bytes!("../../decibell-media/tests/assets/rot270.mkv"),
            aspect: 72. / 128.,
            white: (1. - near_y, 1. - near_x),
            black: (1. - near_y, near_x),
            grey: (0.65, 0.5),
        },
    ];
    for case in cases {
        let stage = Stage::open(&cx, &format!("media-rotation:{}", case.name), case.bytes.to_vec(), false, (640., 640.))?;
        let result = async {
            stage.wait("the first picture", Duration::from_secs(10), |player| player.status().has_picture).await?;
            cx.sleep(Duration::from_millis(300)).await;
            let shot = stage.shot().await?;
            let picture = shot.picture().context("the window shows no picture")?;
            let aspect = picture.2 as f32 / picture.3 as f32;
            let (white, black, grey) = (
                shot.luma(picture, case.white.0, case.white.1),
                shot.luma(picture, case.black.0, case.black.1),
                shot.luma(picture, case.grey.0, case.grey.1),
            );
            let video = stage.player.status().video.context("no video")?;
            cx.log(format_args!(
                "{}: drawn {}x{} px in a {}x{} window (aspect {aspect:.3}, the file's turned size {}x{}), decoder {}; white mark {white:.0}, black mark {black:.0}, grey field {grey:.0}",
                case.name,
                picture.2,
                picture.3,
                shot.width,
                shot.height,
                video.width,
                video.height,
                video.backend.map_or("?", |backend| backend.label()),
            ));
            ensure!((aspect - case.aspect).abs() < 0.02, "{}: the box is {aspect:.3}, expected {:.3}", case.name, case.aspect);
            ensure!(white > 190., "{}: the white mark is not where the turned picture has it ({white:.0})", case.name);
            ensure!(black < 50., "{}: the black mark is not where the turned picture has it ({black:.0})", case.name);
            ensure!((grey - 96.).abs() < 30., "{}: the grey field reads {grey:.0}", case.name);
            anyhow::Ok(())
        }
        .await;
        stage.close();
        result?;
    }
    Ok(())
}

// ── The output device (a null sink of our own: `decibell_harness::test_audio` on the branch
// head has the same pattern; use it when the two meet) ──────────────────────────────────────

fn pactl(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("pactl").args(args).output().context("running pactl (is PipeWire / PulseAudio installed?)")?;
    ensure!(out.status.success(), "pactl {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A null sink, unloaded when dropped. Nothing played on it is audible.
struct NullSink {
    name: String,
    module: String,
}

impl NullSink {
    fn load(tag: &str) -> Result<NullSink> {
        let name = format!("decibell-selftest-{}-{tag}", std::process::id());
        let module = pactl(&["load-module", "module-null-sink", &format!("sink_name={name}"), "rate=48000", "channels=2", &format!("sink_properties=device.description={name}")])?;
        Ok(NullSink { name, module: module.trim().to_string() })
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        let _ = pactl(&["unload-module", &self.module]);
    }
}

/// The sinks this process's playback streams sit on, by name.
fn our_sinks() -> Result<Vec<String>> {
    let sinks: Vec<(String, String)> = pactl(&["list", "short", "sinks"])?
        .lines()
        .filter_map(|line| {
            let mut columns = line.split('\t');
            Some((columns.next()?.to_string(), columns.next()?.to_string()))
        })
        .collect();
    let pid = format!("application.process.id = \"{}\"", std::process::id());
    let mut found = Vec::new();
    for input in pactl(&["list", "sink-inputs"])?.split("Sink Input #").skip(1) {
        if !input.contains(&pid) {
            continue;
        }
        let sink = input.lines().find_map(|line| line.trim().strip_prefix("Sink: ")).unwrap_or("?").trim().to_string();
        found.push(sinks.iter().find(|(index, _)| *index == sink).map_or(format!("#{sink}"), |(_, name)| name.clone()));
    }
    Ok(found)
}

/// The largest sample (0..1) heard on a sink's monitor over `seconds`.
async fn monitor_peak(cx: &TestCx, sink: &str, seconds: f64) -> Result<f32> {
    let monitor = format!("{sink}.monitor");
    let mut child = std::process::Command::new("parec")
        .args(["--device", &monitor, "--format=s16le", "--rate=48000", "--channels=1", "--raw", "--latency-msec=40"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("running parec")?;
    cx.sleep(Duration::from_secs_f64(seconds)).await;
    let _ = child.kill();
    let output = child.wait_with_output()?;
    Ok(output.stdout.as_chunks::<2>().0.iter().map(|sample| (i16::from_le_bytes(*sample) as f32 / 32768.).abs()).fold(0., f32::max))
}

async fn check_output_device(cx: TestCx) -> Result<()> {
    use decibell_model::Model;
    if !cfg!(target_os = "linux") || pactl(&["info"]).is_err() {
        cx.log("SKIPPED: needs pactl (PipeWire / PulseAudio)");
        return Ok(());
    }
    ensure!(std::env::var("DECIBELL_MEDIA_OUTPUT").is_err(), "this test plays on null sinks of its own: run it without DECIBELL_MEDIA_OUTPUT");
    let default_sink = pactl(&["get-default-sink"])?.trim().to_string();
    let (voice, stream) = (NullSink::load("voice")?, NullSink::load("stream")?);
    ensure!(pactl(&["get-default-sink"])?.trim() == default_sink, "loading a null sink moved the desktop's default sink");
    let (model, playback) = cx.app().update(|cx| (cx.global::<Model>().clone(), crate::playback::Playback::global(cx)));
    let playback = playback.context("no playback")?;
    let saved = cx.app().update(|cx| model.settings.read(cx).get().clone());
    let set = |change: Box<dyn FnOnce(&mut decibell_core::config::AppSettings)>| {
        cx.app().update(|cx| model.settings.update(cx, |settings, cx| settings.change(change, cx)));
    };
    // The voice output is one sink, the stream output the other; the switch is off.
    let (voice_name, stream_name) = (voice.name.clone(), stream.name.clone());
    set(Box::new(move |values| {
        values.output_device = Some(voice_name);
        values.stream_output_device = Some(stream_name);
        values.separate_stream_output = false;
        // Quiet even on a null sink; the clicks are still far above the monitor's silence.
        values.media_audio_volume = Some(0.25);
        values.media_audio_muted = false;
    }));
    let clip: std::sync::Arc<Vec<u8>> = std::sync::Arc::new(include_bytes!("../../decibell-media/tests/assets/sync60.mp4").to_vec());
    let media = decibell_model::chat::viewers::ActiveMedia {
        attachment_id: 990_001,
        server: decibell_model::ServerId::from_wire("media-output-test"),
        channel: decibell_model::ChannelId::from("media-output-test"),
        filename: "clip".into(),
        width: 0,
        height: 0,
    };
    cx.app().update(|cx| {
        playback.update(cx, |playback, _| {
            playback.set_source_provider(std::rc::Rc::new(move |_, _, _| Some(Box::new(std::io::Cursor::new(clip.to_vec())) as Box<dyn decibell_media::Source>)))
        });
        model.chat.audio.update(cx, |audio, cx| audio.set_active(Some(media), cx));
    });
    let result = async {
        // The clip's first click is half a second in: where the stream sits is known before.
        let on = |what: &'static str, expected: String| {
            let cx = cx.clone();
            let default_sink = default_sink.clone();
            async move {
                let started = Instant::now();
                loop {
                    let sinks = our_sinks()?;
                    ensure!(!sinks.contains(&default_sink), "{what}: a stream of ours is on the desktop's default sink ({default_sink})");
                    if sinks == [expected.clone()] {
                        return Ok(started.elapsed());
                    }
                    ensure!(started.elapsed() < Duration::from_secs(5), "{what}: our streams are on {sinks:?}, expected [{expected}]");
                    cx.sleep(Duration::from_millis(40)).await;
                }
            }
        };
        let took = on("the switch off", voice.name.clone()).await?;
        let (heard, other) = (monitor_peak(&cx, &voice.name, 1.6).await?, monitor_peak(&cx, &stream.name, 0.6).await?);
        cx.log(format_args!("separate stream output off: the stream is on the voice output sink after {:.0} ms; its monitor peaks at {heard:.3}, the stream-output sink's at {other:.3}", took.as_secs_f64() * 1e3));
        ensure!(heard > 0.02 && other < 0.002, "the clip is not (only) on the voice output sink: {heard:.3} / {other:.3}");
        let before = cx.app().update(|cx| model.chat.audio.read(cx).time_secs);
        // The switch on, while playing: the sound moves to the stream-output sink.
        set(Box::new(|values| values.separate_stream_output = true));
        let took = on("the switch on", stream.name.clone()).await?;
        let (heard, other) = (monitor_peak(&cx, &stream.name, 1.6).await?, monitor_peak(&cx, &voice.name, 0.6).await?);
        cx.log(format_args!("separate stream output on: moved to the stream-output sink in {:.0} ms; its monitor peaks at {heard:.3}, the voice sink's at {other:.3}", took.as_secs_f64() * 1e3));
        ensure!(heard > 0.02 && other < 0.002, "the clip is not (only) on the stream-output sink: {heard:.3} / {other:.3}");
        // The device behind the switch changes: back to the first sink by its name.
        let name = voice.name.clone();
        set(Box::new(move |values| values.stream_output_device = Some(name)));
        let took = on("another stream-output device", voice.name.clone()).await?;
        let heard = monitor_peak(&cx, &voice.name, 1.6).await?;
        let after = cx.app().update(|cx| model.chat.audio.read(cx).time_secs);
        cx.log(format_args!("the stream-output device changed: moved in {:.0} ms; its monitor peaks at {heard:.3}; the file went from {before:.1} s to {after:.1} s meanwhile", took.as_secs_f64() * 1e3));
        ensure!(heard > 0.02, "no sound on the new device ({heard:.3})");
        ensure!(after - before > 3.0, "the file stood still across the moves ({before:.1} s to {after:.1} s)");
        ensure!(pactl(&["get-default-sink"])?.trim() == default_sink, "the desktop's default sink moved");
        anyhow::Ok(())
    }
    .await;
    // The player goes before its sinks do (a stream whose sink disappears is moved to the default).
    cx.app().update(|cx| model.chat.audio.update(cx, |audio, cx| audio.set_active(None, cx)));
    cx.sleep(Duration::from_millis(900)).await;
    let left = our_sinks().unwrap_or_default();
    cx.app().update(|cx| model.settings.update(cx, |settings, cx| settings.change(move |values| *values = saved, cx)));
    drop((voice, stream));
    result?;
    ensure!(left.is_empty(), "streams of ours were still open after the player was dropped: {left:?}");
    Ok(())
}
