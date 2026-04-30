use std::io::Cursor;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::capture::{PixelFormat, RawFrame};
use super::encoder::{EncoderConfig, H264Encoder};
use super::bitrate_preset::{self, Quality};
use super::codec_selection::{CodecSelector, SwapEvent, SwapReason};
use super::video_packet::{UdpFecPacket, UdpVideoPacket, FEC_GROUP_SIZE, UDP_MAX_PAYLOAD};

/// Plan C: pipeline-side context for codec swap operations. Holds the
/// values needed to (1) build the StreamCodecChangedNotify packet and
/// (2) recompute bitrate per codec when swapping.
#[derive(Clone, Debug)]
pub struct StreamerContext {
    pub channel_id: String,
    pub streamer_username: String,
    pub quality: Quality,
}

pub enum VideoPipelineControl {
    ForceKeyframe,
    Shutdown,
    /// Enable emitting EncodedFrame events so the streamer can watch their own preview.
    /// Disabled by default to avoid the base64/JSON cost when nobody is previewing.
    SetSelfPreview(bool),
}

pub enum VideoPipelineEvent {
    Started,
    Stopped,
    /// The capture source ended on its own (e.g. window was closed).
    CaptureEnded,
    Error(String),
    ThumbnailReady(Vec<u8>),
    /// Emitted for every successfully encoded frame, but only when self-preview
    /// is enabled (see `VideoPipelineControl::SetSelfPreview`).
    EncodedFrame {
        data: Vec<u8>,
        is_keyframe: bool,
        frame_id: u32,
        /// Codec byte from the active encoder (CodecKind as u8). Drives
        /// the WebCodecs decoder selection on the self-preview viewer.
        codec: u8,
        /// Decoder configuration record (avcC/hvcC/av1C) — present on
        /// keyframes only. Forwarded as-is to the WebCodecs description.
        description: Option<Vec<u8>>,
    },
}

/// Convert a raw frame to a small JPEG thumbnail.
/// Handles both NV12 (Windows) and BGRA/RGBA (Linux) input formats.
fn frame_to_jpeg_thumbnail(frame: &RawFrame) -> Option<Vec<u8>> {
    let w = frame.width as usize;
    let h = frame.height as usize;

    // Target thumbnail width ~320px, preserving aspect ratio
    let thumb_w = 320usize.min(w);
    let thumb_h = (thumb_w * h) / w;
    if thumb_w == 0 || thumb_h == 0 {
        return None;
    }

    let mut rgb = vec![0u8; thumb_w * thumb_h * 3];

    match frame.pixel_format {
        PixelFormat::NV12 => {
            let expected = w * h * 3 / 2;
            if frame.data.len() < expected {
                return None;
            }
            let y_plane = &frame.data[..w * h];
            let uv_plane = &frame.data[w * h..];

            for ty in 0..thumb_h {
                let sy = (ty * h) / thumb_h;
                let uv_row = sy / 2;
                for tx in 0..thumb_w {
                    let sx = (tx * w) / thumb_w;
                    let y_val = y_plane[sy * w + sx] as f32;
                    let uv_idx = uv_row * w + (sx & !1);
                    let u_val = uv_plane[uv_idx] as f32 - 128.0;
                    let v_val = uv_plane[uv_idx + 1] as f32 - 128.0;

                    let idx = (ty * thumb_w + tx) * 3;
                    rgb[idx]     = (y_val + 1.402 * v_val).clamp(0.0, 255.0) as u8;
                    rgb[idx + 1] = (y_val - 0.344 * u_val - 0.714 * v_val).clamp(0.0, 255.0) as u8;
                    rgb[idx + 2] = (y_val + 1.772 * u_val).clamp(0.0, 255.0) as u8;
                }
            }
        }
        PixelFormat::BGRA | PixelFormat::RGBA => {
            let stride = frame.stride;
            let is_bgra = frame.pixel_format == PixelFormat::BGRA;
            for ty in 0..thumb_h {
                let sy = (ty * h) / thumb_h;
                for tx in 0..thumb_w {
                    let sx = (tx * w) / thumb_w;
                    let src_idx = sy * stride + sx * 4;
                    let idx = (ty * thumb_w + tx) * 3;
                    if is_bgra {
                        rgb[idx]     = frame.data[src_idx + 2]; // R
                        rgb[idx + 1] = frame.data[src_idx + 1]; // G
                        rgb[idx + 2] = frame.data[src_idx];     // B
                    } else {
                        rgb[idx]     = frame.data[src_idx];     // R
                        rgb[idx + 1] = frame.data[src_idx + 1]; // G
                        rgb[idx + 2] = frame.data[src_idx + 2]; // B
                    }
                }
            }
        }
    }

    use image::ImageEncoder;
    let mut buf = Cursor::new(Vec::with_capacity(16 * 1024));
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 60);
    encoder
        .write_image(&rgb, thumb_w as u32, thumb_h as u32, image::ColorType::Rgb8.into())
        .ok()?;
    Some(buf.into_inner())
}

/// Run the video send pipeline on a dedicated thread.
/// Reads RawFrames from the channel, encodes them, packetizes, and sends via UDP.
///
/// IMPORTANT: `socket` must be the SAME UDP socket used by the voice audio pipeline.
/// The community server identifies senders by their UDP source address, which was
/// learned during voice connection. A different socket would have a different port
/// and the server would reject the packets.
pub fn run_video_send_pipeline(
    frame_rx: std::sync::mpsc::Receiver<RawFrame>,
    #[cfg(target_os = "linux")]
    gpu_frame_rx: Option<std::sync::mpsc::Receiver<super::capture::DmaBufFrame>>,
    control_rx: std::sync::mpsc::Receiver<VideoPipelineControl>,
    event_tx: std::sync::mpsc::Sender<VideoPipelineEvent>,
    socket: Arc<UdpSocket>,
    sender_id: String,
    config: EncoderConfig,
    target_fps: u32,
    // Initial codec for encoding. Plan B Task 7: passed through from
    // start_screen_share. Plan C: also the initial codec for the
    // CodecSelector; swap_rx fires when the LCD picker chooses different.
    target_codec: crate::media::caps::CodecKind,
    // Plan C: streamer context for building StreamCodecChangedNotify.
    // None = no codec negotiation (legacy path).
    ctx: Option<StreamerContext>,
    // Channel + JWT for sending the StreamCodecChangedNotify packet
    // when a swap completes.
    community_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    jwt: Option<String>,
    // Receiver of swap events from the CodecSelector. None = no negotiation.
    mut swap_rx: Option<tokio::sync::mpsc::UnboundedReceiver<SwapEvent>>,
    // Selector handle for record_swap callback. None = no negotiation.
    selector: Option<Arc<CodecSelector>>,
) {
    // GPU context is lazily initialized on first DMA-BUF frame (Linux only).
    // On NVIDIA+KWin, PipeWire provides MemFd (not DmaBuf), so GPU code never
    // runs and the pipeline behaves identically to the pre-DMA-BUF code path.
    // On AMD/Intel, DMA-BUF frames arrive and trigger GPU encoder init.
    #[cfg(target_os = "linux")]
    let mut gpu_ctx: Option<super::gpu_interop::GpuContext> = None;
    #[cfg(target_os = "linux")]
    let mut gpu_initialized = false;

    // Always start with the standard encoder (works for CPU/MemFd frames).
    // Replaced with a GPU encoder if DMA-BUF frames arrive.
    // Plan B: codec defaults to H264Hw; Plan B Task 7 plumbs the dev
    // force_codec parameter through here, Plan C plumbs the production
    // codec selection from the LCD picker.
    let mut encoder = match H264Encoder::new(target_codec, &config) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VideoPipelineEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(VideoPipelineEvent::Started);
    eprintln!("[video-send] Pipeline started, target {}fps", target_fps);

    let mut frame_id: u32 = 0;
    let mut last_frame_time = Instant::now();

    // Cache the last frame for re-sending during idle periods (e.g. PipeWire
    // damage-based capture sends nothing when the screen is static). Re-sending
    // at a low rate keeps keyframes flowing so new viewers can join.
    // Stored directly — no clone needed since the frame is moved from the channel.
    let mut last_frame: Option<RawFrame> = None;
    let repeat_interval = Duration::from_millis(500);
    let mut have_new_frame = false;

    // Thumbnail generation: every 5 seconds, encode a JPEG from the raw frame
    let thumbnail_interval = Duration::from_secs(5);
    let mut last_thumbnail_time = Instant::now() - thumbnail_interval; // generate first one immediately

    // Track whether the loop exits because of an explicit Shutdown command
    // vs the capture source ending on its own (frame channel disconnected).
    let mut shutdown_requested = false;

    let mut self_preview = false;

    loop {
        // Check control messages
        match control_rx.try_recv() {
            Ok(VideoPipelineControl::Shutdown) => { shutdown_requested = true; break; }
            Ok(VideoPipelineControl::ForceKeyframe) => {
                encoder.force_keyframe();
            }
            Ok(VideoPipelineControl::SetSelfPreview(v)) => {
                eprintln!("[self-preview] SetSelfPreview({})", v);
                self_preview = v;
                if v { encoder.force_keyframe(); }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => { shutdown_requested = true; break; }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        // Plan C: poll codec swap events from the selector. Non-blocking.
        if let Some(rx) = swap_rx.as_mut() {
            match rx.try_recv() {
                Ok(swap) => {
                    if let (Some(c), Some(sel)) = (ctx.as_ref(), selector.as_ref()) {
                        match perform_swap(&swap, c, sel.as_ref(), &mut encoder,
                                           community_write_tx.as_ref(), jwt.as_deref()) {
                            Ok(()) => {}
                            Err(e) => eprintln!("[codec-swap] failed: {}", e),
                        }
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    swap_rx = None; // selector dropped, give up polling
                }
            }
        }

        // ── Receive frame: try GPU channel first (Linux), then CPU channel ──
        let mut got_gpu_frame = false;
        #[cfg(target_os = "linux")]
        let mut gpu_frame_opt: Option<super::capture::DmaBufFrame> = None;

        #[cfg(target_os = "linux")]
        if let Some(ref gpu_rx) = gpu_frame_rx {
            match gpu_rx.try_recv() {
                Ok(gf) => {
                    gpu_frame_opt = Some(gf);
                    got_gpu_frame = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if !got_gpu_frame {
            // CPU frame path (RawFrame)
            match frame_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(f) => {
                    last_frame = Some(f);
                    have_new_frame = true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if last_frame_time.elapsed() >= repeat_interval && last_frame.is_some() {
                        have_new_frame = false;
                    } else {
                        continue;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if have_new_frame || got_gpu_frame {
            last_frame_time = Instant::now();
        }

        // ── Encode ──
        #[cfg(target_os = "linux")]
        let encode_result = if let Some(ref dmabuf) = gpu_frame_opt {
            // Lazy GPU init: only create GPU context on first DMA-BUF frame.
            // On NVIDIA (MemFd), this block never runs — no GPU overhead.
            if !gpu_initialized {
                gpu_initialized = true;
                gpu_ctx = super::gpu_interop::GpuContext::new();
                if let Some(ref mut gpu) = gpu_ctx {
                    match gpu.backend_type() {
                        super::gpu_interop::GpuBackendType::Vaapi => {
                            if let Err(e) = gpu.init_vaapi_frames(config.width, config.height) {
                                eprintln!("[video-send] VAAPI frames init failed: {}", e);
                                gpu_ctx = None;
                            } else {
                                match H264Encoder::new_vaapi(&config, gpu.vaapi_device_ref(), gpu.vaapi_frames_ref()) {
                                    Ok(e) => {
                                        eprintln!("[video-send] VA-API zero-copy encoding enabled");
                                        encoder = e;
                                    }
                                    Err(e) => {
                                        eprintln!("[video-send] VAAPI encoder failed ({}), CPU fallback", e);
                                        gpu_ctx = None;
                                    }
                                }
                            }
                        }
                        super::gpu_interop::GpuBackendType::Cuda => {
                            // Replace the encoder with one opened from scratch
                            // as pix_fmt=CUDA + hw_frames_ctx attached, sharing
                            // our CUcontext. The previous approach (open with
                            // BGRA then bolt on hw_frames_ctx afterwards) left
                            // the codec context expecting CPU memory, so
                            // feeding it CUDA frames failed with EINVAL.
                            let shared_ctx = gpu.cuda_ctx_raw();
                            match super::encoder::H264Encoder::new_cuda(&config, shared_ctx) {
                                Ok(e) => {
                                    eprintln!("[video-send] CUDA zero-copy encoding enabled");
                                    encoder = e;
                                }
                                Err(e) => {
                                    eprintln!("[video-send] CUDA encoder init failed ({}), CPU fallback", e);
                                    gpu_ctx = None;
                                }
                            }
                        }
                    }
                }
            }

            // GPU zero-copy encode
            if let Some(ref mut gpu) = gpu_ctx {
                use std::os::fd::AsRawFd;
                match gpu.backend_type() {
                    super::gpu_interop::GpuBackendType::Cuda => {
                        if encoder.has_cuda_hw() {
                            match gpu.import_dmabuf_cuda(
                                dmabuf.fd.as_raw_fd(),
                                dmabuf.width, dmabuf.height,
                                dmabuf.stride, dmabuf.drm_format, dmabuf.modifier,
                            ) {
                                Some(dev_ptr) => {
                                    gpu.push_cuda_ctx();
                                    let r = encoder.encode_cuda_frame(dev_ptr, dmabuf.width, dmabuf.height);
                                    gpu.pop_cuda_ctx();
                                    r
                                }
                                None => {
                                    eprintln!("[video-send] CUDA import failed, skipping frame");
                                    continue;
                                }
                            }
                        } else {
                            continue;
                        }
                    }
                    super::gpu_interop::GpuBackendType::Vaapi => {
                        match gpu.map_dmabuf_vaapi(
                            dmabuf.fd.as_raw_fd(),
                            dmabuf.width, dmabuf.height,
                            dmabuf.stride, dmabuf.drm_format, dmabuf.modifier,
                        ) {
                            Some(mut vaapi_frame) => {
                                encoder.encode_vaapi_frame(&mut vaapi_frame)
                            }
                            None => {
                                eprintln!("[video-send] VAAPI map failed, skipping frame");
                                continue;
                            }
                        }
                    }
                }
            } else {
                // GPU init failed — skip DMA-BUF frame, rely on CPU frames
                continue;
            }
        } else {
            // CPU encode path (RawFrame) — default path, also Linux MemFd fallback
            let frame = last_frame.as_ref().unwrap();

            // Generate thumbnail periodically
            let now_for_thumb = Instant::now();
            if now_for_thumb.duration_since(last_thumbnail_time) >= thumbnail_interval {
                last_thumbnail_time = now_for_thumb;
                if let Some(jpeg) = frame_to_jpeg_thumbnail(frame) {
                    eprintln!("[video-send] Thumbnail generated: {} bytes", jpeg.len());
                    let _ = event_tx.send(VideoPipelineEvent::ThumbnailReady(jpeg));
                }
            }

            match frame.pixel_format {
                PixelFormat::NV12 => {
                    encoder.encode_nv12_frame(&frame.data, frame.width, frame.height)
                }
                PixelFormat::BGRA | PixelFormat::RGBA => {
                    let is_bgra = frame.pixel_format == PixelFormat::BGRA;
                    encoder.encode_bgra_frame(
                        &frame.data, frame.width, frame.height, frame.stride, is_bgra,
                    )
                }
            }
        };

        // Windows: CPU-only encode path (unchanged)
        #[cfg(not(target_os = "linux"))]
        let encode_result = {
            let frame = last_frame.as_ref().unwrap();

            // Generate thumbnail periodically
            let now_for_thumb = Instant::now();
            if now_for_thumb.duration_since(last_thumbnail_time) >= thumbnail_interval {
                last_thumbnail_time = now_for_thumb;
                if let Some(jpeg) = frame_to_jpeg_thumbnail(frame) {
                    eprintln!("[video-send] Thumbnail generated: {} bytes", jpeg.len());
                    let _ = event_tx.send(VideoPipelineEvent::ThumbnailReady(jpeg));
                }
            }

            match frame.pixel_format {
                PixelFormat::NV12 => {
                    encoder.encode_nv12_frame(&frame.data, frame.width, frame.height)
                }
                PixelFormat::BGRA | PixelFormat::RGBA => {
                    let is_bgra = frame.pixel_format == PixelFormat::BGRA;
                    encoder.encode_bgra_frame(
                        &frame.data, frame.width, frame.height, frame.stride, is_bgra,
                    )
                }
            }
        };

        match encode_result {
            Ok(Some(encoded)) => {
                if self_preview {
                    if frame_id % 60 == 0 || encoded.is_keyframe {
                        eprintln!("[self-preview] Emitting frame {} ({} bytes, keyframe={})",
                            frame_id, encoded.data.len(), encoded.is_keyframe);
                    }
                    let _ = event_tx.send(VideoPipelineEvent::EncodedFrame {
                        data: encoded.data.clone(),
                        is_keyframe: encoded.is_keyframe,
                        frame_id,
                        codec: encoder.codec as u8,
                        description: encoded.avcc_description.clone(),
                    });
                }
                // Packetize: split encoded data into UDP_MAX_PAYLOAD-sized chunks.
                // For HEVC/AV1 keyframes the wire bytes carry a length-prefixed
                // hvcC / av1C ahead of the bitstream so receivers can configure
                // their WebCodecs decoder without parsing the bitstream
                // themselves — encoder::build_wire_data handles the prepend.
                let wire = super::encoder::build_wire_data(&encoded, encoder.codec);
                let chunks: Vec<&[u8]> = wire.chunks(UDP_MAX_PAYLOAD).collect();
                let total = chunks.len() as u16;

                if frame_id % 30 == 0 {
                    eprintln!("[video-send] Frame {} encoded: {} bytes ({} on wire), {} packets, keyframe={}",
                        frame_id, encoded.data.len(), wire.len(), total, encoded.is_keyframe);
                }

                // FEC accumulator: XOR payload buffer and payload_size for current group
                let mut fec_payload = [0u8; UDP_MAX_PAYLOAD];
                let mut fec_size_xor: u16 = 0;
                let mut fec_group_start: u16 = 0;
                let mut fec_group_count: u16 = 0;

                let mut send_ok = 0u32;
                let mut send_err = 0u32;
                let codec_byte = encoder.codec as u8;
                for (i, chunk) in chunks.iter().enumerate() {
                    let pkt = UdpVideoPacket::new_with_codec(
                        &sender_id,
                        frame_id,
                        i as u16,
                        total,
                        encoded.is_keyframe,
                        codec_byte,
                        chunk,
                    );
                    match socket.send(&pkt.to_bytes()) {
                        Ok(_) => send_ok += 1,
                        Err(e) => {
                            if send_err == 0 {
                                eprintln!("[video-send] UDP send error on pkt {}/{}: {}", i, total, e);
                            }
                            send_err += 1;
                        }
                    }

                    // Accumulate FEC: XOR this packet's payload (zero-padded) and size
                    for (j, &b) in chunk.iter().enumerate() {
                        fec_payload[j] ^= b;
                    }
                    fec_size_xor ^= chunk.len() as u16;
                    fec_group_count += 1;

                    // Emit FEC packet when group is full
                    if fec_group_count == FEC_GROUP_SIZE {
                        let fec_pkt = UdpFecPacket::new(
                            &sender_id, frame_id, fec_group_start,
                            fec_group_count, fec_size_xor, &fec_payload,
                        );
                        let _ = socket.send(&fec_pkt.to_bytes());
                        fec_payload = [0u8; UDP_MAX_PAYLOAD];
                        fec_size_xor = 0;
                        fec_group_start = i as u16 + 1;
                        fec_group_count = 0;
                    }

                    // Pace large frames to avoid overwhelming the server's UDP buffer.
                    if total > 10 && i % 10 == 9 {
                        std::thread::sleep(Duration::from_micros(500));
                    }
                }

                // Emit FEC for the trailing partial group (need at least 2 packets)
                if fec_group_count > 1 {
                    let fec_pkt = UdpFecPacket::new(
                        &sender_id, frame_id, fec_group_start,
                        fec_group_count, fec_size_xor, &fec_payload,
                    );
                    let _ = socket.send(&fec_pkt.to_bytes());
                }

                if encoded.is_keyframe || send_err > 0 {
                    eprintln!("[video-send] Frame {} sent: {}/{} ok, {} errors, keyframe={}",
                        frame_id, send_ok, total, send_err, encoded.is_keyframe);
                }
                frame_id = frame_id.wrapping_add(1);
            }
            Ok(None) => {} // Encoder still buffering (startup)
            Err(e) => {
                eprintln!("[video-send] Encode error: {}", e);
                let _ = event_tx.send(VideoPipelineEvent::Error(format!("Encode: {}", e)));
            }
        }
    }

    // Flush encoder (FEC not critical for final frames)
    for encoded in encoder.flush() {
        let wire = super::encoder::build_wire_data(&encoded, encoder.codec);
        let chunks: Vec<&[u8]> = wire.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;
        let codec_byte = encoder.codec as u8;
        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = UdpVideoPacket::new_with_codec(&sender_id, frame_id, i as u16, total, encoded.is_keyframe, codec_byte, chunk);
            let _ = socket.send(&pkt.to_bytes());
        }
        frame_id = frame_id.wrapping_add(1);
    }

    if shutdown_requested {
        let _ = event_tx.send(VideoPipelineEvent::Stopped);
    } else {
        eprintln!("[video-send] Capture source ended, signalling CaptureEnded");
        let _ = event_tx.send(VideoPipelineEvent::CaptureEnded);
    }
}

/// Plan C swap implementation: build a new encoder for the target codec
/// + dimensions, send StreamCodecChangedNotify to the community server
/// (which will rebroadcast presence + forward the notify for toasts),
/// atomically replace the active encoder, force a keyframe so viewers
/// can configure their decoder, and tell the selector the swap landed.
fn perform_swap(
    swap: &SwapEvent,
    ctx: &StreamerContext,
    selector: &CodecSelector,
    encoder: &mut H264Encoder,
    community_write_tx: Option<&tokio::sync::mpsc::Sender<Vec<u8>>>,
    jwt: Option<&str>,
) -> Result<(), String> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, Packet, StreamCodecChangedNotify};

    eprintln!("[codec-swap] {:?} → {:?} ({:?}) at {}x{}@{}",
        encoder.codec, swap.target.codec, swap.reason,
        swap.target.width, swap.target.height, swap.target.fps);

    // Codec-aware bitrate (spec §8) — recompute from preset table so
    // visual quality stays roughly comparable across the swap.
    let new_bitrate = bitrate_preset::bitrate_kbps(
        ctx.quality, swap.target.codec,
        swap.target.width, swap.target.height, swap.target.fps,
    );
    let new_config = EncoderConfig {
        width: swap.target.width,
        height: swap.target.height,
        fps: swap.target.fps,
        bitrate_kbps: new_bitrate,
        keyframe_interval_secs: 2,
    };

    // Build the new encoder OUT-OF-LOCK so the in-flight encoder isn't
    // disturbed during the (potentially slow) construction.
    let new_encoder = H264Encoder::new(swap.target.codec, &new_config)
        .map_err(|e| format!("build new encoder: {}", e))?;

    // Send the notify BEFORE swapping so toast appears just as the visual
    // transition begins (spec §6).
    if let (Some(tx), Some(jwt)) = (community_write_tx, jwt) {
        let reason_int = match swap.reason {
            SwapReason::WatcherJoinedLowCaps => 1, // Reason::WATCHER_JOINED_LOW_CAPS
            SwapReason::LimitingWatcherLeft => 2,  // Reason::LIMITING_WATCHER_LEFT
            SwapReason::StreamerInitiated => 3,    // Reason::STREAMER_INITIATED
        };
        let notify = StreamCodecChangedNotify {
            channel_id: ctx.channel_id.clone(),
            streamer_username: ctx.streamer_username.clone(),
            new_codec: swap.target.codec as i32,
            new_width: swap.target.width,
            new_height: swap.target.height,
            new_fps: swap.target.fps,
            reason: reason_int,
        };
        let data = build_packet(
            packet::Type::StreamCodecChangedNotify,
            packet::Payload::StreamCodecChangedNotify(notify),
            Some(jwt),
        );
        let _ = tx.try_send(data); // non-blocking; sender pool may be full briefly
        let _ = Packet::default(); // suppress unused-import warning on Packet
    }

    // Atomic swap. Old encoder dropped immediately when reassigned.
    *encoder = new_encoder;
    encoder.force_keyframe();
    selector.record_swap(swap.target);
    Ok(())
}

/// Public helper: split an EncodedFrame into UdpVideoPacket fragments and
/// send them on the given UDP socket. Used by the GPU zero-copy pipeline
/// (gpu_pipeline.rs) where the on_encoded callback gets an EncodedFrame
/// and needs to ship it via UDP without going through the existing
/// run_video_send_pipeline thread. Returns (sent_ok, sent_err) counts.
///
/// The CPU pipeline path (run_video_send_pipeline above) does its own
/// inline packetization with surrounding FEC/stats logic; that path is
/// unchanged. Eventually both paths could share this helper, but the
/// existing CPU code is intentionally not refactored here.
pub fn send_encoded_frame_as_packets(
    socket: &std::net::UdpSocket,
    sender_id: &str,
    frame_id: u32,
    encoded: &super::encoder::EncodedFrame,
    codec_byte: u8,
) -> (u32, u32) {
    use super::video_packet::{UdpVideoPacket, UDP_MAX_PAYLOAD};
    let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
    let total = chunks.len() as u16;
    let mut send_ok = 0u32;
    let mut send_err = 0u32;
    for (i, chunk) in chunks.iter().enumerate() {
        let pkt = UdpVideoPacket::new_with_codec(
            sender_id,
            frame_id,
            i as u16,
            total,
            encoded.is_keyframe,
            codec_byte,
            chunk,
        );
        match socket.send(&pkt.to_bytes()) {
            Ok(_) => send_ok += 1,
            Err(_) => send_err += 1,
        }
    }
    (send_ok, send_err)
}
