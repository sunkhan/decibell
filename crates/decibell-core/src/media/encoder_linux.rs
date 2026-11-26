//! Linux native FFmpeg video encoder: NVENC (BGRA in, converted on the
//! GPU), VAAPI for AMD / Intel (`encoder_vaapi.rs`), and software SVT-AV1
//! as the last resort. Input is always a CPU frame (PipeWire SHM). Which
//! backend opens is decided by `encoder_backend.rs` / `encoder_probe.rs`.
//! See docs/superpowers/specs/2026-05-15-linux-native-ffmpeg-encoder-
//! design.md and 2026-11-26-encoders-every-vendor-design.md.
//!
//! Windows uses `encoder.rs::Encoder` (D3D11VA).
#![cfg(target_os = "linux")]

use super::encoder_backend::Backend;
use super::encoder_vaapi::VaapiInput;

/// Parse H.264 Annex B bitstream into individual NAL units.
/// Returns a list of (nal_type, nal_data_without_start_code).
fn parse_annexb_nals(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut nals = Vec::new();
    let len = data.len();
    let mut i = 0;

    // Find all NAL unit boundaries
    let mut nal_starts: Vec<usize> = Vec::new();

    while i < len {
        if i + 3 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            nal_starts.push(i + 3);
            i += 3;
        } else if i + 4 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            nal_starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }

    for (idx, &start) in nal_starts.iter().enumerate() {
        let end = if idx + 1 < nal_starts.len() {
            // Find the start code before the next NAL
            let next = nal_starts[idx + 1];
            if next >= 4 && data[next - 4] == 0 && data[next - 3] == 0 && data[next - 2] == 0 && data[next - 1] == 1 {
                next - 4
            } else {
                next - 3
            }
        } else {
            len
        };

        if start < end {
            let nal_type = data[start] & 0x1F;
            nals.push((nal_type, &data[start..end]));
        }
    }

    nals
}

/// Parse HEVC Annex B bitstream into individual NAL units.
/// Differs from the H.264 version: HEVC NAL header is 2 bytes (vs 1) and
/// nal_unit_type lives in bits 1-6 of the first header byte (vs bits 0-4).
/// Returns (nal_type, nal_data_with_2byte_header).
fn parse_annexb_nals_hevc(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut nals = Vec::new();
    let len = data.len();
    let mut i = 0;
    let mut nal_starts: Vec<usize> = Vec::new();

    while i < len {
        if i + 3 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            nal_starts.push(i + 3);
            i += 3;
        } else if i + 4 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            nal_starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }

    for (idx, &start) in nal_starts.iter().enumerate() {
        let end = if idx + 1 < nal_starts.len() {
            let next = nal_starts[idx + 1];
            if next >= 4 && data[next - 4] == 0 && data[next - 3] == 0 && data[next - 2] == 0 && data[next - 1] == 1 {
                next - 4
            } else {
                next - 3
            }
        } else {
            len
        };
        if start < end && start + 2 <= end {
            let nal_type = (data[start] >> 1) & 0x3F;
            nals.push((nal_type, &data[start..end]));
        }
    }
    nals
}

/// Build a HEVCDecoderConfigurationRecord (hvcC, ISO/IEC 14496-15) from
/// a buffer of annex-B VPS+SPS+PPS NAL units. FFmpeg's hevc_nvenc puts
/// raw annex-B NALs in extradata even with AV_CODEC_FLAG_GLOBAL_HEADER
/// set (different convention than av1_nvenc which produces proper av1C),
/// so we have to translate.
///
/// HEVC NAL types: 32 = VPS, 33 = SPS, 34 = PPS.
/// Profile/tier/level read from the SPS profile_tier_level (12 fixed bytes
/// at byte offsets 3..15 of the SPS NAL — payload byte 1 onward, after
/// the 2-byte NAL header and the 1-byte sps_video_parameter_set_id field).
/// Strip H.26x emulation prevention bytes: any `00 00 03` sequence in
/// a NAL has the `03` inserted to prevent the byte stream from being
/// mistaken for a start code. Reverse that to get the raw RBSP so we
/// can read fixed-position fields (profile_tier_level, level_idc, etc.)
/// at the correct offsets.
fn unescape_rbsp(nal_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal_bytes.len());
    let mut zero_count = 0usize;
    for &b in nal_bytes {
        if zero_count >= 2 && b == 0x03 {
            // emulation prevention byte — drop it
            zero_count = 0;
        } else {
            out.push(b);
            zero_count = if b == 0 { zero_count + 1 } else { 0 };
        }
    }
    out
}

fn build_hevc_hvcc(annexb_extradata: &[u8]) -> Option<Vec<u8>> {
    let nals = parse_annexb_nals_hevc(annexb_extradata);
    let vps: Vec<&[u8]> = nals.iter().filter(|(t, _)| *t == 32).map(|(_, d)| *d).collect();
    let sps: Vec<&[u8]> = nals.iter().filter(|(t, _)| *t == 33).map(|(_, d)| *d).collect();
    let pps: Vec<&[u8]> = nals.iter().filter(|(t, _)| *t == 34).map(|(_, d)| *d).collect();

    if vps.is_empty() || sps.is_empty() || pps.is_empty() {
        log::warn!("[encoder] hvcC build failed — missing NAL types: vps={}, sps={}, pps={}",
                  vps.len(), sps.len(), pps.len());
        return None;
    }

    let sps0_raw = sps[0];
    // Read fixed-position SPS fields from the unescaped RBSP. Reading raw
    // NAL bytes directly causes wrong values when emulation prevention
    // bytes (00 00 03) sit within the profile_tier_level or earlier — e.g.
    // shifts level_idc by N bytes per emulation byte ahead of it.
    let sps0 = unescape_rbsp(sps0_raw);
    if sps0.len() < 15 {
        log::warn!("[encoder] hvcC build failed — SPS RBSP too short ({} bytes)", sps0.len());
        return None;
    }

    // Diagnostic (trace-level so it doesn't spam stderr every keyframe —
    // enable with RUST_LOG=trace if the hvcC ever looks wrong).
    log::trace!("[encoder] HEVC SPS NAL ({} bytes): {:02X?}", sps0_raw.len(), sps0_raw);
    log::trace!("[encoder] HEVC SPS RBSP ({} bytes): {:02X?}", sps0.len(), &sps0[..sps0.len().min(20)]);

    // sps[2] = sps_video_parameter_set_id<<4 | sps_max_sub_layers_minus1<<1 | sps_temporal_id_nesting_flag
    // sps[3..15] = profile_tier_level (12 bytes — see H.265 spec 7.3.3)
    let profile_byte = sps0[3];
    let profile_compat = &sps0[4..8];
    let constraint_flags = &sps0[8..14];
    let level_idc = sps0[14];
    let temporal_id_nesting = sps0[2] & 0x01;
    let max_sub_layers_minus1 = (sps0[2] >> 1) & 0x07;

    let mut hvcc = Vec::with_capacity(64 + annexb_extradata.len());
    hvcc.push(0x01);                              // configurationVersion
    hvcc.push(profile_byte);                      // profile_space + tier_flag + profile_idc
    hvcc.extend_from_slice(profile_compat);       // 4 bytes general_profile_compatibility_flags
    hvcc.extend_from_slice(constraint_flags);     // 6 bytes general_constraint_indicator_flags
    hvcc.push(level_idc);
    hvcc.extend_from_slice(&[0xF0, 0x00]);        // reserved(4)=1111, min_spatial_segmentation_idc=0
    hvcc.push(0xFC);                              // reserved(6)=111111, parallelismType=0
    hvcc.push(0xFD);                              // reserved(6)=111111, chroma_format_idc=1 (4:2:0)
    hvcc.push(0xF8);                              // reserved(5)=11111, bit_depth_luma_minus8=0
    hvcc.push(0xF8);                              // reserved(5)=11111, bit_depth_chroma_minus8=0
    hvcc.extend_from_slice(&[0x00, 0x00]);        // avgFrameRate=0
    // constantFrameRate(2)=0 | numTemporalLayers(3) | temporalIdNested(1) | lengthSizeMinusOne(2)=3
    let num_temporal_layers = (max_sub_layers_minus1 as u8 + 1) & 0x07;
    hvcc.push(((num_temporal_layers) << 3) | (temporal_id_nesting << 2) | 0x03);
    hvcc.push(0x03);                              // numOfArrays = 3 (VPS, SPS, PPS)

    for (nal_type, group) in [(32u8, &vps), (33u8, &sps), (34u8, &pps)] {
        // array_completeness=1 | reserved=0 | NAL_unit_type(6)
        hvcc.push(0x80 | (nal_type & 0x3F));
        hvcc.extend_from_slice(&(group.len() as u16).to_be_bytes());
        for nal in group {
            hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(nal);
        }
    }

    Some(hvcc)
}

/// Convert H.264 Annex B format to AVCC format (4-byte length-prefixed NAL units).
fn annexb_to_avcc(data: &[u8]) -> Vec<u8> {
    let nals = parse_annexb_nals(data);
    let mut result = Vec::with_capacity(data.len());

    for (_nal_type, nal_data) in &nals {
        let len = nal_data.len() as u32;
        result.extend_from_slice(&len.to_be_bytes());
        result.extend_from_slice(nal_data);
    }

    result
}

/// Magic 4-byte sentinel marking a length-prefixed hvcC/av1C blob ahead
/// of the bitstream on HEVC/AV1 keyframes. Picked so it can't collide
/// with a realistic 4-byte HEVC NAL length (would imply a NAL ~3.7 GB)
/// or an AV1 OBU header byte (forbidden bit forces byte 0 < 0x80; magic
/// byte 0 = 0xDE = 222). Receivers that find this magic strip the
/// prefix; receivers that don't pass the data through unchanged so
/// older clients still talk to newer ones (and vice versa).
pub const WIRE_DESCRIPTION_MAGIC: [u8; 4] = [0xDE, 0xC1, 0xBE, 0x11];

/// Build the wire bytes for a single encoded frame: for HEVC/AV1 keyframes
/// that carry a description (hvcC / av1C), prepend
/// `[MAGIC 4 bytes][u32 BE: desc_len][desc bytes]` so the receiver can
/// configure its WebCodecs decoder without parsing the bitstream itself.
/// H.264 doesn't use this: it travels as Annex B with SPS/PPS inline on
/// keyframes, which WebCodecs decodes without a description.
///
/// For self-preview the streamer's local React event already carries the
/// description in a separate field and uses `EncodedFrame.data` directly,
/// so this prepend only applies to the network path.
pub fn build_wire_data<'a>(
    encoded: &'a EncodedFrame,
    codec: crate::media::caps::CodecKind,
) -> std::borrow::Cow<'a, [u8]> {
    use crate::media::caps::CodecKind;
    if encoded.is_keyframe {
        if let Some(desc) = &encoded.avcc_description {
            if codec == CodecKind::H265 || codec == CodecKind::Av1 {
                let mut wire = Vec::with_capacity(
                    WIRE_DESCRIPTION_MAGIC.len() + 4 + desc.len() + encoded.data.len(),
                );
                wire.extend_from_slice(&WIRE_DESCRIPTION_MAGIC);
                wire.extend_from_slice(&(desc.len() as u32).to_be_bytes());
                wire.extend_from_slice(desc);
                wire.extend_from_slice(&encoded.data);
                return std::borrow::Cow::Owned(wire);
            }
        }
    }
    std::borrow::Cow::Borrowed(&encoded.data)
}

/// The SPS + PPS NAL units of an Annex B H.264 access unit, re-emitted as an
/// Annex B blob (4-byte start codes) that can be spliced in front of a later
/// keyframe. None unless both are present.
fn annexb_h264_parameter_sets(annexb_data: &[u8]) -> Option<Vec<u8>> {
    let nals = parse_annexb_nals(annexb_data);
    let sps = nals.iter().find(|(t, _)| *t == 7)?.1;
    let pps = nals.iter().find(|(t, _)| *t == 8)?.1;
    let mut out = Vec::with_capacity(8 + sps.len() + pps.len());
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(sps);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(pps);
    Some(out)
}

/// Persistent scaler context for pixel format conversion / scaling.
/// Reused across frames to avoid re-creating the FFmpeg SwsContext each time.
struct SwsScaler {
    ctx: ffmpeg_next::software::scaling::Context,
    src_frame: ffmpeg_next::frame::Video,
    src_w: u32,
    src_h: u32,
    src_fmt: ffmpeg_next::format::Pixel,
    dst_fmt: ffmpeg_next::format::Pixel,
}

impl SwsScaler {
    fn new(
        src_w: u32,
        src_h: u32,
        src_fmt: ffmpeg_next::format::Pixel,
        dst_w: u32,
        dst_h: u32,
        dst_fmt: ffmpeg_next::format::Pixel,
    ) -> Result<Self, String> {
        let ctx = ffmpeg_next::software::scaling::Context::get(
            src_fmt, src_w, src_h,
            dst_fmt, dst_w, dst_h,
            ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
        )
        .map_err(|e| format!("sws_getContext: {}", e))?;
        let src_frame = ffmpeg_next::frame::Video::new(src_fmt, src_w, src_h);
        Ok(Self { ctx, src_frame, src_w, src_h, src_fmt, dst_fmt })
    }

    fn matches(&self, src_w: u32, src_h: u32, src_fmt: ffmpeg_next::format::Pixel) -> bool {
        self.src_w == src_w && self.src_h == src_h && self.src_fmt == src_fmt
    }
}

/// swscale for the CPU-converted paths (software: BGRA → I420; VAAPI's CPU
/// tier: BGRA → NV12), converting and scaling in one pass on a few threads.
///
/// Measured on an i7-12700K (docs/superpowers/specs/2026-11-26-encoders-
/// every-vendor-design.md §6): single-threaded, this conversion costs 6 ms
/// a 1080p frame and 10 ms a 1440p one, more than SVT-AV1's fastest presets
/// spend encoding it; swscale's slice threading (the `threads` option, used
/// through `sws_scale_frame`) divides that by the thread count. Its worker
/// threads live as long as the context: nothing is allocated per frame.
struct YuvScaler {
    ctx: *mut ffmpeg_next::sys::SwsContext,
    /// The capture's pixels, copied in row by row (its stride is its own).
    src_frame: ffmpeg_next::frame::Video,
    src_w: u32,
    src_h: u32,
    src_fmt: ffmpeg_next::format::Pixel,
}

impl Drop for YuvScaler {
    fn drop(&mut self) {
        unsafe { ffmpeg_next::sys::sws_freeContext(self.ctx) };
    }
}

/// Threads swscale converts on: a quarter of the logical CPUs, 1 to 4.
/// (Past four the conversion is already under 2 ms at 1440p.)
fn sws_threads(logical_cpus: usize) -> i64 {
    (logical_cpus / 4).clamp(1, 4) as i64
}

impl YuvScaler {
    fn new(
        src_w: u32,
        src_h: u32,
        src_fmt: ffmpeg_next::format::Pixel,
        dst_w: u32,
        dst_h: u32,
        dst_fmt: ffmpeg_next::format::Pixel,
    ) -> Result<Self, String> {
        use ffmpeg_next::sys::*;
        use std::ffi::c_void;
        // libswscale/swscale.h: SWS_BILINEAR (1 << 1). ffmpeg-sys binds the
        // flags as a macro in 8.0 and as an enum in 8.1.
        const SWS_BILINEAR: i64 = 2;
        let threads = sws_threads(std::thread::available_parallelism().map_or(4, |n| n.get()));
        unsafe {
            let ctx = sws_alloc_context();
            if ctx.is_null() {
                return Err("sws_alloc_context failed".to_string());
            }
            let opts: [(&std::ffi::CStr, i64); 8] = [
                (c"srcw", src_w as i64),
                (c"srch", src_h as i64),
                (c"src_format", AVPixelFormat::from(src_fmt) as i64),
                (c"dstw", dst_w as i64),
                (c"dsth", dst_h as i64),
                (c"dst_format", AVPixelFormat::from(dst_fmt) as i64),
                // Bilinear: the same size converts without filtering, and a
                // downscale (a 1440p screen shared at 1080p) keeps text
                // readable without the cost of a wider filter.
                (c"sws_flags", SWS_BILINEAR),
                (c"threads", threads),
            ];
            for (name, value) in opts {
                let rc = av_opt_set_int(ctx as *mut c_void, name.as_ptr(), value, 0);
                if rc < 0 {
                    sws_freeContext(ctx);
                    return Err(format!("swscale option {name:?}: {}", ffmpeg_next::Error::from(rc)));
                }
            }
            let rc = sws_init_context(ctx, std::ptr::null_mut(), std::ptr::null_mut());
            if rc < 0 {
                sws_freeContext(ctx);
                return Err(format!("sws_init_context: {}", ffmpeg_next::Error::from(rc)));
            }
            // RGB -> YUV: swscale converts with BT.601 unless told
            // otherwise, and the stream is flagged BT.709 limited range (as
            // NVENC's own conversion produces). Full-range RGB in, BT.709
            // studio range out; swscale passes this on to its slice threads.
            let bt709 = sws_getCoefficients(SWS_CS_ITU709 as i32);
            sws_setColorspaceDetails(ctx, bt709, 1, bt709, 0, 0, 1 << 16, 1 << 16);
            let src_frame = ffmpeg_next::frame::Video::new(src_fmt, src_w, src_h);
            Ok(Self { ctx, src_frame, src_w, src_h, src_fmt })
        }
    }

    fn matches(&self, src_w: u32, src_h: u32, src_fmt: ffmpeg_next::format::Pixel) -> bool {
        self.src_w == src_w && self.src_h == src_h && self.src_fmt == src_fmt
    }

    /// Convert (and scale) one captured frame into `dst`. `data` was
    /// validated by the caller for `stride` and this scaler's source size.
    fn convert(&mut self, data: &[u8], stride: usize, dst: &mut ffmpeg_next::frame::Video) -> Result<(), String> {
        let src_stride = self.src_frame.stride(0);
        copy_rows(data, stride, self.src_frame.data_mut(0), src_stride, self.src_w as usize * 4, self.src_h as usize);
        let rc = unsafe { ffmpeg_next::sys::sws_scale_frame(self.ctx, dst.as_mut_ptr(), self.src_frame.as_ptr()) };
        if rc < 0 {
            return Err(format!("sws_scale_frame: {}", ffmpeg_next::Error::from(rc)));
        }
        Ok(())
    }
}

/// How a captured BGRA/RGBA CPU frame reaches the encoder's input frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputRoute {
    /// Rows copied as they are into the BGRA input frame.
    CopyBgra,
    /// Rows copied into the BGRA input frame with R and B swapped (an
    /// RGBA/RGBx capture), in the same pass as the copy.
    SwizzleToBgra,
    /// sws scales (and swaps RGBA to BGRA) into the BGRA input frame.
    ScaleToBgra,
    /// sws converts into the NV12 input frame (encoders opened with NV12).
    ConvertToNv12,
}

/// Pick the route for a frame. An encoder opened for BGRA input (NVENC)
/// only ever gets its BGRA frame; NV12 is for the encoders opened with it.
fn input_route(bgra_input: bool, is_bgra: bool, same_size: bool) -> InputRoute {
    match (bgra_input, same_size, is_bgra) {
        (false, _, _) => InputRoute::ConvertToNv12,
        (true, true, true) => InputRoute::CopyBgra,
        (true, true, false) => InputRoute::SwizzleToBgra,
        (true, false, _) => InputRoute::ScaleToBgra,
    }
}

/// Copy `rows` rows of `row_bytes` between buffers with their own strides.
/// Matching strides take one memcpy, which also covers a source whose last
/// row stops at `row_bytes`.
fn copy_rows(src: &[u8], src_stride: usize, dst: &mut [u8], dst_stride: usize, row_bytes: usize, rows: usize) {
    if rows == 0 {
        return;
    }
    if src_stride == dst_stride {
        let n = (rows - 1) * src_stride + row_bytes;
        dst[..n].copy_from_slice(&src[..n]);
        return;
    }
    for row in 0..rows {
        dst[row * dst_stride..][..row_bytes].copy_from_slice(&src[row * src_stride..][..row_bytes]);
    }
}

/// `copy_rows` for RGBA/RGBx → BGRA: each pixel's R and B bytes trade
/// places, the 4th byte (alpha or padding) is kept. One pass with the same
/// memory traffic as the plain copy it stands in for.
fn swizzle_rows_rgba_to_bgra(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    row_bytes: usize,
    rows: usize,
) {
    for row in 0..rows {
        let (s, _) = src[row * src_stride..][..row_bytes].as_chunks::<4>();
        let (d, _) = dst[row * dst_stride..][..row_bytes].as_chunks_mut::<4>();
        for (d, s) in d.iter_mut().zip(s) {
            *d = [s[2], s[1], s[0], s[3]];
        }
    }
}

/// Where an encoder runs: one backend, and for VAAPI the DRM render node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderTarget {
    pub backend: Backend,
    /// VAAPI: `/dev/dri/renderD*`; None = libva's default device.
    pub render_node: Option<String>,
}

impl EncoderTarget {
    pub fn new(backend: Backend) -> Self {
        Self { backend, render_node: None }
    }
}

/// Video encoder using FFmpeg's C API via ffmpeg-next: one backend (NVENC,
/// VAAPI, or software SVT-AV1) and one codec, both chosen at `open`. The
/// name predates the other codecs and is kept for diff hygiene.
pub struct H264Encoder {
    /// Which codec this encoder produces. Plumbs through to the
    /// per-packet UdpVideoPacket.codec byte (Plan B Task 7) and to
    /// codec-specific config / extradata handling (Plan B Tasks 4-5).
    pub codec: crate::media::caps::CodecKind,
    /// The backend it runs on, and FFmpeg's name for it.
    pub backend: Backend,
    encoder_name: &'static str,
    /// H.264 only: the last SPS+PPS seen on a keyframe (Annex B), spliced
    /// into keyframes that arrive without them (see with_h264_parameter_sets).
    h264_param_sets: Option<Vec<u8>>,
    /// HEVC only: the last hvcC built from a keyframe's inline parameter
    /// sets, reused for a keyframe that arrives without them.
    hevc_description: Option<Vec<u8>>,
    /// AV1: ship the encoder's extradata as the keyframe description (the
    /// NVENC path, as before). VAAPI and software AV1 carry the sequence
    /// header in band on every keyframe and send none, like the Windows
    /// encoders.
    av1_description: bool,
    encoder: ffmpeg_next::encoder::Video,
    frame_count: u64,
    keyframe_interval: u64,
    force_next_keyframe: bool,
    target_width: u32,
    target_height: u32,
    /// Persistent CPU-converted input frame, reused across encode calls:
    /// NV12 (VAAPI's CPU-convert tier), I420 for software. Unused by NVENC
    /// and by VAAPI's GPU convert.
    nv12_frame: ffmpeg_next::frame::Video,
    /// The pixel format of `nv12_frame`.
    sw_format: ffmpeg_next::format::Pixel,
    /// Persistent converter / scaler into `nv12_frame`.
    scaler: Option<YuvScaler>,
    /// Whether the encoder was opened with pix_fmt=BGRA (the NVENC family).
    /// When true, NVENC handles BGRA→NV12 conversion on the GPU, avoiding
    /// expensive CPU-based sws_scale color conversion — and only
    /// `bgra_frame` may be sent to it (see `input_route`).
    supports_bgra_input: bool,
    /// Persistent BGRA frame buffer for direct BGRA input path.
    bgra_frame: Option<ffmpeg_next::frame::Video>,
    /// Persistent scaler for BGRA→BGRA scaling (used when source resolution
    /// differs from encoder resolution on the NVENC BGRA path).
    bgra_scaler: Option<SwsScaler>,
    /// VAAPI: the device and the upload / conversion in front of the
    /// encoder. Declared after `encoder` so the codec context goes first.
    vaapi: Option<VaapiInput>,
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe_interval_secs: u32,
    /// A 5 fps stream: the encoder's quality preset (bitrate_preset::detail_opts).
    pub detail: bool,
    /// The capture's frame size when it is known before the first frame
    /// (0×0 otherwise): VAAPI sizes its upload surfaces for it up front
    /// instead of rebuilding its conversion on the first frame.
    pub source_size: (u32, u32),
}

#[derive(Debug)]
pub struct EncodedFrame {
    /// Encoded bitstream in the format the WebCodecs decoder expects:
    ///   - H.264         → Annex B, SPS/PPS inline on keyframes (no description)
    ///   - H.265         → length-prefixed NALU (HVCC) + hvcC description
    ///   - AV1           → OBU stream
    /// Codec is identified via the surrounding pipeline's CodecKind, NOT
    /// stored in this struct (it would duplicate H264Encoder.codec).
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts: u64,
    /// Codec decoder-configuration record, present on keyframes only:
    ///   - H.264 → none (SPS/PPS travel inline in the Annex B keyframe)
    ///   - H.265 → hvcC (built from the keyframe's inline VPS/SPS/PPS)
    ///   - AV1   → NVENC: FFmpeg encoder.extradata(); VAAPI / software: none
    ///     (the sequence header is in band on every keyframe)
    /// Field name kept stable for diff hygiene; semantics are codec-aware.
    /// Pass these bytes verbatim to WebCodecs VideoDecoder.configure({description}).
    pub avcc_description: Option<Vec<u8>>,
}

// The encoder holds raw FFmpeg pointers (VAAPI refs, SwsContext) that
// aren't auto-`Send`, but a `H264Encoder` is only ever constructed on one
// thread and then used solely on the dedicated encode thread it's moved
// into (see encoder_thread_linux) — never shared or touched concurrently.
// Same assertion the Windows `Encoder` makes.
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// Open the first backend of this machine's order that encodes
    /// `target_codec` (no probe cache: tests and tools; a stream start goes
    /// through `encoder_probe::start_plan`).
    pub fn new(target_codec: crate::media::caps::CodecKind, config: &EncoderConfig) -> Result<Self, String> {
        let adapters = super::encoder_probe::adapters();
        let order = super::encoder_backend::backend_order(
            super::encoder_backend::Os::Linux,
            &adapters,
            super::encoder_backend::forced_backend(),
        );
        let mut errors = Vec::new();
        for candidate in order {
            if candidate.backend.encoder_name(target_codec).is_none() {
                continue;
            }
            let target = super::encoder_probe::target_for(candidate, &adapters);
            match Self::open(&target, target_codec, config) {
                Ok(enc) => return Ok(enc),
                Err(e) => errors.push(format!("{}: {e}", candidate.backend.id())),
            }
        }
        Err(format!("No encoder available for {:?}. Tried: {}", target_codec, errors.join("; ")))
    }

    /// Open `target_codec` on one backend. Fails (with the reason) when the
    /// linked FFmpeg lacks the encoder, the driver refuses it, or the
    /// backend has no encoder for the codec.
    pub fn open(
        target: &EncoderTarget,
        target_codec: crate::media::caps::CodecKind,
        config: &EncoderConfig,
    ) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let backend = target.backend;
        let codec_name: &'static str = backend
            .encoder_name(target_codec)
            .ok_or_else(|| format!("{} has no encoder for {:?}", backend.id(), target_codec))?;
        if !matches!(backend, Backend::Nvenc | Backend::Vaapi | Backend::Software) {
            return Err(format!("{} is not a Linux backend", backend.id()));
        }
        let codec = ffmpeg_next::encoder::find_by_name(codec_name)
            .ok_or_else(|| format!("{codec_name} is not in this FFmpeg build"))?;
        let fps = config.fps.max(1);
        let gop = fps * config.keyframe_interval_secs.max(1);

        // NVENC (any codec) accepts BGRA directly — the GPU handles BGRA→NV12
        // conversion internally, eliminating expensive CPU-based sws_scale.
        let supports_bgra_input = backend == Backend::Nvenc;
        // VAAPI: the device, and the conversion that feeds it (GPU convert,
        // or the CPU tier). Opened first: the codec context needs its
        // frames context.
        let mut vaapi = if backend == Backend::Vaapi {
            let (src_w, src_h) = match config.source_size {
                (0, _) | (_, 0) => (config.width, config.height),
                size => size,
            };
            Some(VaapiInput::open(
                target.render_node.as_deref(),
                config.width,
                config.height,
                src_w,
                src_h,
                fps,
            )?)
        } else {
            None
        };
        let sw_format = match backend {
            // SVT-AV1's 8-bit input (libsvtav1.c: yuv420p / yuv420p10).
            Backend::Software => ffmpeg_next::format::Pixel::YUV420P,
            _ => ffmpeg_next::format::Pixel::NV12,
        };
        // AV1 needs AV_CODEC_FLAG_GLOBAL_HEADER set so FFmpeg populates
        // extradata with the config record build_encoded_frame ships to
        // WebCodecs as the description: NVENC only, as before. VAAPI
        // (vaapi_encode.c writes the sequence header with every IDR) and
        // SVT-AV1 (a sequence header OBU opens every key frame's temporal
        // unit) need no description, so none is sent, as on Windows.
        // H.264 and H.265 use a different path: parse SPS/PPS (and VPS for
        // HEVC) out of the keyframe's annex-B inline NALs and build the
        // avcC / hvcC manually. This required for HEVC because Chromium's
        // WebCodecs HEVC decoder rendered NVENC-with-GLOBAL_HEADER bitstreams
        // as just the top-left ~1/4 of the picture, regardless of slices/tune.
        // Inline VPS/SPS/PPS in keyframes (no GLOBAL_HEADER) produces a
        // bitstream the decoder consumes correctly.
        let av1_description = target_codec == crate::media::caps::CodecKind::Av1 && backend == Backend::Nvenc;

        // VAAPI rate control: CBR is what every preset means (as NVENC's
        // rc=cbr). A driver without it (vaapi_encode.c fails the open with
        // "Driver does not support CBR RC mode") gets VBR with the peak at
        // the target, then FFmpeg's own choice. One attempt for the others.
        let rc_ladder: &[Option<&str>] =
            if backend == Backend::Vaapi { &super::bitrate_preset::VAAPI_RC_LADDER } else { &[None] };
        let mut opened = None;
        let mut last_err = String::new();
        for &rc_mode in rc_ladder {
            if backend == Backend::Software {
                // One configuration for both platforms: encoder_software.rs.
                match super::encoder_software::software_context(
                    config.width,
                    config.height,
                    fps,
                    config.bitrate_kbps,
                    gop,
                    config.detail,
                ) {
                    Ok(encoder) => opened = Some((encoder, None)),
                    Err(e) => last_err = e,
                }
                break;
            }
            let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
                .encoder()
                .video()
                .map_err(|e| format!("Encoder context: {}", e))?;

            context.set_width(config.width);
            context.set_height(config.height);
            context.set_frame_rate(Some(ffmpeg_next::Rational::new(fps as i32, 1)));
            context.set_time_base(ffmpeg_next::Rational::new(1, fps as i32));
            context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
            context.set_max_bit_rate((config.bitrate_kbps as usize) * 1000);
            context.set_gop(gop);

            if av1_description {
                const AV_CODEC_FLAG_GLOBAL_HEADER: i32 = 1 << 22;
                unsafe {
                    let p = context.as_mut_ptr();
                    (*p).flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
                }
            }

            // Disable B-frames for real-time streaming — B-frames require reordering
            // which adds latency and can cause artifacts with simple decoders.
            context.set_max_b_frames(0);

            if supports_bgra_input {
                context.set_format(ffmpeg_next::format::Pixel::BGRA);
            } else if let Some(v) = &vaapi {
                context.set_format(ffmpeg_next::format::Pixel::VAAPI);
                unsafe {
                    let p = context.as_mut_ptr();
                    (*p).hw_device_ctx = ffmpeg_next::sys::av_buffer_ref(v.device());
                    (*p).hw_frames_ctx = ffmpeg_next::sys::av_buffer_ref(v.encoder_frames());
                }
            } else {
                context.set_format(sw_format);
            }

            // The colour description, complete: matrix, range, primaries
            // and transfer. For NVENC the matrix is not ours to choose:
            // with BGRA input the driver converts with BT.601, and FFmpeg
            // writes BT.470BG into the VUI / AV1 colour config whatever is
            // asked for here (nvenc.c, nvenc_setup_h264_config and its HEVC
            // / AV1 twins). A decoder may honour a description only when
            // all of it is there (Chromium's VideoColorSpace::IsSpecified
            // wants matrix, primaries and transfer) and otherwise assume
            // BT.709 for HD; a BT.601 picture taken for BT.709 shows pure
            // green a sixth too dark and red tinted orange. So primaries
            // and transfer are set too (BT.709's: an sRGB screen, what
            // every HD decoder assumes), and a test decodes a colour grid
            // through each backend (encoder_bench.rs, colours_round_trip).
            // VAAPI and software convert with BT.709 and say so.
            context.set_colorspace(ffmpeg_next::color::Space::BT709);
            context.set_color_range(ffmpeg_next::color::Range::MPEG);
            unsafe {
                let p = context.as_mut_ptr();
                (*p).color_primaries = ffmpeg_next::sys::AVColorPrimaries::AVCOL_PRI_BT709;
                (*p).color_trc = ffmpeg_next::sys::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
            }

            let mut opts = ffmpeg_next::Dictionary::new();
            match backend {
                // ── NVENC (H.264 / HEVC / AV1): bitrate_preset::nvenc_linux_opts ──
                Backend::Nvenc => {
                    for (k, v) in super::bitrate_preset::nvenc_linux_opts(codec_name, config.width, config.height, fps) {
                        opts.set(k, v);
                    }
                    // The rate-control buffer (bitrate_preset::rate_buffer_bits).
                    let vbv_bits = super::bitrate_preset::rate_buffer_bits(codec_name, config.bitrate_kbps, fps);
                    unsafe { (*context.as_mut_ptr()).rc_buffer_size = vbv_bits; }
                }
                // ── VAAPI (AMD radeonsi, Intel iHD): bitrate_preset::vaapi_opts ──
                Backend::Vaapi => {
                    for (k, v) in super::bitrate_preset::vaapi_opts() {
                        opts.set(k, v);
                    }
                    if let Some(mode) = rc_mode {
                        opts.set("rc_mode", mode);
                    }
                    // The HRD buffer (VAEncMiscParameterHRD): four frames,
                    // at most 400 ms.
                    let vbv_bits = super::encode_util::vbv_bits(config.bitrate_kbps, fps);
                    unsafe { (*context.as_mut_ptr()).rc_buffer_size = vbv_bits; }
                }
                // Software was opened above; AMF / Quick Sync are Windows'.
                Backend::Software | Backend::Amf | Backend::Qsv => {}
            }
            if config.detail {
                for (k, v) in super::bitrate_preset::detail_opts(codec_name) {
                    opts.set(k, v);
                }
            }
            // DECIBELL_NVENC_OPTS=key=value:… laid over the options above,
            // for measuring a setting without a rebuild (the quality bench
            // in encoder_bench.rs). `vbv_ms=<n>` sets the VBV buffer.
            if backend == Backend::Nvenc {
                for (k, v) in super::bitrate_preset::env_overrides("DECIBELL_NVENC_OPTS") {
                    if k == "vbv_ms" {
                        if let Ok(ms) = v.parse::<u64>() {
                            let bits = (config.bitrate_kbps as u64 * ms).min(i32::MAX as u64) as i32;
                            unsafe { (*context.as_mut_ptr()).rc_buffer_size = bits; }
                        }
                    } else {
                        opts.set(&k, &v);
                    }
                }
            }

            match context.open_with(opts) {
                Ok(encoder) => {
                    opened = Some((encoder, rc_mode));
                    break;
                }
                Err(e) => {
                    last_err = match rc_mode {
                        Some(mode) => format!("Open encoder (rc_mode {mode}): {e}"),
                        None => format!("Open encoder: {e}"),
                    };
                    if rc_ladder.len() > 1 {
                        log::info!("[encoder] {codec_name}: {last_err}");
                    }
                }
            }
        }
        let Some((encoder, rc_mode)) = opened else {
            return Err(last_err);
        };

        let input_fmt = match (&vaapi, backend) {
            (Some(v), _) => format!(
                "VAAPI ({}; rate control {})",
                v.tier_name(),
                rc_mode.unwrap_or("chosen by FFmpeg")
            ),
            (None, Backend::Nvenc) => "BGRA (GPU convert)".to_string(),
            _ => "I420 (CPU convert)".to_string(),
        };
        log::info!("[encoder] {:?} encoder opened: {} — {}x{} @ {}fps, {}kbps, input={}",
                  target_codec, codec_name, config.width, config.height, config.fps, config.bitrate_kbps, input_fmt);

        let nv12_frame = ffmpeg_next::frame::Video::new(sw_format, config.width, config.height);

        let bgra_frame = if supports_bgra_input {
            Some(ffmpeg_next::frame::Video::new(
                ffmpeg_next::format::Pixel::BGRA, config.width, config.height,
            ))
        } else {
            None
        };

        Ok(H264Encoder {
            h264_param_sets: None,
            hevc_description: None,
            av1_description,
            codec: target_codec,
            backend,
            encoder_name: codec_name,
            encoder,
            frame_count: 0,
            keyframe_interval: gop as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
            nv12_frame,
            sw_format,
            scaler: None,
            supports_bgra_input,
            bgra_frame,
            bgra_scaler: None,
            vaapi: vaapi.take(),
        })
    }

    /// FFmpeg's name of the open encoder ("h264_nvenc", "libsvtav1", …).
    pub fn encoder_name(&self) -> &'static str {
        self.encoder_name
    }

    /// Receive one encoded packet and produce a codec-appropriate EncodedFrame.
    fn receive_one_packet(&mut self) -> Option<EncodedFrame> {
        let mut packet = ffmpeg_next::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let raw_data = packet.data().unwrap_or(&[]).to_vec();
                let is_keyframe = packet.is_key();
                let pts = packet.pts().unwrap_or(0) as u64;
                self.build_encoded_frame(raw_data, is_keyframe, pts)
            }
            Err(_) => None,
        }
    }

    /// Copy the encoder's extradata as the WebCodecs description record.
    /// FFmpeg populates this with the codec-appropriate config record
    /// (avcC / hvcC / av1C) once the encoder is open. The label argument
    /// is for logging only.
    fn read_extradata_for_description(&self, label: &str) -> Option<Vec<u8>> {
        unsafe {
            let ctx_ptr = self.encoder.as_ptr();
            let extradata = (*ctx_ptr).extradata;
            let size = (*ctx_ptr).extradata_size as usize;
            if extradata.is_null() || size == 0 {
                log::info!("[encoder] WARNING: {} keyframe has no extradata", label);
                return None;
            }
            let bytes = std::slice::from_raw_parts(extradata, size).to_vec();
            log::info!("[encoder] {} description extracted ({} bytes)", label, bytes.len());
            Some(bytes)
        }
    }

    /// Encode a raw NV12 frame into H.264 AVCC format.
    /// Used by Windows capture backends which output NV12 directly.
    pub fn encode_nv12_frame(&mut self, nv12_data: &[u8], width: u32, height: u32) -> Result<Option<EncodedFrame>, String> {
        // An encoder opened for BGRA input lays a frame out by its own
        // format with the BGRA pitch: an NV12 frame sent to it reads (and,
        // in NVENC's copy, writes) past the frames' buffers. No capture
        // produces NV12 today; refuse rather than corrupt memory.
        if self.supports_bgra_input {
            return Err("NV12 input on an encoder opened for BGRA".to_string());
        }
        // Likewise an encoder whose CPU frame is I420 (software), or VAAPI
        // with its RGB upload surfaces.
        if self.sw_format != ffmpeg_next::format::Pixel::NV12
            || self.vaapi.as_ref().is_some_and(|v| v.gpu_convert())
        {
            return Err(format!("NV12 input is not supported by {}", self.encoder_name));
        }
        if width != self.target_width || height != self.target_height {
            return Err(format!("NV12 input of {width}x{height} on a {}x{} encoder", self.target_width, self.target_height));
        }
        if nv12_data.len() < (width as usize * height as usize * 3) / 2 {
            return Err("short NV12 frame".to_string());
        }
        let frame = &mut self.nv12_frame;

        // NV12 layout: Y plane (W×H bytes), UV plane (W×H/2 bytes, interleaved U,V)
        let y_size = (width * height) as usize;
        let uv_size = (width * height / 2) as usize;

        // Copy Y plane (plane 0)
        let y_stride = frame.stride(0);
        if y_stride == width as usize {
            frame.data_mut(0)[..y_size].copy_from_slice(&nv12_data[..y_size]);
        } else {
            for row in 0..height as usize {
                let src_off = row * width as usize;
                let dst_off = row * y_stride;
                frame.data_mut(0)[dst_off..dst_off + width as usize]
                    .copy_from_slice(&nv12_data[src_off..src_off + width as usize]);
            }
        }

        // Copy UV plane (plane 1) — interleaved U,V pairs, half height
        let uv_stride = frame.stride(1);
        let uv_src_offset = y_size;
        let half_h = (height / 2) as usize;
        if uv_stride == width as usize {
            frame.data_mut(1)[..uv_size].copy_from_slice(&nv12_data[uv_src_offset..uv_src_offset + uv_size]);
        } else {
            for row in 0..half_h {
                let src_off = uv_src_offset + row * width as usize;
                let dst_off = row * uv_stride;
                frame.data_mut(1)[dst_off..dst_off + width as usize]
                    .copy_from_slice(&nv12_data[src_off..src_off + width as usize]);
            }
        }

        self.prepare_and_encode()
    }

    /// Encode a raw BGRA/RGBA frame.
    /// - NVENC path: the frame goes into the BGRA input frame (an RGBA
    ///   capture has R and B swapped on the way) and the GPU converts to
    ///   NV12 — no CPU colour conversion.
    /// - VAAPI: uploaded as it is and converted + scaled by the GPU's video
    ///   processor (or, on the CPU tier, sws to NV12 and an NV12 upload).
    /// - Software: sws converts (and scales) to I420 for SVT-AV1.
    pub fn encode_bgra_frame(
        &mut self,
        rgba_data: &[u8],
        src_width: u32,
        src_height: u32,
        stride: usize,
        is_bgra: bool,
    ) -> Result<Option<EncodedFrame>, String> {
        let row_bytes = src_width as usize * 4;
        if src_width == 0 || src_height == 0 || stride < row_bytes {
            return Err(format!("bad frame geometry {src_width}x{src_height} stride {stride}"));
        }
        if rgba_data.len() < (src_height as usize - 1) * stride + row_bytes {
            return Err(format!(
                "short frame: {} bytes for {src_width}x{src_height} stride {stride}",
                rgba_data.len()
            ));
        }
        if self.vaapi.as_ref().is_some_and(|v| v.gpu_convert()) {
            let hw = match self.vaapi.as_mut() {
                Some(v) => v.convert(rgba_data, src_width, src_height, stride, is_bgra)?,
                None => return Err("VAAPI input missing".to_string()),
            };
            return self.send_hw_frame(hw);
        }
        let same_size = src_width == self.target_width && src_height == self.target_height;
        let route = input_route(self.supports_bgra_input, is_bgra, same_size);
        if route != InputRoute::ConvertToNv12 {
            return self.encode_bgra_direct(rgba_data, src_width, src_height, stride, is_bgra, route);
        }

        // CPU conversion via sws_scale into the encoder's CPU format (NV12
        // for VAAPI's CPU tier, I420 for software)
        let src_fmt = if is_bgra {
            ffmpeg_next::format::Pixel::BGRA
        } else {
            ffmpeg_next::format::Pixel::RGBA
        };

        if self.scaler.as_ref().is_none_or(|s| !s.matches(src_width, src_height, src_fmt)) {
            self.scaler = Some(YuvScaler::new(
                src_width, src_height, src_fmt,
                self.target_width, self.target_height,
                self.sw_format,
            )?);
        }
        // Propagate rather than .expect(): a scaler failure on the
        // per-frame path should kill the stream cleanly (caught by the
        // encode loop), not panic the encode thread.
        match self.scaler.as_mut() {
            Some(scaler) => scaler.convert(rgba_data, stride, &mut self.nv12_frame)?,
            None => return Err("scaler missing".to_string()),
        }
        self.prepare_and_encode()
    }

    /// NVENC fast path: fill the BGRA input frame and send it to the
    /// encoder; the GPU handles BGRA→NV12 conversion internally. An RGBA
    /// capture (RGBA/RGBx, which xdg-desktop-portal-wlr / -hyprland can
    /// negotiate) is swapped to BGRA in the same pass as the copy, or by
    /// sws when it is scaled anyway. The encoder was opened with
    /// pix_fmt=BGRA, so nothing else may reach it: NVENC copies an input
    /// frame by the frame's own layout with the BGRA pitch, and an NV12
    /// frame wrote its chroma plane past the input buffer (review B8).
    fn encode_bgra_direct(
        &mut self,
        data: &[u8],
        src_w: u32,
        src_h: u32,
        stride: usize,
        is_bgra: bool,
        route: InputRoute,
    ) -> Result<Option<EncodedFrame>, String> {
        let frame = self
            .bgra_frame
            .as_mut()
            .ok_or_else(|| "no BGRA input frame on this encoder".to_string())?;
        let (row_bytes, rows) = (src_w as usize * 4, src_h as usize);

        match route {
            InputRoute::CopyBgra => {
                let dst_stride = frame.stride(0);
                copy_rows(data, stride, frame.data_mut(0), dst_stride, row_bytes, rows);
            }
            InputRoute::SwizzleToBgra => {
                let dst_stride = frame.stride(0);
                swizzle_rows_rgba_to_bgra(data, stride, frame.data_mut(0), dst_stride, row_bytes, rows);
            }
            InputRoute::ScaleToBgra => {
                // Scale on the CPU (sws also swaps RGBA to BGRA), then the
                // GPU converts to NV12.
                let src_fmt = if is_bgra {
                    ffmpeg_next::format::Pixel::BGRA
                } else {
                    ffmpeg_next::format::Pixel::RGBA
                };
                if self.bgra_scaler.as_ref().is_none_or(|s| !s.matches(src_w, src_h, src_fmt)) {
                    self.bgra_scaler = Some(SwsScaler::new(
                        src_w, src_h, src_fmt,
                        self.target_width, self.target_height,
                        ffmpeg_next::format::Pixel::BGRA,
                    )?);
                }
                let sws = self.bgra_scaler.as_mut().unwrap();
                let src_stride_sws = sws.src_frame.stride(0);
                copy_rows(data, stride, sws.src_frame.data_mut(0), src_stride_sws, row_bytes, rows);
                sws.ctx.run(&sws.src_frame, frame)
                    .map_err(|e| format!("sws_scale bgra: {}", e))?;
            }
            InputRoute::ConvertToNv12 => {
                return Err("NV12 conversion requested on an encoder opened for BGRA".to_string());
            }
        }

        self.prepare_and_encode_bgra()
    }

    /// Set PTS, keyframe flags, and send the BGRA frame to the NVENC encoder.
    fn prepare_and_encode_bgra(&mut self) -> Result<Option<EncodedFrame>, String> {
        let frame = self.bgra_frame.as_mut().unwrap();
        frame.set_pts(Some(self.frame_count as i64));

        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        } else {
            frame.set_kind(ffmpeg_next::picture::Type::None);
        }

        self.frame_count += 1;

        match self.encoder.send_frame(self.bgra_frame.as_ref().unwrap()) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                let _ = self.receive_one_packet();
                self.encoder.send_frame(self.bgra_frame.as_ref().unwrap())
                    .map_err(|e| format!("Send frame (retry): {}", e))?;
            }
            Err(e) => return Err(format!("Send frame: {}", e)),
        }

        Ok(self.receive_one_packet())
    }

    /// Set PTS and keyframe flags on a hardware (VAAPI) surface and send it.
    /// `hw` stays owned by the VAAPI input; the encoder takes its own
    /// reference.
    fn send_hw_frame(&mut self, hw: *mut ffmpeg_next::sys::AVFrame) -> Result<Option<EncodedFrame>, String> {
        use ffmpeg_next::sys::{avcodec_send_frame, AVPictureType};
        // AVERROR(EAGAIN): the negative errno.
        const AGAIN: i32 = -ffmpeg_next::error::EAGAIN;
        unsafe {
            (*hw).pts = self.frame_count as i64;
            // vaapi_encode (hw_base_encode.c) turns pict_type I on an input
            // frame into a forced IDR.
            if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
                (*hw).pict_type = AVPictureType::AV_PICTURE_TYPE_I;
                self.force_next_keyframe = false;
            } else {
                (*hw).pict_type = AVPictureType::AV_PICTURE_TYPE_NONE;
            }
            self.frame_count += 1;
            let ctx = self.encoder.as_mut_ptr();
            let mut rc = avcodec_send_frame(ctx, hw);
            if rc == AGAIN {
                let pending = self.receive_one_packet();
                rc = avcodec_send_frame(ctx, hw);
                if rc < 0 {
                    return Err(format!("Send frame (retry): {}", ffmpeg_next::Error::from(rc)));
                }
                // The packet drained to make room is this call's output.
                return Ok(pending.or_else(|| self.receive_one_packet()));
            }
            if rc < 0 {
                return Err(format!("Send frame: {}", ffmpeg_next::Error::from(rc)));
            }
        }
        Ok(self.receive_one_packet())
    }

    /// A packet the encoder finished since the last call, if any. Software
    /// encodes on its own threads: a frame's packet is usually not ready
    /// when its `encode_*` call returns, so the caller drains with this
    /// after every frame and the encoder's queue never grows.
    pub fn receive_pending(&mut self) -> Option<EncodedFrame> {
        self.receive_one_packet()
    }

    /// Set PTS, keyframe flags, and send the prepared nv12_frame to the encoder.
    fn prepare_and_encode(&mut self) -> Result<Option<EncodedFrame>, String> {
        // VAAPI's CPU tier: the converted NV12 frame is uploaded to a
        // surface, which is what the encoder takes.
        if self.vaapi.is_some() {
            let hw = match self.vaapi.as_mut() {
                Some(v) => v.upload_nv12(unsafe { self.nv12_frame.as_ptr() })?,
                None => return Err("VAAPI input missing".to_string()),
            };
            return self.send_hw_frame(hw);
        }
        self.nv12_frame.set_pts(Some(self.frame_count as i64));

        // Force keyframe at interval or on demand (PLI)
        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            self.nv12_frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        } else {
            self.nv12_frame.set_kind(ffmpeg_next::picture::Type::None);
        }

        self.frame_count += 1;

        // Standard FFmpeg encode pattern: send frame, then try to receive output.
        // If send returns EAGAIN, receive one packet first to free a buffer, then retry.
        match self.encoder.send_frame(&self.nv12_frame) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                let _ = self.receive_one_packet();
                self.encoder.send_frame(&self.nv12_frame)
                    .map_err(|e| format!("Send frame (retry): {}", e))?;
            }
            Err(e) => return Err(format!("Send frame: {}", e)),
        }

        Ok(self.receive_one_packet())
    }

    /// Request the encoder to produce a keyframe on the next encode call.
    pub fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    /// Flush remaining frames from the encoder.
    pub fn flush(&mut self) -> Vec<EncodedFrame> {
        let _ = self.encoder.send_eof();
        let mut frames = Vec::new();
        let mut packet = ffmpeg_next::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            if let Some(data) = packet.data() {
                if let Some(frame) = self.build_encoded_frame(
                    data.to_vec(),
                    packet.is_key(),
                    packet.pts().unwrap_or(0) as u64,
                ) {
                    frames.push(frame);
                }
            }
        }
        frames
    }

    /// Codec-aware bitstream conversion + description extraction. Shared
    /// between receive_one_packet and flush so both produce the right
    /// EncodedFrame shape per codec.
    fn build_encoded_frame(&mut self, raw_data: Vec<u8>, is_keyframe: bool, pts: u64) -> Option<EncodedFrame> {
        use crate::media::caps::CodecKind;
        let (data, description) = match self.codec {
            CodecKind::H264Hw | CodecKind::H264Sw => {
                // Annex B exactly as FFmpeg emits it, no description — the
                // format every receiver configures for (Windows native and the
                // WebCodecs encoder send the same). This used to be converted
                // to AVCC (length-prefixed) with SPS/PPS moved into an avcC
                // description, but descriptions only go on the wire for
                // HEVC/AV1, so remote watchers got length-prefixed H.264 with
                // nothing to configure from and decoded nothing — only the
                // streamer's own preview, which got the description locally,
                // ever showed a picture.
                (self.with_h264_parameter_sets(raw_data, is_keyframe), None)
            }
            CodecKind::H265 => {
                let hvcc_data = annexb_to_avcc(&raw_data);
                let desc = if is_keyframe {
                    // Without GLOBAL_HEADER set on the HEVC encoder (see
                    // H264Encoder::new), VPS/SPS/PPS are inline in each
                    // keyframe. Build hvcC from those NALs directly — same
                    // builder we'd otherwise call on extradata, just pointed
                    // at the keyframe bitstream.
                    let built = build_hevc_hvcc(&raw_data);
                    if built.is_some() {
                        log::info!("[encoder] hvcC built from keyframe ({} bytes from {} bytes annex-B)",
                                  built.as_ref().unwrap().len(), raw_data.len());
                        self.hevc_description = built.clone();
                        built
                    } else if self.hevc_description.is_some() {
                        // A keyframe without inline VPS/SPS/PPS (a driver
                        // that writes them only on the first IDR): the
                        // description from the last one that had them
                        // still configures the decoder.
                        log::debug!("[encoder] HEVC keyframe without parameter sets; reusing the last hvcC");
                        self.hevc_description.clone()
                    } else {
                        log::warn!("[encoder] WARNING: failed to build hvcC from keyframe ({} bytes)", raw_data.len());
                        None
                    }
                } else { None };
                (hvcc_data, desc)
            }
            CodecKind::Av1 => {
                let desc = if is_keyframe && self.av1_description {
                    self.read_extradata_for_description("av1C")
                } else {
                    None
                };
                (raw_data, desc)
            }
            CodecKind::Unknown => return None,
        };
        Some(EncodedFrame { data, is_keyframe, pts, avcc_description: description })
    }

    /// Make sure an H.264 keyframe carries its SPS/PPS, so a watcher can
    /// start decoding on it. NVENC / VA-API / x264 repeat them on every IDR
    /// without GLOBAL_HEADER, but AMF only does on forced IDRs by default —
    /// so remember the last parameter sets seen and splice them into any
    /// keyframe that arrives without (after a leading AUD, if any).
    fn with_h264_parameter_sets(&mut self, data: Vec<u8>, is_keyframe: bool) -> Vec<u8> {
        use crate::media::encode_util::{contains_nal_type, insert_parameter_sets, NalCodec};
        if !is_keyframe {
            return data;
        }
        if contains_nal_type(&data, NalCodec::H264, NalCodec::H264.sps_type()) {
            if let Some(ps) = annexb_h264_parameter_sets(&data) {
                self.h264_param_sets = Some(ps);
            }
            return data;
        }
        match &self.h264_param_sets {
            Some(ps) => {
                log::debug!("[encoder] H.264 keyframe without SPS — inserting cached parameter sets");
                insert_parameter_sets(&data, ps, NalCodec::H264)
            }
            None => data,
        }
    }

}

#[cfg(test)]
mod tests {
    use super::annexb_h264_parameter_sets;
    use crate::media::encode_util::{contains_nal_type, insert_parameter_sets, NalCodec};

    // AUD (9), SPS (7), PPS (8), IDR slice (5) — 4- and 3-byte start codes.
    const KEYFRAME: &[u8] = &[
        0, 0, 0, 1, 0x09, 0xF0, //
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1F, //
        0, 0, 1, 0x68, 0xCE, 0x3C, 0x80, //
        0, 0, 0, 1, 0x65, 0x88, 0x84,
    ];
    // AUD + IDR slice only: an AMF natural IDR without parameter sets.
    const BARE_KEYFRAME: &[u8] = &[0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x65, 0x88, 0x84];

    #[test]
    fn extracts_sps_and_pps_as_annexb() {
        let ps = annexb_h264_parameter_sets(KEYFRAME).expect("SPS + PPS present");
        assert_eq!(
            ps,
            vec![0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1F, 0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]
        );
    }

    #[test]
    fn no_parameter_sets_without_both() {
        assert!(annexb_h264_parameter_sets(BARE_KEYFRAME).is_none());
        // SPS but no PPS.
        assert!(annexb_h264_parameter_sets(&[0, 0, 0, 1, 0x67, 0x42]).is_none());
    }

    #[test]
    fn cached_sets_make_a_bare_keyframe_decodable() {
        let ps = annexb_h264_parameter_sets(KEYFRAME).unwrap();
        assert!(!contains_nal_type(BARE_KEYFRAME, NalCodec::H264, 7));
        let fixed = insert_parameter_sets(BARE_KEYFRAME, &ps, NalCodec::H264);
        assert!(contains_nal_type(&fixed, NalCodec::H264, 7));
        assert!(contains_nal_type(&fixed, NalCodec::H264, 8));
        // The AUD stays first.
        assert_eq!(&fixed[..6], &[0, 0, 0, 1, 0x09, 0xF0]);
    }

    use super::{copy_rows, input_route, swizzle_rows_rgba_to_bgra, InputRoute};

    /// Review B8: an encoder opened for BGRA (NVENC) only ever gets its BGRA
    /// frame, whatever the capture negotiated. An RGBA capture used to fall
    /// through to the NV12 path and send the NV12 frame to the BGRA context.
    #[test]
    fn bgra_encoders_never_get_the_nv12_frame() {
        for is_bgra in [true, false] {
            for same_size in [true, false] {
                assert_ne!(input_route(true, is_bgra, same_size), InputRoute::ConvertToNv12);
                assert_eq!(input_route(false, is_bgra, same_size), InputRoute::ConvertToNv12);
            }
        }
        assert_eq!(input_route(true, true, true), InputRoute::CopyBgra);
        assert_eq!(input_route(true, false, true), InputRoute::SwizzleToBgra);
        assert_eq!(input_route(true, false, false), InputRoute::ScaleToBgra);
        assert_eq!(input_route(true, true, false), InputRoute::ScaleToBgra);
    }

    #[test]
    fn rgba_rows_land_as_bgra_across_strides() {
        // 2×2 RGBA, source stride 12 (4 bytes of padding, then a tight last
        // row), destination stride 16.
        #[rustfmt::skip]
        let src: [u8; 20] = [
            1, 2, 3, 4,   5, 6, 7, 8,   0xEE, 0xEE, 0xEE, 0xEE,
            9, 10, 11, 12,   13, 14, 15, 16,
        ];
        let mut dst = [0u8; 32];
        swizzle_rows_rgba_to_bgra(&src, 12, &mut dst, 16, 8, 2);
        assert_eq!(&dst[..8], &[3, 2, 1, 4, 7, 6, 5, 8]);
        assert_eq!(&dst[8..16], &[0; 8], "destination padding untouched");
        assert_eq!(&dst[16..24], &[11, 10, 9, 12, 15, 14, 13, 16]);
    }

    #[test]
    fn copy_rows_across_strides_and_a_tight_last_row() {
        let src = [1u8, 2, 3, 4, 0xEE, 0xEE, 5, 6, 7, 8];
        let mut packed = [0u8; 8];
        copy_rows(&src, 6, &mut packed, 4, 4, 2);
        assert_eq!(packed, [1, 2, 3, 4, 5, 6, 7, 8]);
        let mut same = [0u8; 12];
        copy_rows(&src, 6, &mut same, 6, 4, 2);
        assert_eq!(&same[..10], &src);
    }

    /// How long the CPU conversion takes (the software path's first step):
    /// `cargo test --release --lib yuv_scaler_speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn yuv_scaler_speed() {
        use super::YuvScaler;
        use ffmpeg_next::format::Pixel;
        ffmpeg_next::init().unwrap();
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        println!("{} swscale thread(s) on {cpus} logical CPUs", super::sws_threads(cpus));
        for (sw, sh, dw, dh) in [(1920u32, 1080u32, 1920u32, 1080u32), (2560, 1440, 2560, 1440), (2560, 1440, 1920, 1080)] {
            let stride = sw as usize * 4;
            let frame: Vec<u8> = (0..stride * sh as usize).map(|i| (i * 31 % 251) as u8).collect();
            let mut scaler = YuvScaler::new(sw, sh, Pixel::BGRA, dw, dh, Pixel::YUV420P).unwrap();
            let mut dst = ffmpeg_next::frame::Video::new(Pixel::YUV420P, dw, dh);
            let mut times = Vec::new();
            for _ in 0..60 {
                let t = std::time::Instant::now();
                scaler.convert(&frame, stride, &mut dst).unwrap();
                times.push(t.elapsed().as_secs_f64() * 1e3);
            }
            times.sort_by(|a, b| a.total_cmp(b));
            println!("{sw}x{sh} BGRA -> {dw}x{dh} I420: median {:.2} ms, p95 {:.2} ms", times[30], times[57]);
        }
    }

    /// A capture in `is_bgra` byte order with a padded stride: left half
    /// red, right half blue, padding bytes 0x77.
    fn two_tone(w: u32, h: u32, stride: usize, is_bgra: bool) -> Vec<u8> {
        let (red, blue): ([u8; 4], [u8; 4]) = if is_bgra {
            ([0, 0, 255, 255], [255, 0, 0, 255])
        } else {
            ([255, 0, 0, 255], [0, 0, 255, 255])
        };
        let mut f = vec![0x77u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let px = if x < w as usize / 2 { red } else { blue };
                f[y * stride + x * 4..][..4].copy_from_slice(&px);
            }
        }
        f
    }

    /// Decode an Annex B H.264 stream with the system ffmpeg; the last
    /// picture as RGB24.
    fn decode_last_frame_rgb(stream: &[u8], w: u32, h: u32, label: &str) -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("decibell-nvenc-{label}-{}.h264", std::process::id()));
        std::fs::write(&path, stream).unwrap();
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "h264", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .expect("run the system ffmpeg");
        let _ = std::fs::remove_file(&path);
        assert!(out.status.success(), "{label}: ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
        let size = (w * h * 3) as usize;
        assert!(out.stdout.len() >= size, "{label}: no decoded picture");
        out.stdout[out.stdout.len() - size..].to_vec()
    }

    /// Opens the real NVENC encoder and pushes synthetic captures through the
    /// stream's own path (`encode_bgra_frame`): BGRA and RGBA, at the encoder
    /// size and scaled, with padded strides. The system ffmpeg decodes the
    /// output and the colours are checked: a missed or doubled R/B swap turns
    /// red into blue. Before the fix the RGBA cases sent an NV12 frame to the
    /// BGRA context. Needs an NVIDIA GPU and `ffmpeg` on PATH:
    /// `cargo test --lib nvenc_ -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn nvenc_encodes_rgba_and_bgra_captures_in_true_colour() {
        let _one_at_a_time = crate::media::encoder_checks::encoder_lock();
        use super::{EncoderConfig, H264Encoder};
        use crate::media::caps::CodecKind;
        const W: u32 = 640;
        const H: u32 = 360;
        let cfg = EncoderConfig { width: W, height: H, fps: 30, bitrate_kbps: 4000, keyframe_interval_secs: 2, detail: false, source_size: (0, 0) };
        for (label, is_bgra, src_w, src_h) in [
            ("bgra", true, W, H),
            ("rgba", false, W, H),
            ("bgra-scaled", true, 1280, 720),
            ("rgba-scaled", false, 1280, 720),
        ] {
            let mut enc = H264Encoder::new(CodecKind::H264Hw, &cfg).expect("open h264_nvenc");
            assert!(enc.supports_bgra_input, "{label}: not an NVENC (BGRA input) encoder");
            let stride = src_w as usize * 4 + 64;
            let capture = two_tone(src_w, src_h, stride, is_bgra);
            let mut stream = Vec::new();
            for _ in 0..10 {
                if let Some(ef) = enc.encode_bgra_frame(&capture, src_w, src_h, stride, is_bgra).expect("encode") {
                    stream.extend_from_slice(&ef.data);
                }
            }
            for ef in enc.flush() {
                stream.extend_from_slice(&ef.data);
            }
            let rgb = decode_last_frame_rgb(&stream, W, H, label);
            let px = |x: u32, y: u32| {
                let i = ((y * W + x) * 3) as usize;
                (rgb[i], rgb[i + 1], rgb[i + 2])
            };
            let (r, g, b) = px(W / 4, H / 2);
            assert!(r > 180 && g < 70 && b < 70, "{label}: left half should be red, got ({r}, {g}, {b})");
            let (r, g, b) = px(3 * W / 4, H / 2);
            assert!(b > 180 && r < 70 && g < 70, "{label}: right half should be blue, got ({r}, {g}, {b})");
            println!("{label}: {} bytes of H.264, red and blue where they belong", stream.len());
        }
        // An NV12 frame never reaches a BGRA context.
        let mut enc = H264Encoder::new(CodecKind::H264Hw, &cfg).expect("open h264_nvenc");
        let nv12 = vec![0u8; (W * H * 3 / 2) as usize];
        assert!(enc.encode_nv12_frame(&nv12, W, H).is_err());
    }
}
