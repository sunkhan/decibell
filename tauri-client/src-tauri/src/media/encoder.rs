/// H.264 hardware encoder using FFmpeg's C API via ffmpeg-next.
pub struct H264Encoder {
    encoder: ffmpeg_next::encoder::Video,
    scaler: Option<ffmpeg_next::software::scaling::Context>,
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
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts: u64,
}

impl H264Encoder {
    /// Create a new H.264 hardware encoder.
    /// Tries hardware encoders in order: NVENC, VA-API (Linux), AMF/QSV (Windows).
    pub fn new(config: &EncoderConfig) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let codec = Self::find_hw_encoder()?;
        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("Encoder context: {}", e))?;

        context.set_width(config.width);
        context.set_height(config.height);
        context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
        context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_format(ffmpeg_next::format::Pixel::NV12);
        context.set_gop(config.fps * config.keyframe_interval_secs);

        let encoder = context
            .open()
            .map_err(|e| format!("Open encoder: {}", e))?;

        Ok(H264Encoder {
            encoder,
            scaler: None,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
        })
    }

    /// Find the best available hardware H.264 encoder.
    fn find_hw_encoder() -> Result<ffmpeg_next::Codec, String> {
        let candidates = if cfg!(target_os = "linux") {
            vec!["h264_nvenc", "h264_vaapi"]
        } else {
            vec!["h264_nvenc", "h264_amf", "h264_qsv"]
        };

        for name in &candidates {
            if let Some(codec) = ffmpeg_next::encoder::find_by_name(name) {
                log::info!("Using H.264 encoder: {}", name);
                return Ok(codec);
            }
        }
        Err("No hardware H.264 encoder found. Install NVIDIA drivers (NVENC) or ensure VA-API is available.".to_string())
    }

    /// Encode a raw BGRA frame into H.264 NAL units.
    /// Returns None if the encoder is buffering (no output yet).
    pub fn encode_frame(&mut self, bgra_data: &[u8], width: u32, height: u32) -> Result<Option<EncodedFrame>, String> {
        // Recreate scaler if source resolution changed
        if self.scaler.is_none() {
            self.scaler = Some(
                ffmpeg_next::software::scaling::Context::get(
                    ffmpeg_next::format::Pixel::BGRA,
                    width,
                    height,
                    ffmpeg_next::format::Pixel::NV12,
                    self.target_width,
                    self.target_height,
                    ffmpeg_next::software::scaling::Flags::BILINEAR,
                )
                .map_err(|e| format!("Failed to create scaler: {}", e))?,
            );
        }
        let scaler = self.scaler.as_mut().unwrap();

        let mut src_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::BGRA, width, height);
        src_frame.data_mut(0)[..bgra_data.len()].copy_from_slice(bgra_data);

        let mut nv12_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12,
            self.target_width,
            self.target_height,
        );
        scaler.run(&src_frame, &mut nv12_frame)
            .map_err(|e| format!("Scale frame: {}", e))?;

        nv12_frame.set_pts(Some(self.frame_count as i64));

        // Force keyframe at interval or on demand (PLI)
        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            nv12_frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        }

        self.frame_count += 1;

        self.encoder.send_frame(&nv12_frame)
            .map_err(|e| format!("Send frame: {}", e))?;

        let mut packet = ffmpeg_next::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let is_keyframe = packet.is_key();
                let data = packet.data().unwrap_or(&[]).to_vec();
                Ok(Some(EncodedFrame {
                    data,
                    is_keyframe,
                    pts: packet.pts().unwrap_or(0) as u64,
                }))
            }
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                Ok(None) // Encoder needs more input
            }
            Err(e) => Err(format!("Receive packet: {}", e)),
        }
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
                frames.push(EncodedFrame {
                    data: data.to_vec(),
                    is_keyframe: packet.is_key(),
                    pts: packet.pts().unwrap_or(0) as u64,
                });
            }
        }
        frames
    }
}
