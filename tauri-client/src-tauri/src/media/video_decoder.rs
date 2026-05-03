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

/// Parse a hvcC config record (ISO/IEC 14496-15 §8.3.3) and emit its
/// VPS/SPS/PPS NALs in Annex B form. Used as the HEVC decoder's extradata
/// because installing the raw hvcC flips ffmpeg into MP4 length-prefix
/// mode while our bitstream is annex-B — they have to match. Returns
/// `None` if the record is malformed; the caller falls back to opening
/// the decoder without extradata (won't decode but won't crash either).
/// Halve an NV12 frame's dimensions with 2×2 area averaging. Used by the
/// Linux self-preview bridge to keep the IPC payload small — at 1440p
/// each NV12 frame is 5.5MB, and Tauri IPC on WebKitGTK runs at ~150–200
/// MB/s, so the JS render loop pinned at ~25fps just shipping bytes.
/// Halving once cuts the payload 4× (one less pixel per side, half the
/// rows) and lets the JS side hit 60fps. The bridge calls this in a
/// loop until the frame fits in a sensible preview ceiling (720p).
///
/// Source dimensions must be even (NV12 chroma subsampling guarantees
/// it for valid frames; we trim odd remainders).
pub fn halve_nv12(src: &Nv12Frame) -> Nv12Frame {
    let src_w = (src.width as usize) & !1;
    let src_h = (src.height as usize) & !1;
    let dst_w = src_w / 2;
    let dst_h = src_h / 2;

    // Y plane: 2×2 area average. Cheaper than swscale, no ffmpeg-frame
    // wrapping overhead, plenty good for a preview (no aliasing on text
    // or sharp edges the way pure decimation would).
    let mut y_plane = vec![0u8; dst_w * dst_h];
    for y in 0..dst_h {
        let src_y0 = y * 2 * src_w;
        let src_y1 = src_y0 + src_w;
        let dst_off = y * dst_w;
        for x in 0..dst_w {
            let sx = x * 2;
            let sum = src.y_plane[src_y0 + sx] as u16
                    + src.y_plane[src_y0 + sx + 1] as u16
                    + src.y_plane[src_y1 + sx] as u16
                    + src.y_plane[src_y1 + sx + 1] as u16;
            y_plane[dst_off + x] = (sum / 4) as u8;
        }
    }

    // UV plane: same idea but the source is already at half resolution
    // (NV12 chroma subsampling), so "halve" means 2×2-average across
    // the UV plane's own grid. Bytes are interleaved U,V,U,V,... per row.
    let src_uv_h = src_h / 2;
    let dst_uv_w = dst_w; // dst has dst_w/2 UV pairs × 2 bytes/pair = dst_w bytes/row
    let dst_uv_h = dst_h / 2;
    let mut uv_plane = vec![0u8; dst_uv_w * dst_uv_h];
    for y in 0..dst_uv_h {
        let src_y0 = y * 2 * src_w; // src UV row width = src_w bytes (src_w/2 pairs)
        let src_y1 = src_y0 + src_w;
        let dst_off = y * dst_uv_w;
        for x in 0..(dst_uv_w / 2) {
            let sx = x * 4; // skip 2 source UV pairs (4 bytes)
            let u = (src.uv_plane[src_y0 + sx] as u16
                  + src.uv_plane[src_y0 + sx + 2] as u16
                  + src.uv_plane[src_y1 + sx] as u16
                  + src.uv_plane[src_y1 + sx + 2] as u16) / 4;
            let v = (src.uv_plane[src_y0 + sx + 1] as u16
                  + src.uv_plane[src_y0 + sx + 3] as u16
                  + src.uv_plane[src_y1 + sx + 1] as u16
                  + src.uv_plane[src_y1 + sx + 3] as u16) / 4;
            uv_plane[dst_off + x * 2] = u as u8;
            uv_plane[dst_off + x * 2 + 1] = v as u8;
        }
    }
    let _ = src_uv_h; // used for arithmetic intuition above

    Nv12Frame {
        width: dst_w as u32,
        height: dst_h as u32,
        sequence: src.sequence,
        timestamp_us: src.timestamp_us,
        y_plane,
        uv_plane,
    }
}

pub fn hvcc_to_annexb_extradata(hvcc: &[u8]) -> Option<Vec<u8>> {
    // Fixed-size hvcC header: 22 bytes through `numOfArrays`.
    if hvcc.len() < 23 || hvcc[0] != 1 { return None; }
    let mut pos = 22;
    let num_arrays = hvcc[pos] as usize;
    pos += 1;
    let mut out = Vec::with_capacity(hvcc.len());
    for _ in 0..num_arrays {
        // Skip array header byte (array_completeness + reserved + NAL_unit_type)
        if pos + 3 > hvcc.len() { return None; }
        pos += 1;
        let num_nalus = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]) as usize;
        pos += 2;
        for _ in 0..num_nalus {
            if pos + 2 > hvcc.len() { return None; }
            let nalu_len = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]) as usize;
            pos += 2;
            if pos + nalu_len > hvcc.len() { return None; }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&hvcc[pos..pos + nalu_len]);
            pos += nalu_len;
        }
    }
    Some(out)
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

        // For AV1, `decoder::find(Id::AV1)` returns libdav1d on most builds,
        // which is software-only and silently ignores `hw_device_ctx`. Frames
        // come back as YUV420P SW frames and `av_hwframe_transfer_data` fails
        // with EINVAL. Prefer the dedicated CUVID decoder for AV1 (and HEVC,
        // H.264 for symmetry) so we get genuine NVDEC HW frames.
        let cuvid_name = match codec {
            CodecKind::Av1 => Some("av1_cuvid"),
            CodecKind::H265 => Some("hevc_cuvid"),
            CodecKind::H264Hw | CodecKind::H264Sw => Some("h264_cuvid"),
            CodecKind::Unknown => None,
        };
        let ff_codec = cuvid_name
            .and_then(ffmpeg_next::decoder::find_by_name)
            .or_else(|| ffmpeg_next::decoder::find(codec_id))
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
        // Trust the actual frame format rather than the declared backend:
        // libdav1d (and other software decoders) silently ignore an attached
        // `hw_device_ctx` and return SW frames anyway. Routing those through
        // `av_hwframe_transfer_data` returns EINVAL — fall through to the
        // SW path instead so the swscale step picks them up.
        let is_hw_frame = matches!(
            decoded.format(),
            ffmpeg_next::format::Pixel::CUDA | ffmpeg_next::format::Pixel::VAAPI,
        );
        let cpu_frame = if is_hw_frame {
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
