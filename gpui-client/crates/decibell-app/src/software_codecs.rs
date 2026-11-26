//! "The software fallback exists in this binary": the `encoder-software` self-test and its unit
//! test. A machine with no working hardware encoder shares its screen with SVT-AV1 and one with
//! no working hardware decoder watches with dav1d / FFmpeg (CLAUDE.md, "Video codecs"); both
//! come from the FFmpeg this binary links, so this checks that FFmpeg end to end rather than
//! trusting the build: generated frames go through the core's software encoder exactly as a
//! stream sends them (Linux: `H264Encoder` on the software backend, BGRA converted by swscale;
//! Windows: `SoftwareEncoder`, the NV12 its D3D11 readback produces), a key frame is asked for
//! in the middle, and every packet is decoded back by the video crate's software decoder (the
//! hub's CPU backend, dav1d), each picture read back and matched to its frame by the barcode
//! drawn into it. `build.rs` refuses to build against an FFmpeg without the codecs; this proves
//! the linked binary (a target directory can hold an older FFmpeg inside ffmpeg-sys-next).

use decibell_harness::{SelfTest, TestCx};

pub fn selftest() -> SelfTest {
    SelfTest {
        name: "encoder-software",
        about: "the software fallback is in this binary: SVT-AV1 encodes, a requested key frame comes, dav1d decodes every frame back",
        run: |cx| Box::pin(run(cx)),
    }
}

async fn run(cx: TestCx) -> anyhow::Result<()> {
    // The encoder and the waits for decoded pictures block: off the main thread.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("selftest-encoder-software".into())
        .spawn(move || drop(done.send(roundtrip())))?;
    let started = std::time::Instant::now();
    loop {
        match result.try_recv() {
            Ok(report) => {
                let report = report.map_err(anyhow::Error::msg)?;
                for line in report.lines() {
                    cx.log(line);
                }
                return Ok(());
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                anyhow::bail!("the check's thread died")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        anyhow::ensure!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "no result within 60 s"
        );
        cx.sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The round trip; the report's lines on success, the first broken rule otherwise.
pub fn roundtrip() -> Result<String, String> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    return imp::roundtrip();
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    Ok("skipped: no native encoder on this OS".to_string())
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod imp {
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    use decibell_core::media::encoder_software;
    use decibell_video::{DecodePath, EncodedFrame, LumaRow, Packet, VideoHub, barcode};

    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 360;
    const FPS: u32 = 30;
    const KBPS: u32 = 1500;
    const FRAMES: u32 = 30;
    /// The frame the test asks a key frame for (the keyframe interval is 2 s: 60 frames).
    const FORCED: u32 = 15;
    /// The barcode band: the top eighth (`decibell_video::barcode` reads 16 cells across).
    const BAND: u32 = HEIGHT / 8;
    /// The wire's `VideoCodec` byte for AV1.
    const AV1: u8 = 4;

    /// The luma of frame `n` at (x, y): its number as 16 black / white cells across the band,
    /// moving diagonal stripes below it so every frame differs.
    fn luma(n: u32, x: u32, y: u32) -> u8 {
        if y < BAND {
            let cell = x * 16 / WIDTH;
            if (n >> (15 - cell)) & 1 == 1 { 235 } else { 16 }
        } else {
            (((x + y + n * 4) / 8) % 2 * 120 + 60) as u8
        }
    }

    /// One encoded frame, as the stream hands it on.
    struct Encoded {
        data: Vec<u8>,
        key: bool,
        pts: i64,
        description: Option<Vec<u8>>,
    }

    /// Linux: the stream's own encoder on the software backend, fed BGRA as PipeWire delivers
    /// it (swscale converts to I420 for SVT-AV1).
    #[cfg(target_os = "linux")]
    struct Encoder {
        inner: decibell_core::media::encoder_linux::H264Encoder,
        bgra: Vec<u8>,
    }

    #[cfg(target_os = "linux")]
    impl Encoder {
        fn open() -> Result<(Self, &'static str), String> {
            use decibell_core::media::caps::CodecKind;
            use decibell_core::media::encoder_backend::Backend;
            use decibell_core::media::encoder_linux::{EncoderConfig, EncoderTarget, H264Encoder};
            let config = EncoderConfig {
                width: WIDTH,
                height: HEIGHT,
                fps: FPS,
                bitrate_kbps: KBPS,
                keyframe_interval_secs: 2,
                detail: false,
                source_size: (WIDTH, HEIGHT),
            };
            let inner = H264Encoder::open(
                &EncoderTarget::new(Backend::Software),
                CodecKind::Av1,
                &config,
            )?;
            let name = inner.encoder_name();
            Ok((
                Self {
                    inner,
                    bgra: vec![0; (WIDTH * HEIGHT * 4) as usize],
                },
                name,
            ))
        }

        fn encode(
            &mut self,
            n: u32,
            force_keyframe: bool,
            out: &mut Vec<Encoded>,
        ) -> Result<(), String> {
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let v = luma(n, x, y);
                    // Grey (R = G = B) whose BT.709 studio-range luma is `v`.
                    let g = ((v as u32 - 16) * 255 / 219) as u8;
                    let at = ((y * WIDTH + x) * 4) as usize;
                    self.bgra[at..at + 4].copy_from_slice(&[g, g, g, 255]);
                }
            }
            if force_keyframe {
                self.inner.force_keyframe();
            }
            let first = self.inner.encode_bgra_frame(
                &self.bgra,
                WIDTH,
                HEIGHT,
                (WIDTH * 4) as usize,
                true,
            )?;
            for frame in first
                .into_iter()
                .chain(std::iter::from_fn(|| self.inner.receive_pending()))
            {
                out.push(Encoded {
                    data: frame.data,
                    key: frame.is_keyframe,
                    pts: frame.pts as i64,
                    description: frame.avcc_description,
                });
            }
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<Encoded>) -> Result<(), String> {
            for frame in self.inner.flush() {
                out.push(Encoded {
                    data: frame.data,
                    key: frame.is_keyframe,
                    pts: frame.pts as i64,
                    description: frame.avcc_description,
                });
            }
            Ok(())
        }
    }

    /// Windows: the software path's encoder, fed the NV12 picture its D3D11 readback produces.
    #[cfg(target_os = "windows")]
    struct Encoder {
        inner: encoder_software::SoftwareEncoder,
        y: Vec<u8>,
        uv: Vec<u8>,
    }

    #[cfg(target_os = "windows")]
    impl Encoder {
        fn open() -> Result<(Self, &'static str), String> {
            let inner =
                encoder_software::SoftwareEncoder::open(WIDTH, HEIGHT, FPS, KBPS, FPS * 2, false)?;
            let (w, h) = (WIDTH as usize, HEIGHT as usize);
            Ok((
                Self {
                    inner,
                    y: vec![0; w * h],
                    uv: vec![128; w * h / 2],
                },
                "libsvtav1",
            ))
        }

        fn encode(
            &mut self,
            n: u32,
            force_keyframe: bool,
            out: &mut Vec<Encoded>,
        ) -> Result<(), String> {
            let w = WIDTH as usize;
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    self.y[y as usize * w + x as usize] = luma(n, x, y);
                }
            }
            self.inner
                .send_nv12(&self.y, w, &self.uv, w, n as i64, force_keyframe)?;
            self.inner.for_each_packet(|data, key, pts| {
                out.push(Encoded {
                    data: data.to_vec(),
                    key,
                    pts,
                    description: None,
                })
            })
        }

        fn finish(&mut self, out: &mut Vec<Encoded>) -> Result<(), String> {
            self.inner.finish(|data, key, pts| {
                out.push(Encoded {
                    data: data.to_vec(),
                    key,
                    pts,
                    description: None,
                })
            })
        }
    }

    pub fn roundtrip() -> Result<String, String> {
        if !encoder_software::available() {
            return Err("libsvtav1 is not in this binary's FFmpeg: a machine without a working hardware \
                        encoder could not share its screen (rebuild FFmpeg: gpui-client/HANDOFF.md §2)"
                .into());
        }
        let hub = VideoHub::headless();
        let support = hub.capabilities();
        let missing: Vec<&str> = support
            .iter()
            .filter(|s| s.path != Some(DecodePath::Software))
            .map(|s| s.codec.label())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "no software decoder for {} in this binary's FFmpeg",
                missing.join(", ")
            ));
        }

        let started = Instant::now();
        let (mut encoder, name) = Encoder::open()?;
        let opened = started.elapsed();
        let stream = hub.stream("selftest-encoder-software");
        let _viewer = stream.viewer();
        let mut packets: Vec<(bool, i64, usize)> = Vec::new();
        let mut matched = 0u32;
        let mut pending: Vec<Encoded> = Vec::new();
        // Each packet is decoded as it would arrive; the picture it gives is read back.
        let mut deliver = |encoded: Encoded,
                           packets: &mut Vec<(bool, i64, usize)>|
         -> Result<(), String> {
            packets.push((encoded.key, encoded.pts, encoded.data.len()));
            let row: Receiver<LumaRow> = stream.capture_luma_row(BAND as f32 / 2.0 / HEIGHT as f32);
            hub.push(
                stream.key(),
                EncodedFrame {
                    codec: AV1,
                    keyframe: encoded.key,
                    timestamp: encoded.pts * 1_000_000 / FPS as i64,
                    data: Packet::from(encoded.data),
                    description: encoded.description,
                    discontinuity: false,
                },
            );
            let row = row.recv_timeout(Duration::from_secs(5)).map_err(|_| {
                format!(
                    "packet {} (pts {}) gave no picture within 5 s",
                    packets.len(),
                    encoded.pts
                )
            })?;
            if (row.width, row.height) != (WIDTH, HEIGHT) {
                return Err(format!(
                    "decoded {}x{}, sent {WIDTH}x{HEIGHT}",
                    row.width, row.height
                ));
            }
            let number = barcode(&row.bytes);
            if number as i64 != encoded.pts {
                return Err(format!(
                    "the picture of pts {} reads as frame {number}",
                    encoded.pts
                ));
            }
            matched += 1;
            Ok(())
        };
        for n in 0..FRAMES {
            encoder.encode(n, n == FORCED, &mut pending)?;
            for encoded in pending.drain(..) {
                deliver(encoded, &mut packets)?;
            }
        }
        encoder.finish(&mut pending)?;
        for encoded in pending.drain(..) {
            deliver(encoded, &mut packets)?;
        }
        let elapsed = started.elapsed();

        let stats = stream.stats();
        hub.remove(stream.key());
        if packets.len() != FRAMES as usize {
            return Err(format!("{} packets for {FRAMES} frames", packets.len()));
        }
        if !packets.windows(2).all(|p| p[1].1 > p[0].1) {
            return Err(format!(
                "timestamps out of order: {:?}",
                packets.iter().map(|p| p.1).collect::<Vec<_>>()
            ));
        }
        let keys: Vec<i64> = packets.iter().filter(|p| p.0).map(|p| p.1).collect();
        if keys != [0, FORCED as i64] {
            return Err(format!(
                "key frames at {keys:?}, expected [0, {FORCED}]: the requested key frame did not come \
                 (SVT-AV1 before 4.0 ignores key frame requests in low-delay mode)"
            ));
        }
        if stats.path != Some(DecodePath::Software)
            || stats.decoded != FRAMES as u64
            || stats.errors != 0
        {
            return Err(format!(
                "decode: path {:?}, {} of {FRAMES} decoded, {} errors",
                stats.path, stats.decoded, stats.errors
            ));
        }
        let bytes: usize = packets.iter().map(|p| p.2).sum();
        Ok(format!(
            "{name} {WIDTH}x{HEIGHT}@{FPS} {KBPS} kbps: opened in {:.0} ms, {FRAMES} packets, key frames at {keys:?} \
             (frame {FORCED} requested), {:.0} kbps\n\
             decoded in software ({}): {}/{FRAMES} pictures, each matched to its frame by its barcode, 0 errors; \
             {:.0} ms in all",
            opened.as_secs_f64() * 1e3,
            bytes as f64 * 8.0 * FPS as f64 / FRAMES as f64 / 1000.0,
            stats.path.map(DecodePath::label).unwrap_or("none"),
            matched,
            elapsed.as_secs_f64() * 1e3,
        ))
    }
}

#[cfg(test)]
mod tests {
    /// The software codecs are linked into the client, and they round-trip: what
    /// `--selftest encoder-software` checks, without a window.
    #[test]
    fn the_software_fallback_round_trips() {
        match super::roundtrip() {
            Ok(report) => println!("{report}"),
            Err(e) => panic!("{e}"),
        }
    }
}
