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

/// H.264 hardware encoder using FFmpeg's C API via ffmpeg-next.
pub struct H264Encoder {
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
    /// H.264 data in AVCC format (4-byte length-prefixed NAL units).
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts: u64,
    /// avcC decoder configuration record (present on keyframes only).
    /// Needed by WebCodecs VideoDecoder as the `description` parameter.
    pub avcc_description: Option<Vec<u8>>,
}

impl H264Encoder {
    /// Create a new H.264 hardware encoder.
    /// Tries hardware encoders in order: NVENC, VA-API (Linux), AMF/QSV (Windows).
    pub fn new(config: &EncoderConfig) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let (codec, codec_name) = Self::find_hw_encoder()?;
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
        // VBV buffer: ~4 frames of headroom. Balances quality on complex frames
        // against UDP transport limits (avoids large bitrate spikes that cause
        // packet loss).
        let vbv_bits = (config.bitrate_kbps as i32) * 1000 / (config.fps as i32) * 4;
        unsafe { (*context.as_mut_ptr()).rc_buffer_size = vbv_bits; }
        context.set_gop(config.fps * config.keyframe_interval_secs);
        // Disable B-frames for real-time streaming — B-frames require reordering
        // which adds latency and can cause artifacts with simple decoders.
        context.set_max_b_frames(0);

        // NVENC accepts BGRA directly — the GPU handles BGRA→NV12 conversion
        // internally, eliminating expensive CPU-based sws_scale.
        let supports_bgra_input = codec_name == "h264_nvenc";

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
            "h264_nvenc" => {
                opts.set("forced_idr", "1");
                opts.set("preset", "p5");
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
            }
            "h264_amf" => {
                opts.set("usage", "ultralowlatency");
                opts.set("quality", "speed");
            }
            "h264_qsv" => {
                opts.set("preset", "veryfast");
                opts.set("forced_idr", "1");
            }
            "h264_mf" => {
                opts.set("rate_control", "cbr");
                opts.set("scenario", "display_remoting");
                opts.set("hw_encoding", "1");
            }
            _ => {
                // h264_vaapi — use defaults
            }
        }

        let encoder = context
            .open_with(opts)
            .map_err(|e| format!("Open encoder: {}", e))?;

        let input_fmt = if supports_bgra_input { "BGRA (GPU convert)" } else { "NV12" };
        eprintln!("[encoder] H.264 encoder opened: {} — {}x{} @ {}fps, {}kbps, input={}", codec_name, config.width, config.height, config.fps, config.bitrate_kbps, input_fmt);

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

    /// Find the best available hardware H.264 encoder.
    fn find_hw_encoder() -> Result<(ffmpeg_next::Codec, String), String> {
        let candidates = if cfg!(target_os = "linux") {
            vec!["h264_nvenc", "h264_vaapi"]
        } else {
            vec!["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"]
        };

        for name in &candidates {
            if let Some(codec) = ffmpeg_next::encoder::find_by_name(name) {
                log::info!("Using H.264 encoder: {}", name);
                return Ok((codec, name.to_string()));
            }
        }
        Err("No hardware H.264 encoder found. Install NVIDIA drivers (NVENC) or ensure VA-API/Media Foundation is available.".to_string())
    }

    /// Initialize CUDA hardware frame encoding for NVENC.
    /// After this, `encode_cuda_frame()` accepts CUdeviceptr from GPU memory.
    #[cfg(target_os = "linux")]
    pub fn init_cuda_hw(&mut self) -> Result<(), String> {
        use ffmpeg_next::sys::*;

        unsafe {
            // Create CUDA hw_device_ctx
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

            // Point the CUDA frame's data[0] to our CUdeviceptr
            (*frame).data[0] = dev_ptr as *mut u8;
            let bpp: u32 = if self.supports_bgra_input { 4 } else { 1 }; // BGRA=4, NV12 Y plane=1
            (*frame).linesize[0] = (width * bpp) as libc::c_int;

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

    /// Receive one encoded packet and convert to AVCC format.
    fn receive_one_packet(&mut self) -> Option<EncodedFrame> {
        let mut packet = ffmpeg_next::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let annexb_data = packet.data().unwrap_or(&[]).to_vec();
                let is_keyframe = packet.is_key();
                let avcc_data = annexb_to_avcc(&annexb_data);
                let avcc_description = if is_keyframe {
                    let desc = extract_avcc_description(&annexb_data);
                    if desc.is_some() {
                        eprintln!("[encoder] Keyframe avcC description extracted ({} bytes)", desc.as_ref().unwrap().len());
                    } else {
                        eprintln!("[encoder] WARNING: Keyframe missing SPS/PPS!");
                    }
                    desc
                } else {
                    None
                };
                Some(EncodedFrame {
                    data: avcc_data,
                    is_keyframe,
                    pts: packet.pts().unwrap_or(0) as u64,
                    avcc_description,
                })
            }
            Err(_) => None,
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
                let annexb_data = data.to_vec();
                let is_keyframe = packet.is_key();
                let avcc_data = annexb_to_avcc(&annexb_data);
                let avcc_description = if is_keyframe {
                    extract_avcc_description(&annexb_data)
                } else {
                    None
                };
                frames.push(EncodedFrame {
                    data: avcc_data,
                    is_keyframe,
                    pts: packet.pts().unwrap_or(0) as u64,
                    avcc_description,
                });
            }
        }
        frames
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
