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
        eprintln!("[encoder] hvcC build failed — missing NAL types: vps={}, sps={}, pps={}",
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
        eprintln!("[encoder] hvcC build failed — SPS RBSP too short ({} bytes)", sps0.len());
        return None;
    }

    // Diagnostic: log both raw NAL and unescaped RBSP so a future bug
    // here is one log line away from being obvious.
    eprintln!("[encoder] HEVC SPS NAL ({} bytes): {:02X?}", sps0_raw.len(), sps0_raw);
    eprintln!("[encoder] HEVC SPS RBSP ({} bytes): {:02X?}", sps0.len(), &sps0[..sps0.len().min(20)]);

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

/// Build an avcC (AVCDecoderConfigurationRecord) from SPS and PPS NAL units.
/// This is required by WebCodecs as the `description` field in VideoDecoderConfig.
fn build_avcc_record(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();

    // configurationVersion
    record.push(1);
    // AVCProfileIndication (from SPS byte 1)
    record.push(if sps.len() > 1 { sps[1] } else { 0x64 }); // default High Profile
    // profile_compatibility (from SPS byte 2)
    record.push(if sps.len() > 2 { sps[2] } else { 0x00 });
    // AVCLevelIndication (from SPS byte 3)
    record.push(if sps.len() > 3 { sps[3] } else { 0x2A }); // default Level 4.2
    // lengthSizeMinusOne = 3 (we use 4-byte lengths) | 0xFC reserved bits
    record.push(0xFF);
    // numOfSequenceParameterSets = 1 | 0xE0 reserved bits
    record.push(0xE1);
    // SPS length (big-endian u16)
    record.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    // SPS data
    record.extend_from_slice(sps);
    // numOfPictureParameterSets = 1
    record.push(1);
    // PPS length (big-endian u16)
    record.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    // PPS data
    record.extend_from_slice(pps);

    record
}

/// Extract avcC description from AVCC-formatted data (4-byte length-prefixed NAL units).
/// Use this on the receiver side to build the description from reassembled keyframe data.
pub fn extract_avcc_description_from_avcc(avcc_data: &[u8]) -> Option<Vec<u8>> {
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut i = 0;
    let len = avcc_data.len();

    while i + 4 <= len {
        let nal_len = u32::from_be_bytes([avcc_data[i], avcc_data[i+1], avcc_data[i+2], avcc_data[i+3]]) as usize;
        i += 4;
        if i + nal_len > len { break; }
        let nal_type = avcc_data[i] & 0x1F;
        match nal_type {
            7 => sps = Some(avcc_data[i..i+nal_len].to_vec()),
            8 => pps = Some(avcc_data[i..i+nal_len].to_vec()),
            _ => {}
        }
        i += nal_len;
    }

    match (sps, pps) {
        (Some(s), Some(p)) => Some(build_avcc_record(&s, &p)),
        _ => None,
    }
}

/// Extract SPS and PPS NAL units from an Annex B keyframe and build avcC record.
fn extract_avcc_description(annexb_data: &[u8]) -> Option<Vec<u8>> {
    let nals = parse_annexb_nals(annexb_data);
    let mut sps: Option<&[u8]> = None;
    let mut pps: Option<&[u8]> = None;

    for (nal_type, nal_data) in &nals {
        match nal_type {
            7 => sps = Some(nal_data), // SPS
            8 => pps = Some(nal_data), // PPS
            _ => {}
        }
    }

    match (sps, pps) {
        (Some(s), Some(p)) => Some(build_avcc_record(s, p)),
        _ => None,
    }
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

/// Video encoder using FFmpeg's C API via ffmpeg-next. Despite the name
/// (kept stable for diff hygiene) this struct now supports H.264, H.265,
/// AV1, and x264 software via the `codec` field — selected at construction
/// time. Per-codec backends and config tuning live in find_hw_encoder /
/// codec_options below.
pub struct H264Encoder {
    /// Which codec this encoder produces. Plumbs through to the
    /// per-packet UdpVideoPacket.codec byte (Plan B Task 7) and to
    /// codec-specific config / extradata handling (Plan B Tasks 4-5).
    pub codec: crate::media::caps::CodecKind,
    encoder: ffmpeg_next::encoder::Video,
    frame_count: u64,
    keyframe_interval: u64,
    force_next_keyframe: bool,
    target_width: u32,
    target_height: u32,
    /// Persistent NV12 frame buffer — reused across encode calls to avoid
    /// allocating a new frame every time.
    nv12_frame: ffmpeg_next::frame::Video,
    /// Persistent scaler for BGRA→NV12 conversion (fallback for non-NVENC).
    scaler: Option<SwsScaler>,
    /// Whether the encoder accepts BGRA input directly (h264_nvenc).
    /// When true, NVENC handles BGRA→NV12 conversion on the GPU, avoiding
    /// expensive CPU-based sws_scale color conversion.
    supports_bgra_input: bool,
    /// Persistent BGRA frame buffer for direct BGRA input path.
    bgra_frame: Option<ffmpeg_next::frame::Video>,
    /// Persistent scaler for BGRA→BGRA scaling (used when source resolution
    /// differs from encoder resolution on the NVENC BGRA path).
    bgra_scaler: Option<SwsScaler>,

    /// NVIDIA CUDA: FFmpeg hw_device_ctx (AVBufferRef*). When set, NVENC
    /// reads frames from GPU memory instead of system memory.
    #[cfg(target_os = "linux")]
    cuda_hw_device_ref: *mut std::ffi::c_void,
    /// NVIDIA CUDA: hw_frames_ctx (AVBufferRef*) for frame pool.
    #[cfg(target_os = "linux")]
    cuda_hw_frames_ref: *mut std::ffi::c_void,

    /// VA-API: whether this encoder instance uses h264_vaapi with HW frames.
    #[cfg(target_os = "linux")]
    is_vaapi_hw: bool,
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe_interval_secs: u32,
}

#[derive(Debug)]
pub struct EncodedFrame {
    /// Encoded bitstream in the format the WebCodecs decoder expects:
    ///   - H.264 / H.265 → length-prefixed NALU (AVCC / HVCC)
    ///   - AV1           → OBU stream
    /// Codec is identified via the surrounding pipeline's CodecKind, NOT
    /// stored in this struct (it would duplicate H264Encoder.codec).
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts: u64,
    /// Codec decoder-configuration record, present on keyframes only:
    ///   - H.264 → avcC (built from SPS/PPS extracted from annex-B)
    ///   - H.265 → hvcC (FFmpeg encoder.extradata())
    ///   - AV1   → av1C (FFmpeg encoder.extradata())
    /// Field name kept stable for diff hygiene; semantics are codec-aware.
    /// Pass these bytes verbatim to WebCodecs VideoDecoder.configure({description}).
    pub avcc_description: Option<Vec<u8>>,
}

impl H264Encoder {
    /// Create a new video encoder for the given codec.
    /// Tries codec-specific backends in order: NVENC, VA-API (Linux), AMF/QSV/MF (Windows).
    /// For CodecKind::H264Sw, picks libx264 (no hardware fallback).
    pub fn new(target_codec: crate::media::caps::CodecKind, config: &EncoderConfig) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let (codec, codec_name) = Self::find_hw_encoder(target_codec)?;
        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("Encoder context: {}", e))?;

        context.set_width(config.width);
        context.set_height(config.height);
        context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
        context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_max_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_gop(config.fps * config.keyframe_interval_secs);

        // AV1 needs AV_CODEC_FLAG_GLOBAL_HEADER set so FFmpeg populates
        // extradata with a proper av1C config record (which build_encoded_frame
        // ships verbatim to WebCodecs as the description).
        // H.264 and H.265 use a different path: parse SPS/PPS (and VPS for
        // HEVC) out of the keyframe's annex-B inline NALs and build the
        // avcC / hvcC manually. This required for HEVC because Chromium's
        // WebCodecs HEVC decoder rendered NVENC-with-GLOBAL_HEADER bitstreams
        // as just the top-left ~1/4 of the picture, regardless of slices/tune.
        // Inline VPS/SPS/PPS in keyframes (no GLOBAL_HEADER) produces a
        // bitstream the decoder consumes correctly.
        if target_codec == crate::media::caps::CodecKind::Av1 {
            const AV_CODEC_FLAG_GLOBAL_HEADER: i32 = 1 << 22;
            unsafe {
                let p = context.as_mut_ptr();
                (*p).flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
            }
        }

        // Disable B-frames for real-time streaming — B-frames require reordering
        // which adds latency and can cause artifacts with simple decoders.
        context.set_max_b_frames(0);

        // NVENC (any codec) accepts BGRA directly — the GPU handles BGRA→NV12
        // conversion internally, eliminating expensive CPU-based sws_scale.
        let supports_bgra_input = cfg!(target_os = "linux") &&
            (codec_name == "h264_nvenc" || codec_name == "hevc_nvenc" || codec_name == "av1_nvenc");

        if supports_bgra_input {
            context.set_format(ffmpeg_next::format::Pixel::BGRA);
        } else {
            context.set_format(ffmpeg_next::format::Pixel::NV12);
        }

        // Signal BT.709 colorspace in SPS VUI so the decoder uses the correct
        // YUV→RGB matrix. Without this, decoders may assume BT.601 and produce
        // a green tint on HD content.
        context.set_colorspace(ffmpeg_next::color::Space::BT709);
        context.set_color_range(ffmpeg_next::color::Range::MPEG);

        let mut opts = ffmpeg_next::Dictionary::new();
        match codec_name.as_str() {
            // ── NVENC family (H.264 / HEVC / AV1) — same option vocabulary ──
            "h264_nvenc" | "hevc_nvenc" | "av1_nvenc" => {
                opts.set("forced_idr", "1");
                opts.set("preset", "p5");
                opts.set("rc", "cbr");
                // hevc_nvenc with tune=ull was producing pictures that
                // Chromium's WebCodecs decoder rendered as just the top-left
                // ~1280x720 region. Switching to tune=ll (still low-latency,
                // less aggressive) and forcing single-slice produces a
                // bitstream the decoder handles correctly. H.264 and AV1
                // NVENC remain on tune=ull where they work fine.
                if codec_name == "hevc_nvenc" {
                    opts.set("tune", "ll");
                    opts.set("slices", "1");
                } else {
                    opts.set("tune", "ull");
                }
                // VBV buffer: ~4 frames of headroom for rate control
                let vbv_bits = (config.bitrate_kbps as i32) * 1000 / (config.fps as i32) * 4;
                unsafe { (*context.as_mut_ptr()).rc_buffer_size = vbv_bits; }
            }
            // ── AMF family (AMD) ──
            "h264_amf" | "hevc_amf" | "av1_amf" => {
                opts.set("usage", "ultralowlatency");
                opts.set("quality", "speed");
            }
            // ── QSV family (Intel) ──
            "h264_qsv" | "hevc_qsv" | "av1_qsv" => {
                opts.set("preset", "veryfast");
                opts.set("forced_idr", "1");
            }
            // ── Media Foundation (Windows) — H.264 + HEVC only ──
            "h264_mf" | "hevc_mf" => {
                opts.set("rate_control", "cbr");
                opts.set("scenario", "display_remoting");
                opts.set("hw_encoding", "1");
            }
            // ── x264 software encoder ──
            "libx264" => {
                opts.set("preset", "veryfast");
                opts.set("tune", "zerolatency");
                opts.set("nal-hrd", "cbr");
                opts.set("forced-idr", "1");
            }
            _ => {
                // h264_vaapi / hevc_vaapi / av1_vaapi — use defaults
            }
        }

        let encoder = context
            .open_with(opts)
            .map_err(|e| format!("Open encoder: {}", e))?;

        let input_fmt = if supports_bgra_input { "BGRA (GPU convert)" } else { "NV12" };
        eprintln!("[encoder] {:?} encoder opened: {} — {}x{} @ {}fps, {}kbps, input={}",
                  target_codec, codec_name, config.width, config.height, config.fps, config.bitrate_kbps, input_fmt);

        let nv12_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12, config.width, config.height,
        );

        let bgra_frame = if supports_bgra_input {
            Some(ffmpeg_next::frame::Video::new(
                ffmpeg_next::format::Pixel::BGRA, config.width, config.height,
            ))
        } else {
            None
        };

        Ok(H264Encoder {
            codec: target_codec,
            encoder,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
            nv12_frame,
            scaler: None,
            supports_bgra_input,
            bgra_frame,
            bgra_scaler: None,
            #[cfg(target_os = "linux")]
            cuda_hw_device_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            cuda_hw_frames_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            is_vaapi_hw: false,
        })
    }

    /// Find the best available encoder backend for the given codec.
    /// Hardware backends tried in priority order; for CodecKind::H264Sw the
    /// only candidate is libx264 (no hardware fallback — software is the
    /// codec). CodecKind::Unknown returns an error.
    fn find_hw_encoder(
        target_codec: crate::media::caps::CodecKind,
    ) -> Result<(ffmpeg_next::Codec, String), String> {
        use crate::media::caps::CodecKind;
        let candidates: Vec<&str> = match target_codec {
            CodecKind::H264Hw => if cfg!(target_os = "linux") {
                vec!["h264_nvenc", "h264_vaapi", "h264_amf"]
            } else {
                vec!["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"]
            },
            CodecKind::H264Sw => vec!["libx264"],
            CodecKind::H265 => if cfg!(target_os = "linux") {
                vec!["hevc_nvenc", "hevc_vaapi", "hevc_amf"]
            } else {
                vec!["hevc_nvenc", "hevc_amf", "hevc_qsv", "hevc_mf"]
            },
            CodecKind::Av1 => if cfg!(target_os = "linux") {
                vec!["av1_nvenc", "av1_vaapi", "av1_amf"]
            } else {
                vec!["av1_nvenc", "av1_amf", "av1_qsv"]
            },
            CodecKind::Unknown => return Err("Cannot construct encoder for CODEC_UNKNOWN".to_string()),
        };

        for name in &candidates {
            if let Some(codec) = ffmpeg_next::encoder::find_by_name(name) {
                log::info!("Using {:?} encoder: {}", target_codec, name);
                return Ok((codec, name.to_string()));
            }
        }
        Err(format!(
            "No encoder available for {:?}. Tried: {:?}. Check FFmpeg build features and hardware drivers.",
            target_codec, candidates
        ))
    }

    /// Initialize CUDA hardware frame encoding for NVENC.
    /// After this, `encode_cuda_frame()` accepts CUdeviceptr from GPU memory.
    ///
    /// `external_cu_ctx`, if non-null, is a CUcontext created elsewhere
    /// (specifically: by our `gpu_interop::GpuContext`) that the encoder
    /// must share. CUDA device pointers are context-scoped — passing a
    /// `dev_ptr` allocated in context A into an NVENC session bound to
    /// context B returns EINVAL. Sharing one context lets our DMA-BUF
    /// imports and NVENC use the same memory addresses.
    #[cfg(target_os = "linux")]
    pub fn init_cuda_hw(&mut self, external_cu_ctx: *mut std::ffi::c_void) -> Result<(), String> {
        use ffmpeg_next::sys::*;

        unsafe {
            let mut hw_device_ref: *mut AVBufferRef = if external_cu_ctx.is_null() {
                // No external context: let FFmpeg create its own.
                let mut dev_ref: *mut AVBufferRef = std::ptr::null_mut();
                let rc = av_hwdevice_ctx_create(
                    &mut dev_ref,
                    AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    0,
                );
                if rc < 0 || dev_ref.is_null() {
                    return Err(format!("av_hwdevice_ctx_create(CUDA) failed: {}", rc));
                }
                dev_ref
            } else {
                // Share the caller's CUDA context. Allocate the hw device
                // buffer, manually write our context pointer into the
                // AVCUDADeviceContext.cuda_ctx field, then init it.
                //
                // AVCUDADeviceContext layout:
                //   CUcontext cuda_ctx;   // offset 0  (8 bytes on 64-bit)
                //   CUstream  stream;     // offset 8
                //   void*     internal;   // offset 16
                let dev_ref = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA);
                if dev_ref.is_null() {
                    return Err("av_hwdevice_ctx_alloc(CUDA) failed".into());
                }
                let hw_dev_ctx = (*dev_ref).data as *mut AVHWDeviceContext;
                let cuda_dev_ctx = (*hw_dev_ctx).hwctx as *mut std::ffi::c_void;
                // First field of AVCUDADeviceContext is `CUcontext cuda_ctx`.
                *(cuda_dev_ctx as *mut *mut std::ffi::c_void) = external_cu_ctx;
                let rc = av_hwdevice_ctx_init(dev_ref);
                if rc < 0 {
                    let mut dr = dev_ref;
                    av_buffer_unref(&mut dr);
                    return Err(format!("av_hwdevice_ctx_init(CUDA, shared ctx) failed: {}", rc));
                }
                eprintln!("[encoder] CUDA hw_device_ctx initialized with shared context");
                dev_ref
            };

            // Create hw_frames_ctx
            let frames_ref = av_hwframe_ctx_alloc(hw_device_ref);
            if frames_ref.is_null() {
                av_buffer_unref(&mut hw_device_ref);
                return Err("av_hwframe_ctx_alloc(CUDA) failed".into());
            }

            let frames_ctx = (*frames_ref).data as *mut AVHWFramesContext;
            (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_CUDA;
            // NVENC accepts BGRA as sw_format — GPU handles BGRA->NV12 internally
            (*frames_ctx).sw_format = if self.supports_bgra_input {
                AVPixelFormat::AV_PIX_FMT_BGRA
            } else {
                AVPixelFormat::AV_PIX_FMT_NV12
            };
            (*frames_ctx).width = self.target_width as libc::c_int;
            (*frames_ctx).height = self.target_height as libc::c_int;
            (*frames_ctx).initial_pool_size = 4;

            let rc = av_hwframe_ctx_init(frames_ref);
            if rc < 0 {
                let mut frames_ref_mut = frames_ref;
                av_buffer_unref(&mut frames_ref_mut);
                av_buffer_unref(&mut hw_device_ref);
                return Err(format!("av_hwframe_ctx_init(CUDA) failed: {}", rc));
            }

            self.cuda_hw_device_ref = hw_device_ref as *mut std::ffi::c_void;
            self.cuda_hw_frames_ref = frames_ref as *mut std::ffi::c_void;

            eprintln!("[encoder] CUDA hw_frames_ctx initialized ({}x{})",
                self.target_width, self.target_height);
            Ok(())
        }
    }

    /// Encode a frame from CUDA device memory (NVIDIA zero-copy path).
    /// `dev_ptr` is a CUdeviceptr to BGRA pixel data in GPU-linear memory.
    #[cfg(target_os = "linux")]
    pub fn encode_cuda_frame(
        &mut self,
        dev_ptr: u64,
        width: u32,
        height: u32,
    ) -> Result<Option<EncodedFrame>, String> {
        use ffmpeg_next::sys::*;

        if self.cuda_hw_frames_ref.is_null() {
            return Err("CUDA encoding not initialized".into());
        }

        unsafe {
            // Allocate a CUDA AVFrame from the hw_frames pool
            let frame = av_frame_alloc();
            if frame.is_null() {
                return Err("av_frame_alloc failed".into());
            }

            (*frame).format = AVPixelFormat::AV_PIX_FMT_CUDA as libc::c_int;
            (*frame).width = width as libc::c_int;
            (*frame).height = height as libc::c_int;
            (*frame).hw_frames_ctx = av_buffer_ref(self.cuda_hw_frames_ref as *const AVBufferRef);

            let rc = av_hwframe_get_buffer(
                self.cuda_hw_frames_ref as *mut AVBufferRef, frame, 0,
            );
            if rc < 0 {
                av_frame_free(&mut (frame as *mut AVFrame));
                return Err(format!("av_hwframe_get_buffer(CUDA) failed: {}", rc));
            }

            // Copy our dev_ptr content into the hwframe-allocated buffer.
            // av_hwframe_get_buffer returned a CUDA buffer with NVENC-aligned
            // pitch (frame->linesize[0]); overwriting frame->data[0] to point
            // at our tight-packed buffer was what triggered "Invalid argument"
            // — FFmpeg/NVENC saw a data[0] not matching its buf[0] ref. A
            // cuMemcpy2D on the same context keeps everything consistent and
            // pays only a GPU-local copy (<0.1ms for 1080p BGRA).
            let bpp: u32 = if self.supports_bgra_input { 4 } else { 1 }; // BGRA=4, NV12 Y plane=1
            let src_pitch = (width * bpp) as usize;
            let dst_pitch = (*frame).linesize[0] as usize;
            let dst_dev_ptr = (*frame).data[0] as u64;

            // Dynamically load cuMemcpy2D once (we don't own the global
            // CudaApi here, so just grab the entry we need).
            let cu_memcpy_fn = {
                let cu_lib = libloading::Library::new("libcuda.so.1")
                    .map_err(|e| format!("libcuda.so.1 load: {}", e))?;
                let sym: libloading::Symbol<unsafe extern "C" fn(*const u8) -> i32> =
                    cu_lib.get(b"cuMemcpy2D_v2\0")
                        .map_err(|e| format!("cuMemcpy2D_v2 lookup: {}", e))?;
                let ptr = *sym;
                // Leak the Library to keep the symbol valid for the process lifetime.
                std::mem::forget(cu_lib);
                ptr
            };

            // CUDA_MEMCPY2D struct — layout must match the driver API header.
            //   u=0 offset bytes/lines, memory type 2 = device
            const CU_MEMTYPE_DEVICE: u32 = 2;
            #[repr(C)]
            struct CudaMemcpy2D {
                src_x_in_bytes: usize,
                src_y: usize,
                src_memory_type: u32,
                src_host: *const std::ffi::c_void,
                src_device: u64,
                src_array: *mut std::ffi::c_void,
                _src_reserved: usize,
                src_pitch: usize,
                dst_x_in_bytes: usize,
                dst_y: usize,
                dst_memory_type: u32,
                dst_host: *mut std::ffi::c_void,
                dst_device: u64,
                dst_array: *mut std::ffi::c_void,
                _dst_reserved: usize,
                dst_pitch: usize,
                width_in_bytes: usize,
                height: usize,
            }
            let copy = CudaMemcpy2D {
                src_x_in_bytes: 0, src_y: 0,
                src_memory_type: CU_MEMTYPE_DEVICE,
                src_host: std::ptr::null(), src_device: dev_ptr,
                src_array: std::ptr::null_mut(), _src_reserved: 0,
                src_pitch,
                dst_x_in_bytes: 0, dst_y: 0,
                dst_memory_type: CU_MEMTYPE_DEVICE,
                dst_host: std::ptr::null_mut(), dst_device: dst_dev_ptr,
                dst_array: std::ptr::null_mut(), _dst_reserved: 0,
                dst_pitch,
                width_in_bytes: (width * bpp) as usize,
                height: height as usize,
            };
            let rc = cu_memcpy_fn(&copy as *const _ as *const u8);
            if rc != 0 {
                av_frame_free(&mut (frame as *mut AVFrame));
                return Err(format!("cuMemcpy2D(dev_ptr -> hwframe) failed: {}", rc));
            }

            // Set PTS and keyframe flags
            (*frame).pts = self.frame_count as i64;
            if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
                (*frame).pict_type = AVPictureType::AV_PICTURE_TYPE_I;
                self.force_next_keyframe = false;
            } else {
                (*frame).pict_type = AVPictureType::AV_PICTURE_TYPE_NONE;
            }
            self.frame_count += 1;

            // Wrap in ffmpeg_next for send_frame
            let wrapped = ffmpeg_next::frame::Video::wrap(frame);

            let send_result = self.encoder.send_frame(&wrapped);
            match send_result {
                Ok(()) => {}
                Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                    let _ = self.receive_one_packet();
                    self.encoder.send_frame(&wrapped)
                        .map_err(|e| format!("Send CUDA frame (retry): {}", e))?;
                }
                Err(e) => {
                    return Err(format!("Send CUDA frame: {}", e));
                }
            }

            Ok(self.receive_one_packet())
        }
    }

    /// Check if CUDA hw encoding is ready.
    #[cfg(target_os = "linux")]
    pub fn has_cuda_hw(&self) -> bool {
        !self.cuda_hw_frames_ref.is_null()
    }

    /// Create a new H264Encoder specifically for VA-API with hw_device_ctx.
    /// Used when GPU context detected VAAPI backend (AMD/Intel).
    #[cfg(target_os = "linux")]
    pub fn new_vaapi(
        config: &EncoderConfig,
        vaapi_device_ref: *mut ffmpeg_next::sys::AVBufferRef,
        vaapi_frames_ref: *mut ffmpeg_next::sys::AVBufferRef,
    ) -> Result<Self, String> {
        use ffmpeg_next::sys::*;

        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let codec = ffmpeg_next::encoder::find_by_name("h264_vaapi")
            .ok_or("h264_vaapi encoder not found")?;

        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder().video()
            .map_err(|e| format!("VAAPI encoder context: {}", e))?;

        context.set_width(config.width);
        context.set_height(config.height);
        context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
        context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_max_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_gop(config.fps * config.keyframe_interval_secs);
        context.set_max_b_frames(0);
        context.set_format(ffmpeg_next::format::Pixel::VAAPI);

        // Set hw_device_ctx and hw_frames_ctx on the raw AVCodecContext
        unsafe {
            let ctx_ptr = context.as_mut_ptr();
            (*ctx_ptr).hw_device_ctx = av_buffer_ref(vaapi_device_ref);
            (*ctx_ptr).hw_frames_ctx = av_buffer_ref(vaapi_frames_ref);
        }

        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("rc_mode", "CBR");

        let encoder = context.open_with(opts)
            .map_err(|e| format!("Open h264_vaapi encoder: {}", e))?;

        eprintln!("[encoder] h264_vaapi opened: {}x{} @ {}fps, {}kbps (DMA-BUF zero-copy)",
            config.width, config.height, config.fps, config.bitrate_kbps);

        // NV12 frame is unused in VAAPI path but required by struct
        let nv12_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12, config.width, config.height,
        );

        Ok(H264Encoder {
            // VAAPI path is currently H.264-only (Plan B Group 2 limits scope).
            codec: crate::media::caps::CodecKind::H264Hw,
            encoder,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
            nv12_frame,
            scaler: None,
            supports_bgra_input: false,
            bgra_frame: None,
            bgra_scaler: None,
            #[cfg(target_os = "linux")]
            cuda_hw_device_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            cuda_hw_frames_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            is_vaapi_hw: true,
        })
    }

    /// NVIDIA CUDA-native constructor. Creates a hw_device_ctx sharing the
    /// caller's CUcontext, a hw_frames_ctx for the pool, and opens h264_nvenc
    /// with `pix_fmt=AV_PIX_FMT_CUDA` so NVENC accepts CUDA frames directly.
    ///
    /// This replaces the older `H264Encoder::new` + `init_cuda_hw` flow: the
    /// older flow opened the encoder with `pix_fmt=BGRA` (CPU-side memory)
    /// and later tried to feed it `AV_PIX_FMT_CUDA` frames, which NVENC
    /// rejected with EINVAL because the codec context was already configured
    /// for a different frame kind.
    #[cfg(target_os = "linux")]
    pub fn new_cuda(
        config: &EncoderConfig,
        external_cu_ctx: *mut std::ffi::c_void,
    ) -> Result<Self, String> {
        use ffmpeg_next::sys::*;

        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let codec = ffmpeg_next::encoder::find_by_name("h264_nvenc")
            .ok_or_else(|| "h264_nvenc encoder not available".to_string())?;

        // Build hw_device_ctx with the shared CUcontext.
        let (hw_device_ref, hw_frames_ref) = unsafe {
            let dev_ref = if external_cu_ctx.is_null() {
                let mut r: *mut AVBufferRef = std::ptr::null_mut();
                let rc = av_hwdevice_ctx_create(
                    &mut r, AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                    std::ptr::null(), std::ptr::null_mut(), 0,
                );
                if rc < 0 || r.is_null() {
                    return Err(format!("av_hwdevice_ctx_create(CUDA) failed: {}", rc));
                }
                r
            } else {
                let r = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA);
                if r.is_null() {
                    return Err("av_hwdevice_ctx_alloc(CUDA) failed".into());
                }
                let hw_dev_ctx = (*r).data as *mut AVHWDeviceContext;
                let cuda_dev_ctx = (*hw_dev_ctx).hwctx as *mut std::ffi::c_void;
                *(cuda_dev_ctx as *mut *mut std::ffi::c_void) = external_cu_ctx;
                let rc = av_hwdevice_ctx_init(r);
                if rc < 0 {
                    let mut rr = r;
                    av_buffer_unref(&mut rr);
                    return Err(format!("av_hwdevice_ctx_init(CUDA, shared) failed: {}", rc));
                }
                r
            };

            let frames_ref = av_hwframe_ctx_alloc(dev_ref);
            if frames_ref.is_null() {
                let mut dr = dev_ref;
                av_buffer_unref(&mut dr);
                return Err("av_hwframe_ctx_alloc(CUDA) failed".into());
            }
            let frames_ctx = (*frames_ref).data as *mut AVHWFramesContext;
            (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_CUDA;
            (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_BGRA;
            (*frames_ctx).width = config.width as libc::c_int;
            (*frames_ctx).height = config.height as libc::c_int;
            (*frames_ctx).initial_pool_size = 4;
            let rc = av_hwframe_ctx_init(frames_ref);
            if rc < 0 {
                let mut fr = frames_ref;
                let mut dr = dev_ref;
                av_buffer_unref(&mut fr);
                av_buffer_unref(&mut dr);
                return Err(format!("av_hwframe_ctx_init(CUDA) failed: {}", rc));
            }
            eprintln!("[encoder] CUDA hw_frames_ctx initialized ({}x{}) with shared ctx",
                config.width, config.height);
            (dev_ref, frames_ref)
        };

        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder().video()
            .map_err(|e| {
                unsafe {
                    let mut fr = hw_frames_ref;
                    let mut dr = hw_device_ref;
                    av_buffer_unref(&mut fr);
                    av_buffer_unref(&mut dr);
                }
                format!("CUDA encoder context: {}", e)
            })?;

        context.set_width(config.width);
        context.set_height(config.height);
        context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
        context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_max_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_gop(config.fps * config.keyframe_interval_secs);
        context.set_max_b_frames(0);
        context.set_format(ffmpeg_next::format::Pixel::CUDA);
        context.set_colorspace(ffmpeg_next::color::Space::BT709);
        context.set_color_range(ffmpeg_next::color::Range::MPEG);

        // Attach hw_device_ctx and hw_frames_ctx. AVCodecContext takes its own
        // ref; we keep our own copy in self for later frame allocation.
        unsafe {
            let ctx_ptr = context.as_mut_ptr();
            (*ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ref);
            (*ctx_ptr).hw_frames_ctx = av_buffer_ref(hw_frames_ref);
            // VBV buffer: ~4 frames of headroom for rate control
            let vbv_bits = (config.bitrate_kbps as i32) * 1000 / (config.fps as i32) * 4;
            (*ctx_ptr).rc_buffer_size = vbv_bits;
        }

        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("forced_idr", "1");
        opts.set("preset", "p5");
        opts.set("tune", "ull");
        opts.set("rc", "cbr");

        let encoder = context.open_with(opts)
            .map_err(|e| {
                unsafe {
                    let mut fr = hw_frames_ref;
                    let mut dr = hw_device_ref;
                    av_buffer_unref(&mut fr);
                    av_buffer_unref(&mut dr);
                }
                format!("Open h264_nvenc (CUDA path): {}", e)
            })?;

        eprintln!(
            "[encoder] h264_nvenc opened with CUDA pix_fmt: {}x{} @ {}fps, {}kbps (zero-copy)",
            config.width, config.height, config.fps, config.bitrate_kbps
        );

        let nv12_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12, config.width, config.height,
        );

        Ok(H264Encoder {
            // CUDA path is currently H.264-only (Plan B Group 2 limits scope).
            codec: crate::media::caps::CodecKind::H264Hw,
            encoder,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
            nv12_frame,
            scaler: None,
            supports_bgra_input: true,
            bgra_frame: None,
            bgra_scaler: None,
            cuda_hw_device_ref: hw_device_ref as *mut std::ffi::c_void,
            cuda_hw_frames_ref: hw_frames_ref as *mut std::ffi::c_void,
            is_vaapi_hw: false,
        })
    }

    /// Encode a VA-API hardware frame (AMD/Intel zero-copy path).
    /// The `vaapi_frame` must have format=VAAPI with a valid VASurface.
    #[cfg(target_os = "linux")]
    pub fn encode_vaapi_frame(
        &mut self,
        vaapi_frame: &mut ffmpeg_next::frame::Video,
    ) -> Result<Option<EncodedFrame>, String> {
        vaapi_frame.set_pts(Some(self.frame_count as i64));

        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            vaapi_frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        } else {
            vaapi_frame.set_kind(ffmpeg_next::picture::Type::None);
        }
        self.frame_count += 1;

        match self.encoder.send_frame(vaapi_frame) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                let _ = self.receive_one_packet();
                self.encoder.send_frame(vaapi_frame)
                    .map_err(|e| format!("Send VAAPI frame (retry): {}", e))?;
            }
            Err(e) => return Err(format!("Send VAAPI frame: {}", e)),
        }

        Ok(self.receive_one_packet())
    }

    /// Check if this encoder uses VA-API hardware frames.
    #[cfg(target_os = "linux")]
    pub fn is_vaapi_hw(&self) -> bool {
        self.is_vaapi_hw
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

    /// HEVC-specific: read the encoder's extradata (annex-B VPS+SPS+PPS),
    /// convert to a proper hvcC record per ISO/IEC 14496-15. Cached on
    /// first call would be a future optimization (extradata doesn't
    /// change after encoder open) but for now we rebuild per keyframe.
    fn build_hvcc_description(&self) -> Option<Vec<u8>> {
        let raw = self.read_raw_extradata()?;
        if raw.is_empty() {
            eprintln!("[encoder] WARNING: HEVC extradata empty");
            return None;
        }
        // Diagnostic: log first 16 bytes so the format is unambiguous in
        // debug output. annex-B starts with 00 00 00 01, hvcC starts with 01.
        let preview_len = raw.len().min(16);
        eprintln!("[encoder] HEVC extradata first {} bytes: {:02X?}", preview_len, &raw[..preview_len]);
        let hvcc = build_hevc_hvcc(&raw)?;
        eprintln!("[encoder] hvcC built ({} bytes from {} bytes annex-B)", hvcc.len(), raw.len());
        Some(hvcc)
    }

    fn read_raw_extradata(&self) -> Option<Vec<u8>> {
        unsafe {
            let ctx_ptr = self.encoder.as_ptr();
            let extradata = (*ctx_ptr).extradata;
            let size = (*ctx_ptr).extradata_size as usize;
            if extradata.is_null() || size == 0 { return None; }
            Some(std::slice::from_raw_parts(extradata, size).to_vec())
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
                eprintln!("[encoder] WARNING: {} keyframe has no extradata", label);
                return None;
            }
            let bytes = std::slice::from_raw_parts(extradata, size).to_vec();
            eprintln!("[encoder] {} description extracted ({} bytes)", label, bytes.len());
            Some(bytes)
        }
    }

    /// Encode a raw NV12 frame into H.264 AVCC format.
    /// Used by Windows capture backends which output NV12 directly.
    pub fn encode_nv12_frame(&mut self, nv12_data: &[u8], width: u32, height: u32) -> Result<Option<EncodedFrame>, String> {
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
    /// - NVENC path: sends BGRA directly to the GPU encoder (no CPU color conversion).
    /// - Fallback path: converts to NV12 via CPU sws_scale (for VA-API etc.).
    pub fn encode_bgra_frame(
        &mut self,
        rgba_data: &[u8],
        src_width: u32,
        src_height: u32,
        stride: usize,
        is_bgra: bool,
    ) -> Result<Option<EncodedFrame>, String> {
        // Fast path: NVENC accepts BGRA directly, GPU handles color conversion
        if self.supports_bgra_input && is_bgra {
            return self.encode_bgra_direct(rgba_data, src_width, src_height, stride);
        }

        // Fallback: CPU-based BGRA→NV12 conversion via sws_scale
        let src_fmt = if is_bgra {
            ffmpeg_next::format::Pixel::BGRA
        } else {
            ffmpeg_next::format::Pixel::RGBA
        };

        if self.scaler.as_ref().map_or(true, |s| !s.matches(src_width, src_height, src_fmt)) {
            self.scaler = Some(SwsScaler::new(
                src_width, src_height, src_fmt,
                self.target_width, self.target_height,
                ffmpeg_next::format::Pixel::NV12,
            )?);
        }
        let sws = self.scaler.as_mut().unwrap();

        let src_stride = sws.src_frame.stride(0);
        if stride == src_stride {
            let copy_size = (src_height as usize) * stride;
            sws.src_frame.data_mut(0)[..copy_size].copy_from_slice(&rgba_data[..copy_size]);
        } else {
            let row_bytes = (src_width as usize) * 4;
            for row in 0..src_height as usize {
                let src_off = row * stride;
                let dst_off = row * src_stride;
                sws.src_frame.data_mut(0)[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&rgba_data[src_off..src_off + row_bytes]);
            }
        }

        sws.ctx.run(&sws.src_frame, &mut self.nv12_frame).expect("sws_scale");
        self.prepare_and_encode()
    }

    /// NVENC fast path: send BGRA frames directly to the encoder.
    /// The GPU handles BGRA→NV12 conversion internally.
    fn encode_bgra_direct(
        &mut self,
        data: &[u8],
        src_w: u32,
        src_h: u32,
        stride: usize,
    ) -> Result<Option<EncodedFrame>, String> {
        let frame = self.bgra_frame.as_mut().unwrap();

        if src_w == self.target_width && src_h == self.target_height {
            // No scaling needed — single copy into encoder frame
            let dst_stride = frame.stride(0);
            if stride == dst_stride {
                let size = src_h as usize * stride;
                frame.data_mut(0)[..size].copy_from_slice(&data[..size]);
            } else {
                let row_bytes = src_w as usize * 4;
                for row in 0..src_h as usize {
                    let src_off = row * stride;
                    let dst_off = row * dst_stride;
                    frame.data_mut(0)[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&data[src_off..src_off + row_bytes]);
                }
            }
        } else {
            // Scale BGRA→BGRA on CPU, then GPU converts to NV12
            if self.bgra_scaler.as_ref().map_or(true, |s| !s.matches(src_w, src_h, ffmpeg_next::format::Pixel::BGRA)) {
                self.bgra_scaler = Some(SwsScaler::new(
                    src_w, src_h, ffmpeg_next::format::Pixel::BGRA,
                    self.target_width, self.target_height,
                    ffmpeg_next::format::Pixel::BGRA,
                )?);
            }
            let sws = self.bgra_scaler.as_mut().unwrap();

            let src_stride_sws = sws.src_frame.stride(0);
            if stride == src_stride_sws {
                let size = src_h as usize * stride;
                sws.src_frame.data_mut(0)[..size].copy_from_slice(&data[..size]);
            } else {
                let row_bytes = src_w as usize * 4;
                for row in 0..src_h as usize {
                    let src_off = row * stride;
                    let dst_off = row * src_stride_sws;
                    sws.src_frame.data_mut(0)[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&data[src_off..src_off + row_bytes]);
                }
            }

            sws.ctx.run(&sws.src_frame, frame).expect("sws_scale bgra");
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

    /// Set PTS, keyframe flags, and send the prepared nv12_frame to the encoder.
    fn prepare_and_encode(&mut self) -> Result<Option<EncodedFrame>, String> {
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
    fn build_encoded_frame(&self, raw_data: Vec<u8>, is_keyframe: bool, pts: u64) -> Option<EncodedFrame> {
        use crate::media::caps::CodecKind;
        let (data, description) = match self.codec {
            CodecKind::H264Hw | CodecKind::H264Sw => {
                let avcc_data = annexb_to_avcc(&raw_data);
                let desc = if is_keyframe { extract_avcc_description(&raw_data) } else { None };
                (avcc_data, desc)
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
                        eprintln!("[encoder] hvcC built from keyframe ({} bytes from {} bytes annex-B)",
                                  built.as_ref().unwrap().len(), raw_data.len());
                    } else {
                        eprintln!("[encoder] WARNING: failed to build hvcC from keyframe ({} bytes)", raw_data.len());
                    }
                    built
                } else { None };
                (hvcc_data, desc)
            }
            CodecKind::Av1 => {
                let desc = if is_keyframe { self.read_extradata_for_description("av1C") } else { None };
                (raw_data, desc)
            }
            CodecKind::Unknown => return None,
        };
        Some(EncodedFrame { data, is_keyframe, pts, avcc_description: description })
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use ffmpeg_next::sys::*;
            unsafe {
                if !self.cuda_hw_frames_ref.is_null() {
                    let mut ptr = self.cuda_hw_frames_ref as *mut AVBufferRef;
                    av_buffer_unref(&mut ptr);
                    self.cuda_hw_frames_ref = std::ptr::null_mut();
                }
                if !self.cuda_hw_device_ref.is_null() {
                    let mut ptr = self.cuda_hw_device_ref as *mut AVBufferRef;
                    av_buffer_unref(&mut ptr);
                    self.cuda_hw_device_ref = std::ptr::null_mut();
                }
            }
        }
    }
}
