//! Encoder quality bench and the stream-contract tests that came out of
//! it (docs/superpowers/specs/2026-11-26-encoders-every-vendor-design.md,
//! "Quality").
//!
//! The bench (`quality_bench`, ignored) pushes each kind of screen content
//! (`encoder_content.rs`) through an encoder by the stream's own path and
//! prints, per configuration: the bitrate against its target over 1 s
//! windows, how large keyframes and the largest frames are (they become
//! packet bursts), how long a frame takes, and how close the decoded
//! pictures are to the source. With `DECIBELL_BENCH_DUMP=<dir>` it writes
//! decoded frames next to their source as PPM, to be looked at: PSNR says
//! little about whether small text reads (`DECIBELL_BENCH_DUMP_FRAMES=59,60`
//! picks the frames).
//!
//! ```sh
//! PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
//!   cargo test --release --lib quality_bench -- --ignored --nocapture
//! ```
//! `DECIBELL_ENCODER=nvenc|vaapi|software` picks the backend (default: the
//! machine's first), `DECIBELL_BENCH_SCENES=text,game`,
//! `DECIBELL_BENCH_CODECS=h264,hevc,av1`,
//! `DECIBELL_BENCH_ROWS=1280x720@30:3000,1920x1080@60:10000` (5 fps rows
//! are encoded as `detail` streams), `DECIBELL_BENCH_SECONDS` (default 6), `DECIBELL_BENCH_GOP` (seconds
//! between keyframes; default the stream's),
//! `DECIBELL_BENCH_UNPACED=1` (as fast as the encoder goes: no latency
//! column), `DECIBELL_BENCH_RATE_ONLY=1` (no decode and comparison: timing runs),
//! `DECIBELL_BENCH_FFLOG=1` (FFmpeg's debug log: NVENC prints the
//! configuration it opened with), and `DECIBELL_NVENC_OPTS` /
//! `DECIBELL_SVT_PARAMS` / `DECIBELL_SVT_PRESET` to try settings.
//!
//! The tests that run in every `cargo test --lib` (skipping where the
//! backend or a decoder is absent): colours come back exact through every
//! backend's conversion and signalling, the bitrate is within 10% of the
//! target, a frame's packet comes back from its own encode call, and a
//! requested keyframe is the very next frame.
#![cfg(all(test, target_os = "linux"))]

use std::time::{Duration, Instant};

use super::caps::CodecKind;
use super::encoder_backend::{self as backend, Backend, Os};
use super::encoder_checks::{
    check_packets, decode_pictures, encoder_lock, picture_rgb, reference_yuv, ssim, DecodeError, Expect, Framing, Matrix, Packet,
};
use super::encoder_content::{Content, Scene};
use super::encoder_linux::{EncoderConfig, EncoderTarget, H264Encoder};

pub struct Job {
    pub target: EncoderTarget,
    pub codec: CodecKind,
    pub scene: Scene,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub kbps: u32,
    pub frames: usize,
    pub detail: bool,
    /// Seconds between periodic keyframes (the stream's: `stream_gop_secs`).
    pub gop_secs: u32,
    /// Feed frames at the stream's rate (latency is then measured).
    pub paced: bool,
    pub forced_at: Vec<usize>,
}

#[derive(Default)]
pub struct Encoded {
    pub encoder_name: &'static str,
    pub packets: Vec<Packet>,
    /// Per frame: how long `encode_bgra_frame` held the encode thread, ms.
    pub call_ms: Vec<f64>,
    /// Per frame, paced runs: from the call to the frame's packet, ms.
    pub latency_ms: Vec<f64>,
    /// Frames whose packet did not come back from their own encode call
    /// (it came later: the encoder holds frames back).
    pub late: usize,
}

/// The keyframe interval a Linux stream runs with (encoder_thread_linux).
fn stream_gop_secs(detail: bool) -> u32 {
    if detail {
        super::bitrate_preset::DETAIL_KEYFRAME_SECS
    } else {
        super::encoder_thread_linux::KEYFRAME_INTERVAL_SECS
    }
}

fn framing(codec: CodecKind) -> Framing {
    if codec == CodecKind::H265 {
        Framing::LengthPrefixed
    } else {
        Framing::AnnexB
    }
}

fn to_packet(f: super::encoder_linux::EncodedFrame) -> Packet {
    Packet { data: f.data, key: f.is_keyframe, pts: f.pts as i64, description: f.avcc_description }
}

pub fn encode(job: &Job) -> Result<Encoded, String> {
    let _one_at_a_time = encoder_lock();
    let config = EncoderConfig {
        width: job.width,
        height: job.height,
        fps: job.fps,
        bitrate_kbps: job.kbps,
        keyframe_interval_secs: job.gop_secs,
        detail: job.detail,
        source_size: (job.width, job.height),
    };
    let mut encoder = H264Encoder::open(&job.target, job.codec, &config)?;
    let mut content = Content::new(job.scene, job.width, job.height, job.fps);
    let mut picture = vec![0u8; content.frame_bytes()];
    let mut out = Encoded { encoder_name: encoder.encoder_name(), ..Default::default() };
    let interval = Duration::from_secs_f64(1.0 / job.fps as f64);
    let start = Instant::now();
    let mut sent_at: Vec<Instant> = Vec::with_capacity(job.frames);
    for n in 0..job.frames {
        content.render(n as u32, &mut picture);
        if job.paced {
            let due = start + interval * n as u32;
            while Instant::now() < due {
                while let Some(f) = encoder.receive_pending() {
                    out.latency_ms.push(sent_at[f.pts as usize].elapsed().as_secs_f64() * 1e3);
                    out.packets.push(to_packet(f));
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        if job.forced_at.contains(&n) {
            encoder.force_keyframe();
        }
        let t = Instant::now();
        sent_at.push(t);
        let mut next = encoder.encode_bgra_frame(&picture, job.width, job.height, content.stride, true)?;
        out.call_ms.push(t.elapsed().as_secs_f64() * 1e3);
        let mut own = false;
        while let Some(f) = next {
            own |= f.pts as usize == n;
            out.latency_ms.push(sent_at[f.pts as usize].elapsed().as_secs_f64() * 1e3);
            out.packets.push(to_packet(f));
            next = encoder.receive_pending();
        }
        if !own {
            out.late += 1;
        }
    }
    out.packets.extend(encoder.flush().into_iter().map(to_packet));
    Ok(out)
}

fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() as f64 * p) as usize).min(s.len() - 1)]
}

/// What the packets alone say about a stream's rate.
#[derive(Debug, Default)]
pub struct Rate {
    pub kbps: f64,
    /// The quietest and the busiest second (a window of `fps` frames slid
    /// over the stream), kbps.
    pub window_min: f64,
    pub window_max: f64,
    /// Largest keyframe, and the 99th percentile frame, as multiples of
    /// the average frame.
    pub key_ratio: f64,
    pub p99_ratio: f64,
    /// Largest keyframe as a share of one second's budget at the target.
    pub key_seconds: f64,
}

pub fn rate(packets: &[Packet], fps: u32, target_kbps: u32) -> Rate {
    let sizes: Vec<f64> = packets.iter().map(|p| p.data.len() as f64).collect();
    if sizes.is_empty() {
        return Rate::default();
    }
    let total: f64 = sizes.iter().sum();
    let mean = total / sizes.len() as f64;
    let window = (fps as usize).min(sizes.len());
    let mut sum: f64 = sizes[..window].iter().sum();
    let (mut lo, mut hi) = (sum, sum);
    for i in window..sizes.len() {
        sum += sizes[i] - sizes[i - window];
        lo = lo.min(sum);
        hi = hi.max(sum);
    }
    let per_window = |bytes: f64| bytes * 8.0 / 1000.0 * fps as f64 / window as f64;
    let key = packets.iter().filter(|p| p.key).map(|p| p.data.len() as f64).fold(0.0, f64::max);
    Rate {
        kbps: total * 8.0 / 1000.0 * fps as f64 / sizes.len() as f64,
        window_min: per_window(lo),
        window_max: per_window(hi),
        key_ratio: key / mean,
        p99_ratio: percentile(&sizes, 0.99) / mean,
        key_seconds: key * 8.0 / (target_kbps as f64 * 1000.0),
    }
}

/// How close the decoded pictures are to the source.
#[derive(Debug, Default)]
pub struct Quality {
    pub decoded: usize,
    /// PSNR from the mean squared error over the compared frames, dB.
    pub psnr_y: f64,
    pub psnr_c: f64,
    pub ssim: f64,
    pub worst_psnr_y: f64,
    /// PSNR-Y of the frames in the half second after each keyframe but
    /// the first: where a keyframe squeezed into the buffer shows.
    pub after_key_psnr_y: f64,
    /// The colour description the decoder read: matrix / primaries /
    /// transfer / range (H.273 code points).
    pub colour: [i32; 4],
}

fn db(mse: f64) -> f64 {
    if mse <= 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

fn mse(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum::<f64>() / a.len().max(1) as f64
}

fn tight(plane: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for row in 0..h {
        out[row * w..][..w].copy_from_slice(&plane[row * stride..][..w]);
    }
    out
}

/// (NAL type, size) of each NAL unit of an Annex B H.264 access unit.
fn annexb_nal_sizes(data: &[u8]) -> Vec<(u8, usize)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .filter(|(_, &at)| at < data.len())
        .map(|(n, &at)| (data[at] & 0x1F, starts.get(n + 1).map_or(data.len(), |next| next - 3) - at))
        .collect()
}

fn write_ppm(path: &str, w: usize, h: usize, rgb: &[u8]) {
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    ppm.extend_from_slice(rgb);
    let _ = std::fs::write(path, ppm);
}

/// Decode the run and compare every `every`-th picture (and each
/// keyframe's half second) with the source, converted with the matrix and
/// range the stream itself declares: what a viewer's decoder would undo.
/// `Ok(None)`: this FFmpeg build has no decoder for the codec.
pub fn quality(job: &Job, out: &Encoded, every: usize, tag: &str) -> Result<Option<Quality>, String> {
    let (w, h) = (job.width as usize, job.height as usize);
    let mut content = Content::new(job.scene, job.width, job.height, job.fps);
    let mut source = vec![0u8; content.frame_bytes()];
    let keys: Vec<usize> = out.packets.iter().enumerate().filter(|(_, p)| p.key).map(|(i, _)| i).collect();
    let after_key = |i: usize| keys.iter().skip(1).any(|&k| i >= k && i < k + (job.fps as usize / 2).max(1));
    let dump = std::env::var("DECIBELL_BENCH_DUMP").ok();
    // Looked at: the frame before a mid-stream keyframe, the keyframe, the
    // one after, and one mid-GOP.
    let mid_key = keys.get(1).copied().unwrap_or(job.frames / 2);
    // DECIBELL_BENCH_DUMP_FRAMES=59,60,61 picks the frames instead.
    let dumped: Vec<usize> = match env_list("DECIBELL_BENCH_DUMP_FRAMES") {
        Some(items) => items.iter().filter_map(|s| s.parse().ok()).collect(),
        None => vec![mid_key.saturating_sub(1), mid_key, mid_key + 1, mid_key + job.fps as usize],
    };
    let mut q = Quality { worst_psnr_y: f64::MAX, ..Default::default() };
    let (mut sum_y, mut sum_c, mut sum_ssim, mut n) = (0.0, 0.0, 0.0, 0usize);
    let (mut sum_key, mut n_key) = (0.0, 0usize);
    let mut error = None;
    let decoded = decode_pictures(job.codec, framing(job.codec), &out.packets, |p| {
        if p.w != w || p.h != h {
            error = Some(format!("decoded {}x{}, encoded {w}x{h}", p.w, p.h));
            return;
        }
        q.colour = [p.matrix, p.primaries, p.transfer, p.range];
        let wanted_dump = dump.is_some() && dumped.contains(&p.index);
        if p.index % every != 0 && !after_key(p.index) && !wanted_dump {
            return;
        }
        content.render(p.index as u32, &mut source);
        let reference = reference_yuv(&source, content.stride, w, h, Matrix::of(p.matrix, h), p.range == 2);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let got = [tight(p.planes[0], p.strides[0], w, h), tight(p.planes[1], p.strides[1], cw, ch), tight(p.planes[2], p.strides[2], cw, ch)];
        let my = mse(&reference[0], &got[0]);
        if after_key(p.index) {
            sum_key += my;
            n_key += 1;
        }
        // DECIBELL_BENCH_TRACE=1: every compared frame's size and PSNR-Y,
        // to see how the picture recovers after a keyframe.
        if std::env::var_os("DECIBELL_BENCH_TRACE").is_some() {
            let packet = &out.packets[p.index];
            // H.264: how much of the packet is filler (NAL type 12) or
            // trailing zero bytes.
            let filler: usize = if matches!(job.codec, CodecKind::H264Hw) {
                annexb_nal_sizes(&packet.data).iter().filter(|(t, _)| *t == 12).map(|(_, n)| *n).sum()
            } else {
                0
            };
            println!(
                "  frame {:>4} {} {:>7} bytes ({filler} filler)  PSNR-Y {:.2}",
                p.index,
                if packet.key { "K" } else { " " },
                packet.data.len(),
                db(my)
            );
        }
        if p.index % every == 0 {
            sum_y += my;
            sum_c += (mse(&reference[1], &got[1]) + mse(&reference[2], &got[2])) / 2.0;
            sum_ssim += ssim(&reference[0], &got[0], w, h);
            q.worst_psnr_y = q.worst_psnr_y.min(db(my));
            n += 1;
        }
        if let (Some(dir), true) = (&dump, wanted_dump) {
            let mut rgb = vec![0u8; w * h * 3];
            for y in 0..h {
                for x in 0..w {
                    let px = &source[y * content.stride + x * 4..][..3];
                    rgb[(y * w + x) * 3..][..3].copy_from_slice(&[px[2], px[1], px[0]]);
                }
            }
            let base = format!("{dir}/{}-{}-{w}x{h}at{}-{:04}", job.scene.name(), out.encoder_name, job.fps, p.index);
            write_ppm(&format!("{base}-source.ppm"), w, h, &rgb);
            write_ppm(&format!("{base}-{tag}.ppm"), w, h, &picture_rgb(p));
        }
    });
    if let Some(e) = error {
        return Err(e);
    }
    match decoded {
        Ok(count) => {
            q.decoded = count;
            if n > 0 {
                q.psnr_y = db(sum_y / n as f64);
                q.psnr_c = db(sum_c / n as f64);
                q.ssim = sum_ssim / n as f64;
            }
            q.after_key_psnr_y = if n_key > 0 { db(sum_key / n_key as f64) } else { 0.0 };
            Ok(Some(q))
        }
        Err(DecodeError::NoDecoder(_)) => Ok(None),
        Err(DecodeError::Failed(e)) => Err(format!("the output does not decode: {e}")),
    }
}

/// Where `b` opens on this machine (the first adapter of its order).
fn target(b: Backend) -> EncoderTarget {
    let adapters = super::encoder_probe::adapters();
    match backend::backend_order(Os::Linux, &adapters, Some(b)).first() {
        Some(&candidate) => super::encoder_probe::target_for(candidate, &adapters),
        None => EncoderTarget::new(b),
    }
}

/// The backend the bench and the per-machine tests use: the forced one,
/// else the first of this machine's order.
fn machine_backend() -> Backend {
    backend::forced_backend().unwrap_or_else(|| {
        let adapters = super::encoder_probe::adapters();
        backend::backend_order(Os::Linux, &adapters, None).first().map_or(Backend::Software, |c| c.backend)
    })
}

fn codec_name(codec: CodecKind) -> &'static str {
    match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => "h264",
        CodecKind::H265 => "hevc",
        CodecKind::Av1 => "av1",
        CodecKind::Unknown => "?",
    }
}

fn env_list(name: &str) -> Option<Vec<String>> {
    let v = std::env::var(name).ok()?;
    let items: Vec<String> = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    (!items.is_empty()).then_some(items)
}

/// The quality table (see the module comment). The default rows are the
/// Go Live presets' sizes and bitrates: Data saver (720p30, 3 Mbps),
/// 1080p30 at 6 Mbps, Smooth (1080p60, 10 Mbps), Sharp on a 1440p screen
/// (30 fps, 12 Mbps), 1440p60 at 20 Mbps, and Slides (1440p, 5 fps,
/// 12 Mbps, `detail`).
#[test]
#[ignore]
fn quality_bench() {
    if std::env::var_os("DECIBELL_BENCH_FFLOG").is_some() {
        unsafe { ffmpeg_next::sys::av_log_set_level(ffmpeg_next::sys::AV_LOG_DEBUG) };
    }
    let b = machine_backend();
    let seconds: usize = std::env::var("DECIBELL_BENCH_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
    let paced = std::env::var_os("DECIBELL_BENCH_UNPACED").is_none();
    let scenes: Vec<Scene> = match env_list("DECIBELL_BENCH_SCENES") {
        Some(names) => names.iter().filter_map(|n| Scene::parse(n)).collect(),
        None => Scene::ALL.to_vec(),
    };
    let codecs: Vec<CodecKind> = backend::STREAM_CODECS
        .into_iter()
        .filter(|c| b.encoder_name(*c).is_some())
        .filter(|c| env_list("DECIBELL_BENCH_CODECS").is_none_or(|names| names.iter().any(|n| n == codec_name(*c))))
        .collect();
    let rows: Vec<(u32, u32, u32, u32)> = match env_list("DECIBELL_BENCH_ROWS") {
        Some(items) => items
            .iter()
            .filter_map(|item| {
                let (size, kbps) = item.split_once(':')?;
                let (size, fps) = size.split_once('@')?;
                let (w, h) = size.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?, fps.parse().ok()?, kbps.parse().ok()?))
            })
            .collect(),
        None => vec![
            (1280, 720, 30, 3000),
            (1920, 1080, 30, 6000),
            (1920, 1080, 60, 10000),
            (2560, 1440, 30, 12000),
            (2560, 1440, 60, 20000),
            (2560, 1440, 5, 12000),
        ],
    };
    let tag = std::env::var("DECIBELL_BENCH_TAG").unwrap_or_else(|_| "decoded".to_string());
    println!(
        "{} — {seconds} s a row{}; kbps: mean (quietest..busiest second); key / p99: largest keyframe and 99th-percentile frame, x the average frame; key s: largest keyframe in seconds of the target rate; call: ms the encode call takes, p50 / p99; lat: ms from the call to the packet; late: frames whose packet came after their own call; PSNR-Y / -C: luma / chroma dB; after key: PSNR-Y of the half second after a keyframe",
        backend::backend_label(b, 0),
        if paced { ", paced in real time" } else { ", unpaced" },
    );
    println!("scene   | size      fps  target | codec | kbps mean (min..max)      | key x  p99 x  key s | call p50/p99  | lat p50/p99   | late | PSNR-Y  PSNR-C  SSIM   worst  after key | colour");
    for &scene in &scenes {
        for &(w, h, fps, kbps) in &rows {
            for &codec in &codecs {
                let job = Job {
                    target: target(b),
                    codec,
                    scene,
                    width: w,
                    height: h,
                    fps,
                    kbps,
                    frames: (fps as usize * seconds).max(10),
                    detail: fps <= 5,
                    gop_secs: std::env::var("DECIBELL_BENCH_GOP").ok().and_then(|s| s.parse().ok()).unwrap_or(stream_gop_secs(fps <= 5)),
                    paced,
                    forced_at: Vec::new(),
                };
                let head = format!("{:<7} | {:<9} {fps:>3}  {kbps:>6} | {:<5}", scene.name(), format!("{w}x{h}"), codec_name(codec));
                let out = match encode(&job) {
                    Ok(o) => o,
                    Err(e) => {
                        println!("{head} | cannot run: {e}");
                        continue;
                    }
                };
                let r = rate(&out.packets, fps, kbps);
                // DECIBELL_BENCH_RATE_ONLY=1: no decode and comparison
                // (most of a 4K row's time), for timing runs.
                let compared = if std::env::var_os("DECIBELL_BENCH_RATE_ONLY").is_some() { Ok(None) } else { quality(&job, &out, 3, &tag) };
                let q = match compared {
                    Ok(Some(q)) => format!(
                        "{:>6.2}  {:>6.2}  {:.4} {:>6.2} {:>9.2}  | {}/{}/{}/{}",
                        q.psnr_y, q.psnr_c, q.ssim, q.worst_psnr_y, q.after_key_psnr_y, q.colour[0], q.colour[1], q.colour[2], q.colour[3]
                    ),
                    // Rate only: the fastest tenth of the calls instead,
                    // which a busy machine does not inflate.
                    Ok(None) => format!("not compared; call p10 {:.2} ms", percentile(&out.call_ms, 0.1)),
                    Err(e) => e,
                };
                println!(
                    "{head} | {:>6.0} ({:>6.0}..{:>6.0}) | {:>5.1}  {:>5.1}  {:>5.2} | {:>5.2}/{:<6.2} | {:>5.2}/{:<6.2} | {:>4} | {q}",
                    r.kbps,
                    r.window_min,
                    r.window_max,
                    r.key_ratio,
                    r.p99_ratio,
                    r.key_seconds,
                    percentile(&out.call_ms, 0.5),
                    percentile(&out.call_ms, 0.99),
                    percentile(&out.latency_ms, 0.5),
                    percentile(&out.latency_ms, 0.99),
                    out.late,
                );
            }
        }
    }
}

/// A 4×3 grid of flat colours: the corners of the RGB cube, greys, and
/// the levels a range mix-up moves most (black, near black, near white,
/// white).
const BARS: [[u8; 3]; 12] = [
    [0, 0, 0],
    [255, 255, 255],
    [128, 128, 128],
    [16, 16, 16],
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [235, 235, 235],
    [0, 255, 255],
    [255, 0, 255],
    [255, 255, 0],
    [255, 128, 0],
];

/// Encode the colour grid with `b` and decode it with FFmpeg's software
/// decoder: every patch must come back as the RGB that went in, when
/// converted with the matrix and range the bitstream declares (a receiver
/// has nothing else to go by), and the description must be complete:
/// matrix, primaries, transfer and range all present, so no decoder has
/// to guess. A BT.601 / BT.709 mix-up moves pure red or green by 20 to 40
/// levels; a studio / full range mix-up moves black to 16 or crushes it.
/// `Ok(None)`: the backend or the decoder is absent here.
fn colours_round_trip(b: Backend, codec: CodecKind) -> Result<Option<String>, String> {
    const W: u32 = 640;
    const H: u32 = 384;
    let config = EncoderConfig {
        width: W,
        height: H,
        fps: 30,
        bitrate_kbps: 4000,
        keyframe_interval_secs: 2,
        detail: false,
        source_size: (W, H),
    };
    let name = b.encoder_name(codec).unwrap_or("?");
    let (cell_w, cell_h) = (W as usize / 4, H as usize / 3);
    let mut seen = Vec::new();
    // Both byte orders a capture delivers.
    for is_bgra in [true, false] {
        let _one_at_a_time = encoder_lock();
        let mut encoder = match H264Encoder::open(&target(b), codec, &config) {
            Ok(e) => e,
            Err(e) => {
                println!("{name}: cannot run here: {e}");
                return Ok(None);
            }
        };
        let stride = W as usize * 4 + 16;
        let mut picture = vec![0u8; stride * H as usize];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let [r, g, bl] = BARS[(y / cell_h).min(2) * 4 + (x / cell_w).min(3)];
                let px = if is_bgra { [bl, g, r, 255] } else { [r, g, bl, 255] };
                picture[y * stride + x * 4..][..4].copy_from_slice(&px);
            }
        }
        let mut packets = Vec::new();
        const FRAMES: usize = 12;
        for _ in 0..FRAMES {
            let mut next = encoder.encode_bgra_frame(&picture, W, H, stride, is_bgra)?;
            while let Some(f) = next {
                packets.push(to_packet(f));
                next = encoder.receive_pending();
            }
        }
        packets.extend(encoder.flush().into_iter().map(to_packet));
        drop(encoder);
        let mut patches = [[0f64; 3]; 12];
        let mut colour = [0i32; 4];
        match decode_pictures(codec, framing(codec), &packets, |p| {
            if p.index != FRAMES - 1 {
                return;
            }
            colour = [p.matrix, p.primaries, p.transfer, p.range];
            let matrix = Matrix::of(p.matrix, p.h);
            for (i, patch) in patches.iter_mut().enumerate() {
                // The middle 16×16 of the cell (8×8 of chroma), away from
                // the edges where neighbours bleed in.
                let (cx, cy) = ((i % 4) * cell_w + cell_w / 2, (i / 4) * cell_h + cell_h / 2);
                let mean = |plane: usize, x0: usize, y0: usize, n: usize| -> f64 {
                    let mut sum = 0u32;
                    for y in y0..y0 + n {
                        for x in x0..x0 + n {
                            sum += p.planes[plane][y * p.strides[plane] + x] as u32;
                        }
                    }
                    sum as f64 / (n * n) as f64
                };
                let yuv = [mean(0, cx - 8, cy - 8, 16), mean(1, cx / 2 - 4, cy / 2 - 4, 8), mean(2, cx / 2 - 4, cy / 2 - 4, 8)];
                *patch = matrix.to_rgb(yuv, p.range == 2);
            }
        }) {
            Ok(n) if n == FRAMES => {}
            Ok(n) => return Err(format!("{name}: {FRAMES} frames encoded, {n} decoded")),
            Err(DecodeError::NoDecoder(d)) => {
                println!("{name}: colour check skipped (no {d} decoder in this FFmpeg build)");
                return Ok(None);
            }
            Err(DecodeError::Failed(e)) => return Err(format!("{name}: {e}")),
        }
        let [matrix, primaries, transfer, range] = colour;
        if !matches!(matrix, 1 | 5 | 6) || primaries != 1 || !matches!(transfer, 1 | 13) || range != 1 {
            return Err(format!(
                "{name}: incomplete colour description (matrix {matrix}, primaries {primaries}, transfer {transfer}, range {range}; 2 / 0 = unspecified): a decoder would have to guess"
            ));
        }
        let mut worst = 0.0f64;
        for (i, got) in patches.iter().enumerate() {
            for ch in 0..3 {
                let off = (got[ch] - BARS[i][ch] as f64).abs();
                worst = worst.max(off);
                // Studio-range 8-bit rounding alone moves a channel by up
                // to 2; lossy flat patches add a level.
                if off > 4.0 {
                    return Err(format!(
                        "{name} ({}): RGB {:?} came back as [{:.0}, {:.0}, {:.0}] under the declared matrix {matrix} / range {range}",
                        if is_bgra { "BGRA in" } else { "RGBA in" },
                        BARS[i],
                        got[0],
                        got[1],
                        got[2]
                    ));
                }
            }
        }
        seen.push(format!("{} worst channel error {worst:.1}", if is_bgra { "BGRA" } else { "RGBA" }));
        if is_bgra {
            seen.push(format!("declares matrix {matrix} primaries {primaries} transfer {transfer} range {range}"));
        }
    }
    Ok(Some(format!("{} {name}: {} — colours exact", backend::backend_label(b, 0), seen.join(", "))))
}

fn colours_or_skipped(b: Backend, codec: CodecKind) {
    match colours_round_trip(b, codec) {
        Ok(Some(line)) => println!("{line}"),
        Ok(None) => assert_ne!(backend::forced_backend(), Some(b), "{} was forced and cannot run", b.id()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn nvenc_h264_colours_round_trip() {
    colours_or_skipped(Backend::Nvenc, CodecKind::H264Hw);
}

#[test]
fn nvenc_hevc_colours_round_trip() {
    colours_or_skipped(Backend::Nvenc, CodecKind::H265);
}

#[test]
fn nvenc_av1_colours_round_trip() {
    colours_or_skipped(Backend::Nvenc, CodecKind::Av1);
}

#[test]
fn software_av1_colours_round_trip() {
    colours_or_skipped(Backend::Software, CodecKind::Av1);
}

/// The owner's colour check for a backend nobody here has:
/// `DECIBELL_ENCODER=vaapi cargo test --lib forced_backend_colours -- --ignored --nocapture`.
#[test]
#[ignore]
fn forced_backend_colours_round_trip() {
    let b = backend::forced_backend().unwrap_or_else(|| panic!("set {}=nvenc|vaapi|software for this test", backend::FORCE_ENV));
    let mut passed = 0;
    for codec in backend::STREAM_CODECS {
        if b.encoder_name(codec).is_none() {
            continue;
        }
        match colours_round_trip(b, codec) {
            Ok(Some(line)) => {
                println!("{line}");
                passed += 1;
            }
            Ok(None) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert!(passed > 0, "{}: no codec could be checked on this machine (reasons above)", b.id());
}

/// What a hardware stream promises, on five seconds of scrolling code at
/// 720p30 and 3 Mbps with a keyframe requested mid-GOP:
///  - no delay: every frame's packet comes back from its own encode call,
///    with that frame's timestamp (no reordering, nothing held back);
///  - the requested keyframe is the frame it was requested on;
///  - the bitrate is within 10% of the target, and no second of the
///    stream runs more than the rate-control buffer over it;
///  - no keyframe is larger than the buffer allows (it goes out as one
///    burst of datagrams);
///  - everything decodes, and the text is recognisably the source.
fn contract(b: Backend, codec: CodecKind) -> Result<Option<String>, String> {
    const FPS: u32 = 30;
    const KBPS: u32 = 3000;
    let frames = FPS as usize * 5;
    let job = Job {
        target: target(b),
        codec,
        scene: Scene::Text,
        width: 1280,
        height: 720,
        fps: FPS,
        kbps: KBPS,
        frames,
        detail: false,
        gop_secs: stream_gop_secs(false),
        paced: false,
        forced_at: vec![77],
    };
    let name = b.encoder_name(codec).unwrap_or("?");
    let out = match encode(&job) {
        Ok(out) => out,
        Err(e) => {
            println!("{name}: cannot run here: {e}");
            return Ok(None);
        }
    };
    let expect = Expect {
        codec,
        framing: framing(codec),
        frames,
        fps: FPS,
        bitrate_kbps: KBPS,
        forced_at: job.forced_at.clone(),
        forced_within: 0,
        max_overshoot: 0.10,
        min_share: 0.90,
    };
    check_packets(&out.packets, &expect).map_err(|e| format!("{name}: {e}"))?;
    if out.late > 0 {
        return Err(format!("{name}: {} of {frames} packets came back after their frame's encode call (the encoder holds frames)", out.late));
    }
    if let Some((i, p)) = out.packets.iter().enumerate().find(|(i, p)| p.pts != *i as i64) {
        return Err(format!("{name}: packet {i} carries pts {}", p.pts));
    }
    let r = rate(&out.packets, FPS, KBPS);
    let vbv_seconds = super::bitrate_preset::rate_buffer_bits(name, KBPS, FPS) as f64 / (KBPS as f64 * 1000.0);
    // A keyframe may use the whole buffer and what the frame itself was
    // due; a fifth on top is the encoder's own slack.
    let key_limit = (vbv_seconds + 1.0 / FPS as f64) * 1.2;
    if r.key_seconds > key_limit {
        return Err(format!("{name}: a keyframe of {:.2} s of the bitrate (limit {key_limit:.2})", r.key_seconds));
    }
    if r.window_max > KBPS as f64 * (1.0 + vbv_seconds) * 1.05 {
        return Err(format!("{name}: {:.0} kbps in the busiest second of a {KBPS} kbps stream", r.window_max));
    }
    let q = match quality(&job, &out, 5, "decoded").map_err(|e| format!("{name}: {e}"))? {
        Some(q) => {
            if q.decoded != frames {
                return Err(format!("{name}: {frames} frames encoded, {} decoded", q.decoded));
            }
            // Measured: 37 to 41 dB (single pass, a keyframe every 2 s:
            // 34 to 40). Well under that is a regression of the settings.
            if q.psnr_y < 36.0 || q.ssim < 0.985 {
                return Err(format!("{name}: PSNR-Y {:.1} dB, SSIM {:.3} against the source", q.psnr_y, q.ssim));
            }
            format!("PSNR-Y {:.1} dB (worst {:.1}), SSIM {:.3}", q.psnr_y, q.worst_psnr_y, q.ssim)
        }
        None => "decode check skipped (no software decoder in this FFmpeg build)".to_string(),
    };
    Ok(Some(format!(
        "{} {name} 1280x720@{FPS}: {:.0} kbps of {KBPS} ({:.0}..{:.0} over a second), largest keyframe {:.1}x the average frame ({:.2} s of the bitrate), no frame held, keyframe on request, {q} — OK",
        backend::backend_label(b, 0),
        r.kbps,
        r.window_min,
        r.window_max,
        r.key_ratio,
        r.key_seconds,
    )))
}

fn contract_or_skipped(b: Backend, codec: CodecKind) {
    match contract(b, codec) {
        Ok(Some(line)) => println!("{line}"),
        Ok(None) => assert_ne!(backend::forced_backend(), Some(b), "{} was forced and cannot run", b.id()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn nvenc_h264_keeps_the_stream_contract() {
    contract_or_skipped(Backend::Nvenc, CodecKind::H264Hw);
}

#[test]
fn nvenc_hevc_keeps_the_stream_contract() {
    contract_or_skipped(Backend::Nvenc, CodecKind::H265);
}

#[test]
fn nvenc_av1_keeps_the_stream_contract() {
    contract_or_skipped(Backend::Nvenc, CodecKind::Av1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(len: usize, key: bool, pts: i64) -> Packet {
        Packet { data: vec![0; len], key, pts, description: None }
    }

    #[test]
    fn rate_reads_windows_and_keyframes() {
        // 10 fps, 2 s: a 5000-byte keyframe then 1000-byte frames.
        let packets: Vec<Packet> = (0..20).map(|i| packet(if i == 0 { 5000 } else { 1000 }, i == 0, i)).collect();
        let r = rate(&packets, 10, 96);
        // 24000 bytes over 2 s.
        assert!((r.kbps - 96.0).abs() < 1e-9);
        // The first second holds the keyframe: 14000 bytes; any second
        // without it: 10000.
        assert!((r.window_max - 112.0).abs() < 1e-9);
        assert!((r.window_min - 80.0).abs() < 1e-9);
        assert!((r.key_ratio - 5000.0 / 1200.0).abs() < 1e-9);
        // 40000 bits of a 96000 bit/s stream.
        assert!((r.key_seconds - 40.0 / 96.0).abs() < 1e-9);
    }

    #[test]
    fn the_matrices_invert_and_differ() {
        for matrix in [Matrix::Bt601, Matrix::Bt709] {
            for full in [false, true] {
                for rgb in BARS {
                    let c = [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64];
                    let back = matrix.to_rgb(matrix.to_yuv(c, full), full);
                    for i in 0..3 {
                        assert!((back[i] - c[i]).abs() < 1e-6);
                    }
                }
            }
        }
        // Studio-range luma of pure red: 81 under BT.601, 63 under BT.709.
        assert_eq!(Matrix::Bt601.to_yuv([255.0, 0.0, 0.0], false)[0].round(), 81.0);
        assert_eq!(Matrix::Bt709.to_yuv([255.0, 0.0, 0.0], false)[0].round(), 63.0);
        // Black and white sit at 16 and 235, and at 0 and 255 in full range.
        assert_eq!(Matrix::Bt709.to_yuv([0.0; 3], false), [16.0, 128.0, 128.0]);
        assert_eq!(Matrix::Bt709.to_yuv([255.0; 3], false)[0].round(), 235.0);
        assert_eq!(Matrix::Bt709.to_yuv([255.0; 3], true)[0].round(), 255.0);
        // What a decoder assumes when the stream does not say.
        assert_eq!(Matrix::of(2, 1080), Matrix::Bt709);
        assert_eq!(Matrix::of(2, 480), Matrix::Bt601);
        assert_eq!(Matrix::of(5, 1080), Matrix::Bt601);
    }
}
