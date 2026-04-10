//! H.264 decoder for Linux where WebKitGTK lacks WebCodecs VideoDecoder.
//! Decodes AVCC-formatted H.264 frames to JPEG for the frontend.
//!
//! Tries GPU-accelerated decode first (VAAPI for AMD/Intel, NVDEC/CUDA for NVIDIA),
//! falling back to software decode. All paths output JPEG via ffmpeg's MJPEG encoder.

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

pub struct H264Decoder {
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
unsafe impl Send for H264Decoder {}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        if !self.hw_device_ref.is_null() {
            unsafe {
                ffmpeg_next::sys::av_buffer_unref(&mut self.hw_device_ref);
            }
        }
    }
}

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {}", e))?;

        // Try GPU backends first, then software.
        // CUDA/NVDEC first — native path for NVIDIA GPUs (faster than VAAPI translation layer).
        if let Ok(dec) = Self::try_cuda() {
            return Ok(dec);
        }
        if let Ok(dec) = Self::try_vaapi() {
            return Ok(dec);
        }
        Self::try_software()
    }

    fn try_vaapi() -> Result<Self, String> {
        use ffmpeg_next::sys::*;

        let render_node = if std::path::Path::new("/dev/dri/renderD128").exists() {
            "/dev/dri/renderD128"
        } else if std::path::Path::new("/dev/dri/renderD129").exists() {
            "/dev/dri/renderD129"
        } else {
            return Err("No DRI render node found".into());
        };

        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264)
            .ok_or("H.264 decoder not found")?;

        unsafe {
            // Create VAAPI device context directly (simpler than DRM→derived for decode)
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

            // Create decoder context with hw_device_ctx set BEFORE opening
            let mut ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
            let raw = ctx.as_mut_ptr();
            (*raw).hw_device_ctx = av_buffer_ref(hw_device_ref);

            let decoder = ctx
                .decoder()
                .video()
                .map_err(|e| {
                    av_buffer_unref(&mut hw_device_ref);
                    format!("VAAPI decoder open: {}", e)
                })?;

            eprintln!("[video-decoder] VAAPI GPU decode ready ({})", render_node);

            Ok(H264Decoder {
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

    fn try_cuda() -> Result<Self, String> {
        use ffmpeg_next::sys::*;

        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264)
            .ok_or("H.264 decoder not found")?;

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

            let mut ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
            let raw = ctx.as_mut_ptr();
            (*raw).hw_device_ctx = av_buffer_ref(hw_device_ref);

            let decoder = ctx
                .decoder()
                .video()
                .map_err(|e| {
                    av_buffer_unref(&mut hw_device_ref);
                    format!("CUDA decoder open: {}", e)
                })?;

            eprintln!("[video-decoder] NVDEC/CUDA GPU decode ready");

            Ok(H264Decoder {
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

    fn try_software() -> Result<Self, String> {
        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264)
            .ok_or("H.264 decoder not found")?;

        let mut ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).thread_count = 2;
        }

        let decoder = ctx
            .decoder()
            .video()
            .map_err(|e| format!("Software decoder open: {}", e))?;

        eprintln!("[video-decoder] Software decode (no GPU acceleration available)");

        Ok(H264Decoder {
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
                "[video-decoder] First frame decoded: {}x{} using {} backend",
                w, h, self.backend
            );
        }

        // Scale to YUV420P (full range) for MJPEG encoder
        let src_format = cpu_frame.format();
        let need_new_scaler = self.scaler.is_none()
            || self.last_width != w
            || self.last_height != h;

        if need_new_scaler {
            self.scaler = ffmpeg_next::software::scaling::Context::get(
                src_format,
                w,
                h,
                ffmpeg_next::format::Pixel::YUV420P,
                w,
                h,
                ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
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

// Keep old name as alias for backward compatibility in mod.rs
pub type SoftwareH264Decoder = H264Decoder;
