//! Codec-aware video decoder for Linux where WebKitGTK lacks WebCodecs.
//!
//! Decodes H.264, HEVC, or AV1 frames to JPEG for the React canvas.
//! Tries GPU-accelerated decode first (NVDEC/CUDA for NVIDIA, VAAPI for
//! AMD/Intel), falling back to software. The chosen codec is fixed at
//! construction; if the streamer swaps codec mid-stream (Plan C), the
//! caller drops this decoder and builds a new one for the new codec.

use crate::media::caps::CodecKind;

/// Convert AVCC-formatted data (4-byte length-prefixed NALs) to Annex B
/// (start-code-prefixed NALs) which is what ffmpeg's H.264 / HEVC decoders
/// expect. AV1 is not NAL-framed (it uses OBUs) — passes through unchanged.
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum DecoderBackend {
    Vaapi,
    Cuda,
    Software,
}

impl std::fmt::Display for DecoderBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecoderBackend::Vaapi => write!(f, "VAAPI (GPU)"),
            DecoderBackend::Cuda => write!(f, "NVDEC/CUDA (GPU)"),
            DecoderBackend::Software => write!(f, "Software"),
        }
    }
}

/// Map a decibell CodecKind to ffmpeg's codec ID. Returns None for codecs
/// we don't decode (Unknown / H264Sw — H264Sw uses the same H.264 decoder
/// as H264Hw, so we collapse it).
fn codec_to_ffmpeg_id(codec: CodecKind) -> Option<ffmpeg_next::codec::Id> {
    match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => Some(ffmpeg_next::codec::Id::H264),
        CodecKind::H265 => Some(ffmpeg_next::codec::Id::HEVC),
        CodecKind::Av1 => Some(ffmpeg_next::codec::Id::AV1),
        CodecKind::Unknown => None,
    }
}

fn codec_label(codec: CodecKind) -> &'static str {
    match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => "H.264",
        CodecKind::H265 => "HEVC",
        CodecKind::Av1 => "AV1",
        CodecKind::Unknown => "?",
    }
}

pub struct VideoDecoder {
    codec: CodecKind,
    decoder: ffmpeg_next::decoder::Video,
    backend: DecoderBackend,
    hw_device_ref: *mut ffmpeg_next::sys::AVBufferRef,
    jpeg_encoder: Option<ffmpeg_next::encoder::Video>,
    scaler: Option<ffmpeg_next::software::scaling::Context>,
    last_width: u32,
    last_height: u32,
    frame_count: u64,
}

// Raw pointers are only used from a single thread (video recv thread)
unsafe impl Send for VideoDecoder {}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        if !self.hw_device_ref.is_null() {
            unsafe {
                ffmpeg_next::sys::av_buffer_unref(&mut self.hw_device_ref);
            }
        }
    }
}

impl VideoDecoder {
    /// Build a decoder for the given codec. Tries hardware backends in
    /// preference order (CUDA → VAAPI → software). Returns Err if no
    /// backend can open this codec at all.
    pub fn new(codec: CodecKind) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {}", e))?;
        let codec_id = codec_to_ffmpeg_id(codec)
            .ok_or_else(|| format!("Unsupported codec: {:?}", codec))?;

        if let Ok(dec) = Self::try_cuda(codec, codec_id) {
            return Ok(dec);
        }
        if let Ok(dec) = Self::try_vaapi(codec, codec_id) {
            return Ok(dec);
        }
        Self::try_software(codec, codec_id)
    }

    fn try_vaapi(codec: CodecKind, codec_id: ffmpeg_next::codec::Id) -> Result<Self, String> {
        use ffmpeg_next::sys::*;

        let render_node = if std::path::Path::new("/dev/dri/renderD128").exists() {
            "/dev/dri/renderD128"
        } else if std::path::Path::new("/dev/dri/renderD129").exists() {
            "/dev/dri/renderD129"
        } else {
            return Err("No DRI render node found".into());
        };

        let ff_codec = ffmpeg_next::decoder::find(codec_id)
            .ok_or_else(|| format!("{} decoder not found", codec_label(codec)))?;

        unsafe {
            let render_cstr = std::ffi::CString::new(render_node).unwrap();
            let mut hw_device_ref: *mut AVBufferRef = std::ptr::null_mut();
            let rc = av_hwdevice_ctx_create(
                &mut hw_device_ref,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                render_cstr.as_ptr(),
                std::ptr::null_mut(),
                0,
            );
            if rc < 0 || hw_device_ref.is_null() {
                return Err(format!("av_hwdevice_ctx_create(VAAPI) failed: {}", rc));
            }

            let mut ctx = ffmpeg_next::codec::Context::new_with_codec(ff_codec);
            let raw = ctx.as_mut_ptr();
            (*raw).hw_device_ctx = av_buffer_ref(hw_device_ref);

            let decoder = ctx
                .decoder()
                .video()
                .map_err(|e| {
                    av_buffer_unref(&mut hw_device_ref);
                    format!("VAAPI {} decoder open: {}", codec_label(codec), e)
                })?;

            eprintln!("[video-decoder] {} VAAPI GPU decode ready ({})",
                codec_label(codec), render_node);

            Ok(VideoDecoder {
                codec,
                decoder,
                backend: DecoderBackend::Vaapi,
                hw_device_ref,
                jpeg_encoder: None,
                scaler: None,
                last_width: 0,
                last_height: 0,
                frame_count: 0,
            })
        }
    }

    fn try_cuda(codec: CodecKind, codec_id: ffmpeg_next::codec::Id) -> Result<Self, String> {
        use ffmpeg_next::sys::*;

        let ff_codec = ffmpeg_next::decoder::find(codec_id)
            .ok_or_else(|| format!("{} decoder not found", codec_label(codec)))?;

        unsafe {
            let mut hw_device_ref: *mut AVBufferRef = std::ptr::null_mut();
            let rc = av_hwdevice_ctx_create(
                &mut hw_device_ref,
                AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            );
            if rc < 0 || hw_device_ref.is_null() {
                return Err(format!("av_hwdevice_ctx_create(CUDA) failed: {}", rc));
            }

            let mut ctx = ffmpeg_next::codec::Context::new_with_codec(ff_codec);
            let raw = ctx.as_mut_ptr();
            (*raw).hw_device_ctx = av_buffer_ref(hw_device_ref);

            let decoder = ctx
                .decoder()
                .video()
                .map_err(|e| {
                    av_buffer_unref(&mut hw_device_ref);
                    format!("CUDA {} decoder open: {}", codec_label(codec), e)
                })?;

            eprintln!("[video-decoder] {} NVDEC/CUDA GPU decode ready",
                codec_label(codec));

            Ok(VideoDecoder {
                codec,
                decoder,
                backend: DecoderBackend::Cuda,
                hw_device_ref,
                jpeg_encoder: None,
                scaler: None,
                last_width: 0,
                last_height: 0,
                frame_count: 0,
            })
        }
    }

    fn try_software(codec: CodecKind, codec_id: ffmpeg_next::codec::Id) -> Result<Self, String> {
        let ff_codec = ffmpeg_next::decoder::find(codec_id)
            .ok_or_else(|| format!("{} decoder not found", codec_label(codec)))?;

        let mut ctx = ffmpeg_next::codec::Context::new_with_codec(ff_codec);
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).thread_count = 2;
        }

        let decoder = ctx
            .decoder()
            .video()
            .map_err(|e| format!("Software {} decoder open: {}",
                codec_label(codec), e))?;

        eprintln!("[video-decoder] {} software decode (no GPU acceleration available)",
            codec_label(codec));

        Ok(VideoDecoder {
            codec,
            decoder,
            backend: DecoderBackend::Software,
            hw_device_ref: std::ptr::null_mut(),
            jpeg_encoder: None,
            scaler: None,
            last_width: 0,
            last_height: 0,
            frame_count: 0,
        })
    }

    pub fn codec(&self) -> CodecKind { self.codec }

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
        ctx.set_format(ffmpeg_next::format::Pixel::YUV420P);
        ctx.set_color_range(ffmpeg_next::color::Range::JPEG);
        ctx.set_time_base(ffmpeg_next::Rational::new(1, 30));
        // MJPEG qscale: 1 = near-lossless, 31 = worst. 8 produced visible
        // 8x8 block artifacts on remote streams; 2 is high-quality and modest.
        ctx.set_quality(2);

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

    /// Decode one frame and return JPEG bytes. The input format depends on
    /// the codec: H.264 / HEVC arrive AVCC-framed (length-prefixed NALs)
    /// from the encoder; AV1 arrives raw (OBU-framed). The decoder feeds
    /// ffmpeg in the format it wants.
    pub fn decode_to_jpeg(&mut self, frame_data: &[u8]) -> Option<Vec<u8>> {
        let bitstream: Vec<u8> = match self.codec {
            CodecKind::H264Hw | CodecKind::H264Sw | CodecKind::H265 => {
                let annexb = avcc_to_annexb(frame_data);
                if annexb.is_empty() { return None; }
                annexb
            }
            CodecKind::Av1 => {
                // AV1 is OBU-framed at the encoder side too — passthrough.
                frame_data.to_vec()
            }
            CodecKind::Unknown => return None,
        };

        let mut packet = ffmpeg_next::Packet::copy(&bitstream);
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

        // GPU decode produces frames in GPU memory — transfer to system RAM
        let cpu_frame = if self.backend != DecoderBackend::Software {
            let mut sw_frame = ffmpeg_next::frame::Video::empty();
            let rc = unsafe {
                ffmpeg_next::sys::av_hwframe_transfer_data(
                    sw_frame.as_mut_ptr(),
                    decoded.as_ptr(),
                    0,
                )
            };
            if rc < 0 {
                eprintln!("[video-decoder] av_hwframe_transfer_data failed: {}", rc);
                return None;
            }
            sw_frame
        } else {
            decoded
        };

        self.frame_count += 1;

        if self.frame_count == 1 {
            eprintln!(
                "[video-decoder] First {} frame decoded: {}x{} via {} backend",
                codec_label(self.codec), w, h, self.backend
            );
        }

        // Scale to YUV420P (full range) for MJPEG encoder
        let src_format = cpu_frame.format();
        let need_new_scaler = self.scaler.is_none()
            || self.last_width != w
            || self.last_height != h;

        if need_new_scaler {
            self.scaler = ffmpeg_next::software::scaling::Context::get(
                src_format, w, h,
                ffmpeg_next::format::Pixel::YUV420P, w, h,
                ffmpeg_next::software::scaling::Flags::BILINEAR,
            )
            .ok();
        }

        let scaler = self.scaler.as_mut()?;
        let mut yuv_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, w, h);
        scaler.run(&cpu_frame, &mut yuv_frame).ok()?;
        unsafe {
            (*yuv_frame.as_mut_ptr()).color_range = ffmpeg_next::sys::AVColorRange::AVCOL_RANGE_JPEG;
        }

        self.last_width = w;
        self.last_height = h;

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

/// Probe what video codecs the Rust ffmpeg decoder can actually open on
/// this machine. Walks (H.264, HEVC, AV1) and tries each — first GPU
/// backend that opens counts as a success; if all GPU paths fail we
/// fall back to checking software, which is universal for any codec
/// ffmpeg was built with.
///
/// Used on Linux (where the JS WebCodecs probe always returns nothing)
/// to populate the user's decode caps for the LCD picker. On Windows the
/// JS probe is authoritative because the webview decodes natively, so
/// this function isn't called there.
pub fn probe_decoders() -> Vec<crate::media::caps::CodecCap> {
    use crate::media::caps::CodecCap;
    if ffmpeg_next::init().is_err() {
        return Vec::new();
    }

    let candidates: &[(CodecKind, ffmpeg_next::codec::Id)] = &[
        (CodecKind::Av1,    ffmpeg_next::codec::Id::AV1),
        (CodecKind::H265,   ffmpeg_next::codec::Id::HEVC),
        (CodecKind::H264Hw, ffmpeg_next::codec::Id::H264),
    ];

    // Per-codec ceilings — match the JS probe (decoderProbe.ts) so the
    // LCD picker treats Rust-probed and JS-probed clients comparably.
    fn ceiling(_codec: CodecKind) -> (u32, u32, u32) { (3840, 2160, 60) }

    let mut out = Vec::new();
    for &(codec, codec_id) in candidates {
        // Try CUDA → VAAPI → software. The first one that opens means we
        // can decode this codec end-to-end.
        let cuda_ok = VideoDecoder::try_cuda(codec, codec_id).is_ok();
        let vaapi_ok = !cuda_ok && VideoDecoder::try_vaapi(codec, codec_id).is_ok();
        let sw_ok = !cuda_ok && !vaapi_ok && VideoDecoder::try_software(codec, codec_id).is_ok();
        if cuda_ok || vaapi_ok || sw_ok {
            let (w, h, fps) = ceiling(codec);
            out.push(CodecCap { codec, max_width: w, max_height: h, max_fps: fps });
            eprintln!("[video-decoder] probe: {} decode supported (cuda={}, vaapi={}, sw={})",
                codec_label(codec), cuda_ok, vaapi_ok, sw_ok);
        } else {
            eprintln!("[video-decoder] probe: {} decode NOT available (no backend opened)",
                codec_label(codec));
        }
    }

    // Spec §3.3 fallback: H.264 always advertised. ffmpeg always ships an
    // H.264 software decoder (libavcodec built-in), so this is a no-op in
    // practice — but matches the JS probe's guarantee so the LCD picker
    // never sits without a converging codec.
    if !out.iter().any(|c| c.codec == CodecKind::H264Hw) {
        let (w, h, fps) = ceiling(CodecKind::H264Hw);
        out.push(CodecCap { codec: CodecKind::H264Hw, max_width: w, max_height: h, max_fps: fps });
    }

    out
}
