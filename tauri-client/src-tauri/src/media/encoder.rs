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

/// H.264 hardware encoder using FFmpeg's C API via ffmpeg-next.
pub struct H264Encoder {
    encoder: ffmpeg_next::encoder::Video,
    frame_count: u64,
    keyframe_interval: u64,
    force_next_keyframe: bool,
    target_width: u32,
    target_height: u32,
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
        context.set_format(ffmpeg_next::format::Pixel::NV12);
        context.set_gop(config.fps * config.keyframe_interval_secs);
        // Disable B-frames for real-time streaming — B-frames require reordering
        // which adds latency and can cause artifacts with simple decoders.
        context.set_max_b_frames(0);

        // Signal BT.709 colorspace in SPS VUI so the decoder uses the correct
        // YUV→RGB matrix. Without this, decoders may assume BT.601 and produce
        // a green tint on HD content.
        context.set_colorspace(ffmpeg_next::color::Space::BT709);
        context.set_color_range(ffmpeg_next::color::Range::MPEG);

        let mut opts = ffmpeg_next::Dictionary::new();
        match codec_name.as_str() {
            "h264_nvenc" => {
                opts.set("forced_idr", "1");
                opts.set("preset", "p4");
                opts.set("tune", "ull");
                // VBV buffer for smoother rate control — prevents quality drops during
                // high-motion transitions while keeping latency low.
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

        eprintln!("[encoder] H.264 encoder opened: {} — {}x{} @ {}fps, {}kbps", codec_name, config.width, config.height, config.fps, config.bitrate_kbps);

        Ok(H264Encoder {
            encoder,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
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
    /// NV12 data comes directly from GStreamer (which handles RGB→YUV with correct
    /// colorspace) and is NVENC's native format — no FFmpeg conversion needed.
    /// Returns None if the encoder is still buffering (startup only).
    pub fn encode_frame(&mut self, nv12_data: &[u8], width: u32, height: u32) -> Result<Option<EncodedFrame>, String> {
        let mut frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12,
            width,
            height,
        );

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

        frame.set_pts(Some(self.frame_count as i64));

        // Force keyframe at interval or on demand (PLI)
        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        }

        self.frame_count += 1;

        // Standard FFmpeg encode pattern: send frame, then try to receive output.
        // If send returns EAGAIN, receive one packet first to free a buffer, then retry.
        match self.encoder.send_frame(&frame) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                let _ = self.receive_one_packet();
                self.encoder.send_frame(&frame)
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
