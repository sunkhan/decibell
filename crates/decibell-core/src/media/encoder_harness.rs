//! Encoder harness: a synthetic screen share (`encoder_checks::Screen`)
//! pushed through one backend by the stream's own path
//! (`H264Encoder::encode_bgra_frame`), and the output checked: one packet a
//! frame, keyframes that carry their parameter sets, a requested keyframe
//! honoured within a couple of frames, the bitrate within tolerance over
//! five seconds, increasing timestamps, and the whole stream decoded by
//! FFmpeg's software decoder with every frame back and close to the source.
//!
//! What runs where:
//!  - `nvenc_*` and `software_*` run in every `cargo test --lib` and skip
//!    (saying why) on a machine or FFmpeg build without that backend.
//!  - `forced_backend_streams_are_sound` is ignored by default. With
//!    `DECIBELL_ENCODER=<backend>` it is the one command that verifies a
//!    backend on hardware nobody here has:
//!    `DECIBELL_ENCODER=vaapi cargo test --lib forced_backend -- --ignored --nocapture`
//!    (see `crates/decibell-core/tools/verify-encoders.md`).
//!  - `software_measurements` (ignored) prints the table the software
//!    path's presets and limits were chosen from.
#![cfg(all(test, target_os = "linux"))]

use std::time::{Duration, Instant};

use super::caps::CodecKind;
use super::encoder_backend::{self as backend, Backend, Os};
use super::encoder_checks::{
    check_packets, decode, decode_pictures, psnr, reference_yuv, ssim, DecodeError, Expect, Framing, Matrix, Packet, Screen,
};
use super::encoder_linux::{EncodedFrame, EncoderConfig, EncoderTarget, H264Encoder};

struct Run {
    target: EncoderTarget,
    codec: CodecKind,
    width: u32,
    height: u32,
    /// The capture's size when it differs from the encode size (scaled).
    source: Option<(u32, u32)>,
    fps: u32,
    bitrate_kbps: u32,
    frames: usize,
    scroll_px: u32,
    forced_at: Vec<usize>,
    /// Feed frames at the stream's rate (measurements) instead of as fast
    /// as the encoder takes them (tests).
    paced: bool,
    detail: bool,
}

#[derive(Default)]
struct Output {
    encoder_name: &'static str,
    packets: Vec<Packet>,
    /// Per frame: the time `encode_bgra_frame` held the encode thread
    /// (conversion + submit), ms.
    call_ms: Vec<f64>,
    /// Per frame, paced runs only: from the call to the frame's packet, ms.
    latency_ms: Vec<f64>,
    /// CPU seconds the process spent, minus generating the frames.
    cpu_seconds: f64,
    wall_seconds: f64,
}

fn framing(codec: CodecKind) -> Framing {
    // The Linux encoder sends HEVC length-prefixed with an hvcC.
    if codec == CodecKind::H265 {
        Framing::LengthPrefixed
    } else {
        Framing::AnnexB
    }
}

fn cpu_time(clock: libc::clockid_t) -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

fn to_packet(f: EncodedFrame) -> Packet {
    Packet { data: f.data, key: f.is_keyframe, pts: f.pts as i64, description: f.avcc_description }
}

fn run(r: &Run) -> Result<Output, String> {
    let _one_at_a_time = super::encoder_checks::encoder_lock();
    let (src_w, src_h) = r.source.unwrap_or((r.width, r.height));
    let config = EncoderConfig {
        width: r.width,
        height: r.height,
        fps: r.fps,
        bitrate_kbps: r.bitrate_kbps,
        keyframe_interval_secs: if r.detail { super::bitrate_preset::DETAIL_KEYFRAME_SECS } else { 2 },
        detail: r.detail,
        source_size: (src_w, src_h),
    };
    let mut encoder = H264Encoder::open(&r.target, r.codec, &config)?;
    let screen = Screen::new(src_w, src_h, r.scroll_px);
    let mut picture = vec![0u8; screen.frame_bytes()];
    let mut out = Output { encoder_name: encoder.encoder_name(), ..Default::default() };
    let interval = Duration::from_secs_f64(1.0 / r.fps as f64);
    let (cpu0, wall0) = (cpu_time(libc::CLOCK_PROCESS_CPUTIME_ID), Instant::now());
    let mut render_cpu = 0.0;
    let mut sent_at: Vec<Instant> = Vec::with_capacity(r.frames);
    for n in 0..r.frames {
        let due = wall0 + interval * n as u32;
        let t = cpu_time(libc::CLOCK_THREAD_CPUTIME_ID);
        screen.render(n as u32, &mut picture);
        render_cpu += cpu_time(libc::CLOCK_THREAD_CPUTIME_ID) - t;
        if r.paced {
            // Drain while waiting for the frame's time, as the stream's
            // loop does on its next frame.
            while Instant::now() < due {
                while let Some(f) = encoder.receive_pending() {
                    out.latency_ms.push(sent_at[f.pts as usize].elapsed().as_secs_f64() * 1e3);
                    out.packets.push(to_packet(f));
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        if r.forced_at.contains(&n) {
            encoder.force_keyframe();
        }
        let t = Instant::now();
        sent_at.push(t);
        let mut next = encoder.encode_bgra_frame(&picture, src_w, src_h, screen.stride, true)?;
        out.call_ms.push(t.elapsed().as_secs_f64() * 1e3);
        while let Some(f) = next {
            if r.paced {
                out.latency_ms.push(sent_at[f.pts as usize].elapsed().as_secs_f64() * 1e3);
            }
            out.packets.push(to_packet(f));
            next = encoder.receive_pending();
        }
    }
    out.wall_seconds = wall0.elapsed().as_secs_f64();
    out.cpu_seconds = cpu_time(libc::CLOCK_PROCESS_CPUTIME_ID) - cpu0 - render_cpu;
    out.packets.extend(encoder.flush().into_iter().map(to_packet));
    Ok(out)
}

/// Decode the run's output and compare every `every`-th picture with the
/// source: (frames decoded, mean PSNR-Y, mean SSIM-Y, worst PSNR-Y).
/// `Ok(None)`: this FFmpeg build has no decoder for the codec.
fn quality(r: &Run, out: &Output, every: usize) -> Result<Option<(usize, f64, f64, f64)>, String> {
    // The reference is the source at the encode size: only unscaled runs
    // are compared.
    let compare = r.source.is_none();
    let screen = Screen::new(r.width, r.height, r.scroll_px);
    let mut picture = vec![0u8; screen.frame_bytes()];
    let (w, h) = (r.width as usize, r.height as usize);
    let (mut sum_psnr, mut sum_ssim, mut worst, mut compared) = (0.0, 0.0, f64::MAX, 0usize);
    let mut size_error = None;
    let decoded = decode_pictures(r.codec, framing(r.codec), &out.packets, |p| {
        let (i, y, stride) = (p.index, p.planes[0], p.strides[0]);
        if p.w != w || p.h != h {
            size_error = Some(format!("decoded {}x{}, encoded {}x{}", p.w, p.h, r.width, r.height));
            return;
        }
        if !compare || i % every != 0 {
            return;
        }
        screen.render(i as u32, &mut picture);
        // The source's luma under the matrix and range the stream itself
        // declares, which is what a viewer's decoder undoes. (This compared
        // against BT.709 luma whatever the stream said, and NVENC's BGRA
        // conversion is BT.601: every coloured pixel counted as an error.)
        let [reference, _, _] = reference_yuv(&picture, screen.stride, w, h, Matrix::of(p.matrix, p.h), p.range == 2);
        let mut got = vec![0u8; w * h];
        for row in 0..h {
            got[row * w..][..w].copy_from_slice(&y[row * stride..][..w]);
        }
        // DECIBELL_HARNESS_DUMP=<dir>: write each compared picture and its
        // source as PGM, to look at what the numbers mean.
        if let Ok(dir) = std::env::var("DECIBELL_HARNESS_DUMP") {
            for (name, plane) in [("source", &reference), ("decoded", &got)] {
                let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
                pgm.extend_from_slice(plane);
                let _ = std::fs::write(format!("{dir}/{}-{w}x{h}-{i:04}-{name}.pgm", out.encoder_name), pgm);
            }
        }
        let p = psnr(&reference, &got);
        sum_psnr += p;
        sum_ssim += ssim(&reference, &got, w, h);
        worst = worst.min(p);
        compared += 1;
    });
    if let Some(e) = size_error {
        return Err(e);
    }
    match decoded {
        Ok(n) if compared > 0 => Ok(Some((n, sum_psnr / compared as f64, sum_ssim / compared as f64, worst))),
        Ok(n) => Ok(Some((n, 0.0, 0.0, 0.0))),
        Err(DecodeError::NoDecoder(_)) => Ok(None),
        Err(DecodeError::Failed(e)) => Err(format!("the output does not decode: {e}")),
    }
}

/// Where `backend` opens on this machine (the first adapter of its order).
fn target(b: Backend) -> EncoderTarget {
    let adapters = super::encoder_probe::adapters();
    match backend::backend_order(Os::Linux, &adapters, Some(b)).first() {
        Some(&candidate) => super::encoder_probe::target_for(candidate, &adapters),
        None => EncoderTarget::new(b),
    }
}

fn label(b: Backend) -> String {
    let adapters = super::encoder_probe::adapters();
    let vendor = backend::backend_order(Os::Linux, &adapters, Some(b))
        .first()
        .and_then(|c| c.adapter)
        .map_or(0, |i| adapters[i].vendor_id);
    backend::backend_label(b, vendor)
}

/// Five seconds of 720p30 scrolling text at 3 Mbps with a keyframe
/// requested mid-GOP, through one backend and codec; every check. Returns
/// the report line, or why the stream is not sound. `Ok(None)`: the
/// backend cannot open here (the reason is printed).
fn sound(b: Backend, codec: CodecKind) -> Result<Option<String>, String> {
    const FPS: u32 = 30;
    const KBPS: u32 = 3000;
    let frames = FPS as usize * 5;
    let r = Run {
        target: target(b),
        codec,
        width: 1280,
        height: 720,
        source: None,
        fps: FPS,
        bitrate_kbps: KBPS,
        frames,
        scroll_px: 3,
        // Not on a GOP boundary (keyframes fall on 0, 60, 120).
        forced_at: vec![77],
        paced: false,
        detail: false,
    };
    let name = b.encoder_name(codec).unwrap_or("?");
    let out = match run(&r) {
        Ok(out) => out,
        Err(e) => {
            println!("{} {name}: cannot run here: {e}", label(b));
            return Ok(None);
        }
    };
    let expect = Expect {
        codec,
        framing: framing(codec),
        frames,
        fps: FPS,
        bitrate_kbps: KBPS,
        forced_at: r.forced_at.clone(),
        forced_within: 2,
        // Over 5 s a CBR encoder stays within a few percent above its
        // target (measured here: NVENC under it, SVT-AV1 under it).
        max_overshoot: 0.15,
        // Well under what any of them reaches on a screen that changes
        // every frame (measured: h264_nvenc 96%, av1_nvenc 96%, hevc_nvenc
        // 90%, SVT-AV1 75%; hevc_nvenc was at 56% in a single pass): this
        // only catches rate control that is not working at all. The NVENC
        // streams are held to 10% either way in encoder_bench.rs.
        min_share: 0.3,
    };
    let summary = check_packets(&out.packets, &expect).map_err(|e| format!("{name}: {e}"))?;
    let decoded = quality(&r, &out, 10).map_err(|e| format!("{name}: {e}"))?;
    let decode_text = match decoded {
        Some((n, mean, s, worst)) => {
            if n != frames {
                return Err(format!("{name}: {frames} frames encoded, {n} decoded"));
            }
            // 3 Mbps is tight for a page of text scrolling at 720p with a
            // keyframe every 2 s: NVENC averages 34 to 41 dB here, and a
            // keyframe squeezed into the 133 ms buffer dips to 18 to 21. A
            // corrupt or misplaced picture, or the wrong byte order, is
            // far below these.
            if mean < 22.0 || s < 0.80 || worst < 12.0 {
                return Err(format!(
                    "{name}: decoded pictures are PSNR-Y {mean:.1} dB (worst {worst:.1}), SSIM {s:.3} from their source"
                ));
            }
            format!("decoded {n}/{frames}, PSNR-Y {mean:.1} dB (worst {worst:.1}), SSIM {s:.3}")
        }
        None => "decode check skipped (no software decoder in this FFmpeg build)".to_string(),
    };
    Ok(Some(format!(
        "{} {} 1280x720@{FPS}: {} packets, {} keyframes, {:.0} kbps of {KBPS}, largest packet {:.1}x the average, {decode_text} — OK",
        label(b),
        out.encoder_name,
        summary.packets,
        summary.keyframes,
        summary.kbps,
        summary.peak_ratio,
    )))
}

/// A backend that is expected on this kind of machine but may be absent
/// (CI has no GPU; the Electron FFmpeg has no SVT-AV1): sound if it runs,
/// skipped with the reason if it cannot open. With DECIBELL_ENCODER naming
/// it, it must run.
fn sound_or_skipped(b: Backend, codec: CodecKind) {
    match sound(b, codec) {
        Ok(Some(line)) => println!("{line}"),
        Ok(None) => {
            assert_ne!(backend::forced_backend(), Some(b), "{} was forced and cannot run", b.id());
            println!("skipped: {} {:?} is not usable on this machine / FFmpeg build", b.id(), codec);
        }
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn nvenc_h264_stream_is_sound() {
    sound_or_skipped(Backend::Nvenc, CodecKind::H264Hw);
}

#[test]
fn nvenc_hevc_stream_is_sound() {
    sound_or_skipped(Backend::Nvenc, CodecKind::H265);
}

#[test]
fn nvenc_av1_stream_is_sound() {
    sound_or_skipped(Backend::Nvenc, CodecKind::Av1);
}

#[test]
fn software_av1_stream_is_sound() {
    sound_or_skipped(Backend::Software, CodecKind::Av1);
}

/// The owner's one command per backend. Fails unless at least one codec of
/// the forced backend passes every check, and prints a line per codec.
#[test]
#[ignore]
fn forced_backend_streams_are_sound() {
    let b = backend::forced_backend()
        .unwrap_or_else(|| panic!("set {}=nvenc|vaapi|software for this test", backend::FORCE_ENV));
    let mut passed = 0;
    let mut failures = Vec::new();
    for codec in backend::STREAM_CODECS {
        if b.encoder_name(codec).is_none() {
            continue;
        }
        match sound(b, codec) {
            Ok(Some(line)) => {
                println!("{line}");
                passed += 1;
            }
            // The GPU / driver has no encoder for this codec (printed above).
            Ok(None) => {}
            Err(e) => failures.push(e),
        }
    }
    assert!(failures.is_empty(), "{} FAILED: {}", b.id(), failures.join("; "));
    assert!(passed > 0, "{}: no codec could be opened on this machine (reasons above)", b.id());
    println!("{}: {passed} codec(s) verified", b.id());
}

/// The software path converts with BT.709 studio range (what the stream
/// declares): pure red and blue come back with BT.709 luma. swscale's
/// default matrix (BT.601) puts red at 81 and blue at 41.
#[test]
fn software_converts_with_bt709() {
    if !super::encoder_software::available() {
        println!("skipped: no libsvtav1 in this FFmpeg build");
        return;
    }
    const W: u32 = 320;
    const H: u32 = 192;
    let config = EncoderConfig {
        width: W,
        height: H,
        fps: 30,
        bitrate_kbps: 2000,
        keyframe_interval_secs: 2,
        detail: false,
        source_size: (W, H),
    };
    // Both byte orders: an RGBA capture is converted by the same scaler.
    for is_bgra in [true, false] {
        let _one_at_a_time = super::encoder_checks::encoder_lock();
        let mut encoder = H264Encoder::open(&EncoderTarget::new(Backend::Software), CodecKind::Av1, &config).expect("open");
        let stride = W as usize * 4 + 16;
        let mut picture = vec![0u8; stride * H as usize];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let red = x < W as usize / 2;
                let px: [u8; 4] = match (red, is_bgra) {
                    (true, true) => [0, 0, 255, 255],
                    (true, false) => [255, 0, 0, 255],
                    (false, true) => [255, 0, 0, 255],
                    (false, false) => [0, 0, 255, 255],
                };
                picture[y * stride + x * 4..][..4].copy_from_slice(&px);
            }
        }
        let mut packets = Vec::new();
        for _ in 0..8 {
            let mut next = encoder.encode_bgra_frame(&picture, W, H, stride, is_bgra).expect("encode");
            while let Some(f) = next {
                packets.push(to_packet(f));
                next = encoder.receive_pending();
            }
        }
        packets.extend(encoder.flush().into_iter().map(to_packet));
        assert_eq!(packets.len(), 8);
        let mut luma = (0u8, 0u8);
        match decode(CodecKind::Av1, Framing::AnnexB, &packets, |i, y, stride, _, _| {
            if i == 7 {
                let row = &y[(H as usize / 2) * stride..];
                luma = (row[W as usize / 4], row[3 * W as usize / 4]);
            }
        }) {
            Ok(n) => assert_eq!(n, 8),
            Err(DecodeError::NoDecoder(_)) => {
                println!("skipped: no AV1 software decoder in this FFmpeg build");
                return;
            }
            Err(DecodeError::Failed(e)) => panic!("{e}"),
        }
        // BT.709 studio range: red 16 + 0.2126 * 219 = 63, blue 16 + 0.0722 * 219 = 32.
        assert!((luma.0 as i32 - 63).abs() <= 4, "red luma {} (BT.709: 63, BT.601: 81)", luma.0);
        assert!((luma.1 as i32 - 32).abs() <= 4, "blue luma {} (BT.709: 32, BT.601: 41)", luma.1);
    }
}

/// A capture larger than the stream (a 1440p screen shared at 720p) and a
/// forced keyframe on the software path: scaled by swscale, still one
/// packet a frame and decodable at the encode size.
#[test]
fn software_scales_a_larger_capture() {
    if !super::encoder_software::available() {
        println!("skipped: no libsvtav1 in this FFmpeg build");
        return;
    }
    let r = Run {
        target: EncoderTarget::new(Backend::Software),
        codec: CodecKind::Av1,
        width: 1280,
        height: 720,
        source: Some((2560, 1440)),
        fps: 30,
        bitrate_kbps: 2500,
        frames: 45,
        scroll_px: 2,
        forced_at: vec![20],
        paced: false,
        detail: false,
    };
    let out = run(&r).expect("run");
    assert_eq!(out.packets.len(), 45);
    assert!(out.packets[20..23].iter().any(|p| p.key), "forced keyframe");
    match quality(&r, &out, 10) {
        Ok(Some((n, ..))) => assert_eq!(n, 45),
        Ok(None) => println!("decode check skipped"),
        Err(e) => panic!("{e}"),
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn p95(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() as f64 * 0.95) as usize).min(s.len() - 1)]
}

/// The measurements behind the software path's presets and limits
/// (`encoder_backend::software_preset` / `software_limits`): for each size
/// and rate, real-time-paced scrolling text through SVT-AV1 at the presets
/// in question.
///
/// `cargo test --release --lib software_measurements -- --ignored --nocapture`
/// (release: the frame generator and the comparison are Rust; the encoder
/// and swscale are optimised either way). `DECIBELL_BENCH_PRESETS=9,10,11`
/// tries those presets on every row instead of the chosen one,
/// `DECIBELL_SVT_PARAMS=rtc=1` other SVT-AV1 parameters,
/// `DECIBELL_BENCH_SECONDS` another length (default 6),
/// `DECIBELL_BENCH_SCROLL=0` a still page, `DECIBELL_BENCH_ONLY=1920x1080@30`
/// one row.
#[test]
#[ignore]
fn software_measurements() {
    if !super::encoder_software::available() {
        println!("skipped: no libsvtav1 in this FFmpeg build");
        return;
    }
    let seconds: usize = std::env::var("DECIBELL_BENCH_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
    let cpus = super::encoder_probe::logical_cpus();
    // "auto" = what `software_preset` picks for the row.
    let presets: Vec<String> = std::env::var("DECIBELL_BENCH_PRESETS")
        .unwrap_or_else(|_| "auto".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    println!("{cpus} logical CPUs; {seconds} s of scrolling text per row, paced in real time");
    println!("size        fps  kbps  preset | call ms mean/p95 | CPU ms/frame | CPU % of a core | kbps out | PSNR-Y  SSIM");
    // The presets' bitrates at each size (Smooth / Sharp ballpark).
    let only = std::env::var("DECIBELL_BENCH_ONLY").unwrap_or_default();
    for (w, h, fps, kbps) in [
        (1280u32, 720u32, 30u32, 2500u32),
        (1920, 1080, 30, 4000),
        (1920, 1080, 60, 6000),
        (2560, 1440, 30, 6000),
        (2560, 1440, 60, 9000),
    ] {
        // DECIBELL_BENCH_ONLY=1920x1080@30 runs one row.
        if !only.is_empty() && only != format!("{w}x{h}@{fps}") {
            continue;
        }
        for preset in &presets {
            let preset_used = match preset.as_str() {
                "auto" => {
                    std::env::remove_var("DECIBELL_SVT_PRESET");
                    backend::software_preset(w, h, fps, cpus, false).to_string()
                }
                p => {
                    std::env::set_var("DECIBELL_SVT_PRESET", p);
                    p.to_string()
                }
            };
            let r = Run {
                target: EncoderTarget::new(Backend::Software),
                codec: CodecKind::Av1,
                width: w,
                height: h,
                source: None,
                fps,
                bitrate_kbps: kbps,
                frames: fps as usize * seconds,
                // The same on-screen scroll speed at either rate
                // (DECIBELL_BENCH_SCROLL: pixels a second; 0 = a still page).
                scroll_px: std::env::var("DECIBELL_BENCH_SCROLL").ok().and_then(|s| s.parse().ok()).unwrap_or(180) / fps,
                forced_at: Vec::new(),
                paced: true,
                detail: false,
            };
            let out = match run(&r) {
                Ok(o) => o,
                Err(e) => {
                    println!("{w}x{h}@{fps}: {e}");
                    continue;
                }
            };
            let kbps_out = out.packets.iter().map(|p| p.data.len()).sum::<usize>() as f64 * 8.0 / 1000.0 / seconds as f64;
            let q = match quality(&r, &out, 15) {
                Ok(Some((_, psnr, ssim, _))) => format!("{psnr:5.1}  {ssim:.3}"),
                Ok(None) => "no decoder".to_string(),
                Err(e) => e,
            };
            println!(
                "{:<11} {fps:>3}  {kbps:>4}  {preset_used:>6} | {:>6.2} / {:<6.2} | {:>12.2} | {:>14.0}% | {kbps_out:>8.0} | {q}",
                format!("{w}x{h}"),
                mean(&out.call_ms),
                p95(&out.call_ms),
                out.cpu_seconds * 1000.0 / r.frames as f64,
                out.cpu_seconds / out.wall_seconds * 100.0,
            );
        }
    }
}


