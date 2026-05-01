//! Codec-aware video decoder for Linux where WebKitGTK lacks WebCodecs.
//!
//! Decodes H.264, HEVC, or AV1 frames to NV12 planes for upload into a
//! WebGL R8 (Y) + RG8 (UV) texture pair on the renderer side. Tries
//! GPU-accelerated decode first (NVDEC/CUDA for NVIDIA, VAAPI for
//! AMD/Intel), falling back to software (libdav1d for AV1, libavcodec
//! built-ins for H.264/HEVC). The chosen codec is fixed at construction;
//! if the streamer swaps codec mid-stream (Plan C), the caller drops
//! this decoder and builds a new one for the new codec.

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
    /// Cached swscale context for SW backends that deliver YUV420P (or
    /// any non-NV12 format). Reused across frames; rebuilt on resolution
    /// change. CUDA/VAAPI deliver NV12 directly so this stays None for
    /// the hot HW paths.
    nv12_scaler: Option<ffmpeg_next::software::scaling::Context>,
    last_width: u32,
    last_height: u32,
    frame_count: u64,
}

/// One decoded video frame in NV12 layout, planes packed tight (no strides).
/// Sized for direct upload into a WebGL R8 (Y) + RG8 (UV) texture pair on
/// the renderer side. The Linux pull-IPC path swaps these in and out at
/// requestAnimationFrame rate, replacing the old JPEG-over-base64 bridge.
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    /// Monotonically increasing per decoder instance — lets the renderer
    /// detect "is this the same frame I already drew?" without a memcmp.
    pub sequence: u64,
    /// Encoder-side timestamp (microseconds) propagated from the wire
    /// frame. Renderer doesn't use it today but exposing it lets the JS
    /// side do its own clock-sync / drift correction later.
    pub timestamp_us: i64,
    /// Y plane: `width * height` bytes, no padding.
    pub y_plane: Vec<u8>,
    /// Interleaved UV plane (NV12): `(width / 2) * (height / 2) * 2` bytes.
    /// Each row is `width` bytes (U,V,U,V,...) at half vertical resolution.
    pub uv_plane: Vec<u8>,
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

/// Copy a config record (hvcC / av1C / etc.) into FFmpeg-owned extradata
/// on the codec context. Must be called BEFORE the decoder is opened —
/// ffmpeg's open path parses extradata once to seed parameter sets and
/// ignores later changes. The buffer needs `AV_INPUT_BUFFER_PADDING_SIZE`
/// bytes of trailing zeroes so the bitstream reader can over-read safely.
unsafe fn install_extradata(raw_ctx: *mut ffmpeg_next::sys::AVCodecContext, blob: &[u8]) {
    use ffmpeg_next::sys::*;
    const AV_INPUT_BUFFER_PADDING_SIZE: usize = 64;
    let total = blob.len() + AV_INPUT_BUFFER_PADDING_SIZE;
    let buf = av_mallocz(total) as *mut u8;
    if buf.is_null() { return; }
    std::ptr::copy_nonoverlapping(blob.as_ptr(), buf, blob.len());
    (*raw_ctx).extradata = buf;
    (*raw_ctx).extradata_size = blob.len() as i32;
}

impl VideoDecoder {
    /// Build a decoder for the given codec. Tries hardware backends in
    /// preference order (CUDA → VAAPI → software). Returns Err if no
    /// backend can open this codec at all.
    ///
    /// `extradata` carries the codec-config record that the encoder put
    /// in its `extradata` buffer (because we set GLOBAL_HEADER for
    /// HEVC/AV1) — i.e. hvcC for HEVC, av1C for AV1. ffmpeg parses these
    /// natively when it opens the decoder, seeding VPS/SPS/PPS or the
    /// AV1 sequence header. Pass `None` for H.264 (parameter sets are
    /// inline in keyframes) and for the probe path.
    pub fn new(codec: CodecKind, extradata: Option<&[u8]>) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {}", e))?;
        let codec_id = codec_to_ffmpeg_id(codec)
            .ok_or_else(|| format!("Unsupported codec: {:?}", codec))?;

        if let Ok(dec) = Self::try_cuda(codec, codec_id, extradata) {
            return Ok(dec);
        }
        if let Ok(dec) = Self::try_vaapi(codec, codec_id, extradata) {
            return Ok(dec);
        }
        Self::try_software(codec, codec_id, extradata)
    }

    fn try_vaapi(codec: CodecKind, codec_id: ffmpeg_next::codec::Id, extradata: Option<&[u8]>) -> Result<Self, String> {
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
            if let Some(ed) = extradata { install_extradata(raw, ed); }

            let decoder = ctx
                .decoder()
                .video()
                .map_err(|e| {
                    av_buffer_unref(&mut hw_device_ref);
                    format!("VAAPI {} decoder open: {}", codec_label(codec), e)
                })?;

            eprintln!("[video-decoder] {} VAAPI GPU decode ready ({}), extradata={}B",
                codec_label(codec), render_node, extradata.map(|e| e.len()).unwrap_or(0));

            Ok(VideoDecoder {
                codec,
                decoder,
                backend: DecoderBackend::Vaapi,
                hw_device_ref,
                nv12_scaler: None,
                last_width: 0,
                last_height: 0,
                frame_count: 0,
            })
        }
    }

    fn try_cuda(codec: CodecKind, codec_id: ffmpeg_next::codec::Id, extradata: Option<&[u8]>) -> Result<Self, String> {
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
            if let Some(ed) = extradata { install_extradata(raw, ed); }

            let decoder = ctx
                .decoder()
                .video()
                .map_err(|e| {
                    av_buffer_unref(&mut hw_device_ref);
                    format!("CUDA {} decoder open: {}", codec_label(codec), e)
                })?;

            eprintln!("[video-decoder] {} NVDEC/CUDA GPU decode ready, extradata={}B",
                codec_label(codec), extradata.map(|e| e.len()).unwrap_or(0));

            Ok(VideoDecoder {
                codec,
                decoder,
                backend: DecoderBackend::Cuda,
                hw_device_ref,
                nv12_scaler: None,
                last_width: 0,
                last_height: 0,
                frame_count: 0,
            })
        }
    }

    fn try_software(codec: CodecKind, codec_id: ffmpeg_next::codec::Id, extradata: Option<&[u8]>) -> Result<Self, String> {
        let ff_codec = ffmpeg_next::decoder::find(codec_id)
            .ok_or_else(|| format!("Software {} decoder not found", codec_label(codec)))?;

        let mut ctx = ffmpeg_next::codec::Context::new_with_codec(ff_codec);
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).thread_count = 2;
            if let Some(ed) = extradata { install_extradata(raw, ed); }
        }

        let decoder = ctx
            .decoder()
            .video()
            .map_err(|e| format!("Software {} decoder open: {}",
                codec_label(codec), e))?;

        eprintln!("[video-decoder] {} software decode (no GPU acceleration available), extradata={}B",
            codec_label(codec), extradata.map(|e| e.len()).unwrap_or(0));

        Ok(VideoDecoder {
            codec,
            decoder,
            backend: DecoderBackend::Software,
            hw_device_ref: std::ptr::null_mut(),
            nv12_scaler: None,
            last_width: 0,
            last_height: 0,
            frame_count: 0,
        })
    }

    pub fn codec(&self) -> CodecKind { self.codec }

    /// Decode one frame and return tightly-packed NV12 planes, ready for
    /// upload into a WebGL R8 (Y) + RG8 (UV) texture pair.
    ///
    /// Hot path for the Linux watch pipeline (replaces the JPEG bridge):
    ///   bitstream → ffmpeg decode (HW or SW) → CPU NV12 → planes copied
    ///   tight (no swscale stride padding) → caller publishes to its
    ///   per-stream latest-frame slot.
    ///
    /// CUDA/VAAPI typically deliver NV12 directly via `av_hwframe_transfer_data`
    /// so the swscale step is bypassed. SW decoders deliver YUV420P (planar)
    /// and we use `nv12_scaler` to repack the U/V planes into NV12's
    /// interleaved UV plane — one swscale convert per frame, BT.709 colors
    /// preserved, no chroma resampling (both formats are 4:2:0).
    pub fn decode_to_nv12(&mut self, frame_data: &[u8]) -> Option<Nv12Frame> {
        let bitstream: Vec<u8> = match self.codec {
            CodecKind::H264Hw | CodecKind::H264Sw | CodecKind::H265 => {
                let annexb = avcc_to_annexb(frame_data);
                if annexb.is_empty() { return None; }
                annexb
            }
            CodecKind::Av1 => frame_data.to_vec(),
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

        // GPU decode → system memory. CUDA/VAAPI both deliver NV12 here.
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
                "[video-decoder] First {} NV12 frame: {}x{} via {} backend (fmt={:?})",
                codec_label(self.codec), w, h, self.backend, cpu_frame.format()
            );
        }

        // Fast path: HW decode landed in NV12 already — just copy planes.
        // Slow path: convert YUV420P (or whatever SW gave us) → NV12 with
        // swscale; cached scaler avoids per-frame realloc.
        let nv12_owned;
        let nv12_ref: &ffmpeg_next::frame::Video = if cpu_frame.format() == ffmpeg_next::format::Pixel::NV12 {
            &cpu_frame
        } else {
            let need_new_scaler = self.nv12_scaler.is_none()
                || self.last_width != w
                || self.last_height != h;
            if need_new_scaler {
                self.nv12_scaler = ffmpeg_next::software::scaling::Context::get(
                    cpu_frame.format(), w, h,
                    ffmpeg_next::format::Pixel::NV12, w, h,
                    ffmpeg_next::software::scaling::Flags::BILINEAR,
                ).ok();
            }
            let scaler = self.nv12_scaler.as_mut()?;
            let mut out = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::NV12, w, h);
            scaler.run(&cpu_frame, &mut out).ok()?;
            nv12_owned = out;
            &nv12_owned
        };

        self.last_width = w;
        self.last_height = h;

        // Copy planes tight (drop swscale's stride padding). One memcpy per
        // row — modern memcpy is SIMD'd and this is ~3MB at 1080p, sub-ms.
        let y_stride = nv12_ref.stride(0);
        let uv_stride = nv12_ref.stride(1);
        let y_data = nv12_ref.data(0);
        let uv_data = nv12_ref.data(1);

        let row_bytes_y = w as usize;
        let row_bytes_uv = w as usize; // interleaved UV: 2 bytes per chroma pair = `width` bytes/row
        let h_rows = h as usize;
        let uv_rows = (h / 2) as usize;

        // Bounds-check up front: ffmpeg occasionally pads strides aggressively
        // (alignment for SIMD). If the buffer is shorter than expected, bail.
        if y_data.len() < y_stride * h_rows || uv_data.len() < uv_stride * uv_rows {
            return None;
        }

        let mut y_plane = Vec::with_capacity(row_bytes_y * h_rows);
        for row in 0..h_rows {
            let start = row * y_stride;
            y_plane.extend_from_slice(&y_data[start..start + row_bytes_y]);
        }

        let mut uv_plane = Vec::with_capacity(row_bytes_uv * uv_rows);
        for row in 0..uv_rows {
            let start = row * uv_stride;
            uv_plane.extend_from_slice(&uv_data[start..start + row_bytes_uv]);
        }

        Some(Nv12Frame {
            width: w,
            height: h,
            sequence: self.frame_count,
            timestamp_us: 0,
            y_plane,
            uv_plane,
        })
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
    fn ceiling(_codec: CodecKind) -> (u32, u32, u32) { (3840, 2160, 120) }

    let mut out = Vec::new();
    for &(codec, codec_id) in candidates {
        // Try CUDA → VAAPI → software. The first one that opens means we
        // can decode this codec end-to-end. Probe never has extradata —
        // we just want to know the decoder *opens*.
        let cuda_ok = VideoDecoder::try_cuda(codec, codec_id, None).is_ok();
        let vaapi_ok = !cuda_ok && VideoDecoder::try_vaapi(codec, codec_id, None).is_ok();
        let sw_ok = !cuda_ok && !vaapi_ok && VideoDecoder::try_software(codec, codec_id, None).is_ok();
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
