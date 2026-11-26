//! The software encoder: SVT-AV1 through FFmpeg's `libsvtav1`, the last
//! resort of a machine with no usable hardware encoder (owner rule: no
//! software H.264; OpenH264 brings patent-licence strings).
//!
//! `software_context` is the one place the encoder is configured; the Linux
//! encoder (`encoder_linux.rs`, BGRA frames converted by swscale) and the
//! Windows one (`SoftwareEncoder` below, NV12 frames read back from the
//! GPU) both open it. Platform-neutral so its tests run on Linux, where the
//! harness (`encoder_harness.rs`) decodes what it produces.
//!
//! Absent from an FFmpeg build without `--enable-libsvtav1` (the Electron
//! addon's): `available()` is false, the probe reports no software backend,
//! and nothing else changes.
#![cfg(any(target_os = "linux", target_os = "windows"))]

use ffmpeg_next as ff;

use super::bitrate_preset::software_opts;
use super::encoder_backend::SOFTWARE_ENCODER;

/// Is SVT-AV1 in the linked FFmpeg?
pub fn available() -> bool {
    let _ = ff::init();
    ff::encoder::find_by_name(SOFTWARE_ENCODER).is_some()
}

/// The rate-control buffer handed to SVT-AV1 (libsvtav1.c turns
/// `rc_buffer_size` into `maximum_buffer_size_ms`): the stream's usual four
/// frames / 400 ms, but never under 100 ms. SVT-AV1's CBR needs more room
/// than a hardware encoder's to hold its rate on screen content.
pub fn software_vbv_bits(bitrate_kbps: u32, fps: u32) -> i32 {
    super::encode_util::vbv_bits(bitrate_kbps, fps).max((bitrate_kbps as u64 * 100).min(i32::MAX as u64) as i32)
}

/// Open libsvtav1 for an 8-bit I420 stream: real-time, low-delay, screen
/// content, CBR (`bitrate_preset::software_opts` has the reasons).
pub fn software_context(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    gop: u32,
    detail: bool,
) -> Result<ff::encoder::Video, String> {
    ff::init().map_err(|e| format!("FFmpeg init: {e}"))?;
    let codec = ff::encoder::find_by_name(SOFTWARE_ENCODER)
        .ok_or_else(|| format!("{SOFTWARE_ENCODER} is not in this FFmpeg build"))?;
    let fps = fps.max(1);
    let mut context = ff::codec::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| format!("Encoder context: {e}"))?;
    context.set_width(width);
    context.set_height(height);
    context.set_frame_rate(Some(ff::Rational::new(fps as i32, 1)));
    context.set_time_base(ff::Rational::new(1, fps as i32));
    // bit_rate == rc_max_rate is what makes libsvtav1.c pick CBR.
    context.set_bit_rate(bitrate_kbps as usize * 1000);
    context.set_max_bit_rate(bitrate_kbps as usize * 1000);
    context.set_gop(gop.max(2));
    context.set_max_b_frames(0);
    context.set_format(ff::format::Pixel::YUV420P);
    context.set_colorspace(ff::color::Space::BT709);
    context.set_color_range(ff::color::Range::MPEG);
    unsafe {
        (*context.as_mut_ptr()).rc_buffer_size = software_vbv_bits(bitrate_kbps, fps);
    }
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut opts = ff::Dictionary::new();
    for (k, v) in software_opts(width, height, fps, cpus, detail) {
        opts.set(k, &v);
    }
    context.open_with(opts).map_err(|e| format!("Open {SOFTWARE_ENCODER}: {e}"))
}

/// SVT-AV1 fed NV12 frames (the Windows path: the D3D11 video processor's
/// output, read back). One reused I420 input frame: no allocation per
/// frame beyond FFmpeg's own packet.
pub struct SoftwareEncoder {
    context: ff::encoder::Video,
    frame: ff::frame::Video,
    width: u32,
    height: u32,
}

// Owned and used by one encoder thread.
unsafe impl Send for SoftwareEncoder {}

impl SoftwareEncoder {
    pub fn open(width: u32, height: u32, fps: u32, bitrate_kbps: u32, gop: u32, detail: bool) -> Result<Self, String> {
        if width < 64 || height < 64 || width % 2 != 0 || height % 2 != 0 {
            return Err(format!("software encode needs an even size of at least 64x64, got {width}x{height}"));
        }
        let context = software_context(width, height, fps, bitrate_kbps, gop, detail)?;
        let frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, width, height);
        Ok(Self { context, frame, width, height })
    }

    /// Encode one NV12 picture of the encoder's size: `y` holds `height`
    /// rows of `y_stride` bytes, `uv` `height / 2` rows of interleaved
    /// U, V with `uv_stride`. The last row of each may stop at the picture's
    /// width.
    pub fn send_nv12(
        &mut self,
        y: &[u8],
        y_stride: usize,
        uv: &[u8],
        uv_stride: usize,
        pts: i64,
        force_keyframe: bool,
    ) -> Result<(), String> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, ch) = (w / 2, h / 2);
        if y_stride < w || uv_stride < w || y.len() < (h - 1) * y_stride + w || uv.len() < (ch - 1) * uv_stride + w {
            return Err(format!("short NV12 picture for {w}x{h} (strides {y_stride}, {uv_stride})"));
        }
        let dst_stride = self.frame.stride(0);
        let dst = self.frame.data_mut(0);
        for row in 0..h {
            dst[row * dst_stride..][..w].copy_from_slice(&y[row * y_stride..][..w]);
        }
        // Chroma: split the interleaved plane. Two passes keep each
        // destination borrow simple; at 1080p this is half a megabyte.
        for plane in 1..=2 {
            let dst_stride = self.frame.stride(plane);
            let dst = self.frame.data_mut(plane);
            for row in 0..ch {
                let src = &uv[row * uv_stride..][..w];
                let out = &mut dst[row * dst_stride..][..cw];
                for (o, pair) in out.iter_mut().zip(src.chunks_exact(2)) {
                    *o = pair[plane - 1];
                }
            }
        }
        self.frame.set_pts(Some(pts));
        // libsvtav1.c: pict_type I asks SVT-AV1 for a key frame (honoured
        // because force-key-frames is on).
        self.frame.set_kind(if force_keyframe { ff::picture::Type::I } else { ff::picture::Type::None });
        self.context.send_frame(&self.frame).map_err(|e| format!("send_frame: {e}"))
    }

    /// Drain the packets that are ready (SVT-AV1 encodes on its own
    /// threads: a frame's packet usually arrives on a later call).
    pub fn for_each_packet<F: FnMut(&[u8], bool, i64)>(&mut self, mut cb: F) -> Result<(), String> {
        loop {
            let mut packet = ff::Packet::empty();
            match self.context.receive_packet(&mut packet) {
                Ok(()) => cb(packet.data().unwrap_or(&[]), packet.is_key(), packet.pts().unwrap_or(0)),
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => break Ok(()),
                Err(ff::Error::Eof) => break Ok(()),
                Err(e) => break Err(format!("receive_packet: {e}")),
            }
        }
    }

    /// End the stream and hand out what is left (blocks until SVT-AV1 has
    /// finished its queued frames). The probe and the tests; a live stream
    /// that is stopping just drops the encoder.
    pub fn finish<F: FnMut(&[u8], bool, i64)>(&mut self, cb: F) -> Result<(), String> {
        self.context.send_eof().map_err(|e| format!("send_eof: {e}"))?;
        self.for_each_packet(cb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_never_drops_under_100_ms() {
        // 60 fps: four frames is 67 ms; 100 ms is kept.
        assert_eq!(software_vbv_bits(4000, 60), 400_000);
        // 30 fps: four frames (133 ms) already clears it.
        assert_eq!(software_vbv_bits(4000, 30), 533_333);
        // 5 fps: the stream's 400 ms cap.
        assert_eq!(software_vbv_bits(4000, 5), 1_600_000);
    }

    /// Runs only where the linked FFmpeg has libsvtav1 (the development
    /// prefix): NV12 in, AV1 out, a forced key frame honoured, and bad
    /// input refused before it is read.
    #[test]
    fn nv12_frames_become_av1_with_forced_keyframes() {
        #[cfg(test)]
        let _one_at_a_time = crate::media::encoder_checks::encoder_lock();
        if !available() {
            eprintln!("skipped: no libsvtav1 in this FFmpeg build");
            return;
        }
        const W: usize = 320;
        const H: usize = 180;
        let mut enc = SoftwareEncoder::open(W as u32, H as u32, 30, 800, 60, false).expect("open");
        // Strides wider than the picture, as a staging texture has.
        let (ys, uvs) = (W + 32, W + 32);
        let mut y = vec![0u8; ys * H];
        let mut uv = vec![128u8; uvs * (H / 2)];
        let mut packets: Vec<(bool, i64, usize)> = Vec::new();
        for i in 0..20i64 {
            for row in 0..H {
                for x in 0..W {
                    y[row * ys + x] = ((x as i64 + row as i64 + i * 3) & 0xFF) as u8;
                }
            }
            uv[0] = i as u8;
            enc.send_nv12(&y, ys, &uv, uvs, i, i == 10).expect("send");
            enc.for_each_packet(|d, k, pts| packets.push((k, pts, d.len()))).expect("drain");
        }
        enc.finish(|d, k, pts| packets.push((k, pts, d.len()))).expect("finish");
        assert_eq!(packets.len(), 20, "one packet per frame");
        assert!(packets.windows(2).all(|p| p[1].1 > p[0].1), "pts in order: {packets:?}");
        let keys: Vec<i64> = packets.iter().filter(|p| p.0).map(|p| p.1).collect();
        assert_eq!(keys, [0, 10], "the first frame and the forced one are key frames");
        assert!(packets.iter().all(|p| p.2 > 0));

        // A picture that is too short is an error, not a read past the end.
        let mut enc = SoftwareEncoder::open(W as u32, H as u32, 30, 800, 60, false).expect("open");
        assert!(enc.send_nv12(&y[..ys * (H - 1)], ys, &uv, uvs, 0, false).is_err());
        assert!(enc.send_nv12(&y, ys, &uv[..10], uvs, 0, false).is_err());
        assert!(SoftwareEncoder::open(321, 180, 30, 800, 60, false).is_err(), "odd width");
    }
}
