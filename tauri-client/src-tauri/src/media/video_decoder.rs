//! Software H.264 decoder using ffmpeg-next.
//! Used on Linux where WebKitGTK lacks WebCodecs VideoDecoder support.
//! Decodes AVCC-formatted H.264 frames to JPEG for the frontend.
//!
//! Uses ffmpeg's MJPEG encoder for JPEG output — SIMD-accelerated and already
//! linked, much faster than the pure-Rust image crate encoder.

/// Convert AVCC-formatted H.264 data (4-byte length-prefixed NALs) to Annex B
/// (start-code-prefixed NALs) which is what ffmpeg's h264 decoder expects.
fn avcc_to_annexb(avcc: &[u8]) -> Vec<u8> {
    let mut annexb = Vec::with_capacity(avcc.len() + 64);
    let mut i = 0;
    while i + 4 <= avcc.len() {
        let nal_len = u32::from_be_bytes([avcc[i], avcc[i + 1], avcc[i + 2], avcc[i + 3]]) as usize;
        i += 4;
        if i + nal_len > avcc.len() {
            break;
        }
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&avcc[i..i + nal_len]);
        i += nal_len;
    }
    annexb
}

pub struct SoftwareH264Decoder {
    decoder: ffmpeg_next::decoder::Video,
    jpeg_encoder: Option<ffmpeg_next::encoder::Video>,
    scaler: Option<ffmpeg_next::software::scaling::Context>,
    last_width: u32,
    last_height: u32,
    frame_count: u64,
}

impl SoftwareH264Decoder {
    pub fn new() -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {}", e))?;

        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264)
            .ok_or("H.264 decoder not found")?;

        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .decoder()
            .video()
            .map_err(|e| format!("H.264 decoder context: {}", e))?;

        // Multi-threaded decoding
        unsafe {
            let ctx = &mut *context.as_mut_ptr();
            ctx.thread_count = 2;
        }

        Ok(SoftwareH264Decoder {
            decoder: context,
            jpeg_encoder: None,
            scaler: None,
            last_width: 0,
            last_height: 0,
            frame_count: 0,
        })
    }

    /// Create or recreate the MJPEG encoder for the given dimensions.
    fn ensure_jpeg_encoder(&mut self, w: u32, h: u32) -> bool {
        if self.jpeg_encoder.is_some() && self.last_width == w && self.last_height == h {
            return true;
        }

        let codec = match ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::MJPEG) {
            Some(c) => c,
            None => {
                eprintln!("[video-decoder] MJPEG encoder not found");
                return false;
            }
        };

        let mut ctx = match ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[video-decoder] MJPEG encoder context: {}", e);
                return false;
            }
        };

        ctx.set_width(w);
        ctx.set_height(h);
        ctx.set_format(ffmpeg_next::format::Pixel::YUVJ420P);
        ctx.set_time_base(ffmpeg_next::Rational::new(1, 30));
        // Quality: 2-31, lower = better. 8 is good quality at reasonable size.
        ctx.set_quality(8);

        match ctx.open() {
            Ok(enc) => {
                self.jpeg_encoder = Some(enc);
                true
            }
            Err(e) => {
                eprintln!("[video-decoder] MJPEG encoder open: {}", e);
                false
            }
        }
    }

    /// Decode an AVCC-formatted H.264 frame and return JPEG bytes.
    /// Returns None if the frame couldn't be decoded.
    pub fn decode_to_jpeg(&mut self, avcc_data: &[u8]) -> Option<Vec<u8>> {
        let annexb = avcc_to_annexb(avcc_data);
        if annexb.is_empty() {
            return None;
        }

        let mut packet = ffmpeg_next::Packet::copy(&annexb);
        packet.set_pts(None);
        packet.set_dts(None);

        self.decoder.send_packet(&packet).ok()?;

        let mut decoded = ffmpeg_next::frame::Video::empty();
        if self.decoder.receive_frame(&mut decoded).is_err() {
            return None;
        }

        let w = decoded.width();
        let h = decoded.height();
        if w == 0 || h == 0 {
            return None;
        }

        self.frame_count += 1;

        // Scale to YUVJ420P (what MJPEG encoder expects)
        if self.scaler.is_none() || self.last_width != w || self.last_height != h {
            self.scaler = ffmpeg_next::software::scaling::Context::get(
                decoded.format(),
                w,
                h,
                ffmpeg_next::format::Pixel::YUVJ420P,
                w,
                h,
                ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
            )
            .ok();
        }

        let scaler = self.scaler.as_mut()?;
        let mut yuv_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUVJ420P, w, h);
        scaler.run(&decoded, &mut yuv_frame).ok()?;

        // Update dimensions after successful decode+scale
        self.last_width = w;
        self.last_height = h;

        // Encode to JPEG via MJPEG encoder
        if !self.ensure_jpeg_encoder(w, h) {
            return None;
        }
        let enc = self.jpeg_encoder.as_mut()?;

        yuv_frame.set_pts(Some(self.frame_count as i64));
        enc.send_frame(&yuv_frame).ok()?;

        let mut jpeg_pkt = ffmpeg_next::Packet::empty();
        if enc.receive_packet(&mut jpeg_pkt).is_err() {
            return None;
        }

        Some(jpeg_pkt.data().unwrap_or(&[]).to_vec())
    }
}
