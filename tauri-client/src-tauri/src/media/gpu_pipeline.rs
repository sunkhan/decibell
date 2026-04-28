//! GPU-only sender pipeline for Windows: capture → BGRA→NV12 → NVENC.
//! No GPU↔CPU readback (except the periodic 5s thumbnail tick). Single
//! thread runs the whole loop and consumes the same VideoPipelineControl
//! / VideoPipelineEvent surface as the CPU pipeline so VideoEngine can
//! treat both paths uniformly.

#![cfg(target_os = "windows")]

use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Foundation::HMODULE;

use super::bitrate_preset;
use super::capture_dxgi;
use super::capture_wgc;
use super::caps::CodecKind;
use super::codec_selection::{CodecSelector, SwapEvent, SwapReason};
use super::encoder::{EncoderConfig, H264Encoder};
use super::gpu_capture::{CaptureError, GpuCaptureSource};
use super::thumbnail_reader::ThumbnailReader;
use super::video_packet::{UdpFecPacket, UdpVideoPacket, FEC_GROUP_SIZE, UDP_MAX_PAYLOAD};
use super::video_pipeline::{StreamerContext, VideoPipelineControl, VideoPipelineEvent};

pub struct GpuStreamingPipeline {
    // Hold the device + context so they outlive any encoder-internal refs.
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    capture: Box<dyn GpuCaptureSource>,
    converter: super::video_processor::VideoProcessor,
    encoder: H264Encoder,
    thumb_reader: Option<ThumbnailReader>,
    /// Encoder output dims — track separately so codec swap can decide
    /// whether to rebuild the VideoProcessor (dims changed) or only
    /// the encoder (codec only).
    enc_width: u32,
    enc_height: u32,
    /// Source dims as reported by the capture surface; required to rebuild
    /// the VideoProcessor on a swap that changes encoder dims.
    src_width: u32,
    src_height: u32,
}

// Safety: built on the caller's thread, immediately moved into the dedicated
// pipeline thread, then never touched from anywhere else. Internal handles
// (D3D11 device/context, NVENC encoder, capture source) are all single-thread.
// H264Encoder embeds a SwsContext which isn't auto-Send, but the GPU path
// never invokes sws_scale — we still need this to satisfy thread::spawn.
unsafe impl Send for GpuStreamingPipeline {}

impl GpuStreamingPipeline {
    /// Build the pipeline. Returns Err if any D3D11 / NVENC step fails;
    /// caller should fall back to the CPU path on Err. If config.width or
    /// config.height is 0, capture-source native dims substitute in.
    pub fn build(
        target_codec: CodecKind,
        source_id: &str,
        mut config: EncoderConfig,
    ) -> Result<Self, String> {
        // Source IDs from capture::list_sources():
        //   "monitor:{adapter_idx}:{output_idx}"  → DxgiSource
        //   "window:{hwnd}"                       → WgcSource
        let (capture, context, device): (Box<dyn GpuCaptureSource>, _, _) =
            if let Some(rest) = source_id.strip_prefix("monitor:") {
                let mut parts = rest.splitn(2, ':');
                let adapter_idx: u32 = parts
                    .next().and_then(|s| s.parse().ok())
                    .ok_or("monitor source ID malformed (adapter_idx)")?;
                let output_idx: u32 = parts
                    .next().and_then(|s| s.parse().ok())
                    .ok_or("monitor source ID malformed (output_idx)")?;

                let factory: IDXGIFactory1 = unsafe {
                    CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?
                };
                let adapter: IDXGIAdapter1 = unsafe {
                    factory.EnumAdapters1(adapter_idx)
                        .map_err(|e| format!("EnumAdapters1: {}", e))?
                };
                let (device, context) = capture_dxgi::create_device_for_adapter(&adapter)?;
                let src = capture_dxgi::DxgiSource::new(adapter_idx, output_idx, &device)?;
                (Box::new(src) as Box<dyn GpuCaptureSource>, context, device)
            } else if source_id.starts_with("window:") {
                let mut device: Option<ID3D11Device> = None;
                let mut context: Option<ID3D11DeviceContext> = None;
                let mut actual_level = D3D_FEATURE_LEVEL_11_0;
                unsafe {
                    D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_HARDWARE,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        Some(&[D3D_FEATURE_LEVEL_11_0]),
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        Some(&mut actual_level),
                        Some(&mut context),
                    )
                    .map_err(|e| format!("D3D11CreateDevice: {}", e))?;
                }
                let device = device.ok_or("D3D11CreateDevice returned None")?;
                let context = context.ok_or("D3D11CreateDevice context None")?;
                let src = capture_wgc::WgcSource::new(source_id, &device)?;
                (Box::new(src) as Box<dyn GpuCaptureSource>, context, device)
            } else {
                return Err(format!("Unknown source ID prefix: {}", source_id));
            };

        let src_w = capture.width();
        let src_h = capture.height();

        // "source" resolution from the UI: caller passes 0 → use capture native.
        if config.width == 0 { config.width = src_w; }
        if config.height == 0 { config.height = src_h; }

        let converter = super::video_processor::VideoProcessor::new(
            &device, src_w, src_h, config.width, config.height,
        )
        .map_err(|e| format!("VideoProcessor::new: {}", e))?;

        let device_raw = device.as_raw() as *mut std::ffi::c_void;
        let encoder = H264Encoder::new_d3d11(target_codec, &config, device_raw)
            .map_err(|e| format!("H264Encoder::new_d3d11: {}", e))?;

        // Thumbnail readback is best-effort; pipeline continues even if init fails.
        let thumb_reader = match ThumbnailReader::new(&device, src_w, src_h) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("[gpu-pipeline] ThumbnailReader::new failed (continuing without thumbnails): {}", e);
                None
            }
        };

        let enc_width = config.width;
        let enc_height = config.height;
        Ok(GpuStreamingPipeline {
            device,
            context,
            capture,
            converter,
            encoder,
            thumb_reader,
            enc_width,
            enc_height,
            src_width: src_w,
            src_height: src_h,
        })
    }

    /// Encoder output width after build (resolves "source" → native).
    pub fn effective_width(&self) -> u32 { self.enc_width }
    /// Encoder output height after build.
    pub fn effective_height(&self) -> u32 { self.enc_height }

    /// Run the capture→convert→encode loop. Drives off control_rx for
    /// keyframe / self-preview / shutdown, swap_rx for codec
    /// renegotiation, and emits frames + events through event_tx /
    /// the UDP socket. Mirrors the CPU pipeline's surface so a single
    /// VideoEngine can host either implementation.
    pub fn run(
        mut self,
        control_rx: mpsc::Receiver<VideoPipelineControl>,
        event_tx: mpsc::Sender<VideoPipelineEvent>,
        socket: Arc<UdpSocket>,
        sender_id: String,
        ctx: Option<StreamerContext>,
        community_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        jwt: Option<String>,
        mut swap_rx: Option<tokio::sync::mpsc::UnboundedReceiver<SwapEvent>>,
        selector: Option<Arc<CodecSelector>>,
    ) {
        let _ = event_tx.send(VideoPipelineEvent::Started);
        eprintln!("[gpu-pipeline] started, output {}x{}", self.enc_width, self.enc_height);

        let mut frame_id: u32 = 0;
        let mut self_preview = false;
        let mut shutdown_requested = false;

        let thumbnail_interval = Duration::from_secs(5);
        let mut last_thumbnail_time = Instant::now() - thumbnail_interval;

        loop {
            // ── Control messages ──
            match control_rx.try_recv() {
                Ok(VideoPipelineControl::Shutdown) => { shutdown_requested = true; break; }
                Ok(VideoPipelineControl::ForceKeyframe) => {
                    self.encoder.force_keyframe();
                }
                Ok(VideoPipelineControl::SetSelfPreview(v)) => {
                    eprintln!("[gpu-pipeline] SetSelfPreview({})", v);
                    self_preview = v;
                    if v { self.encoder.force_keyframe(); }
                }
                Err(mpsc::TryRecvError::Disconnected) => { shutdown_requested = true; break; }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            // ── Codec swap events ──
            if let Some(rx) = swap_rx.as_mut() {
                match rx.try_recv() {
                    Ok(swap) => {
                        if let (Some(c), Some(sel)) = (ctx.as_ref(), selector.as_ref()) {
                            match self.perform_swap(&swap, c, sel.as_ref(),
                                                    community_write_tx.as_ref(), jwt.as_deref()) {
                                Ok(()) => {}
                                Err(e) => eprintln!("[gpu-codec-swap] failed: {}", e),
                            }
                        }
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        swap_rx = None;
                    }
                }
            }

            // ── Capture next frame ──
            let frame = match self.capture.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => continue,
                Err(CaptureError::AccessLost) => {
                    let _ = event_tx.send(VideoPipelineEvent::Error("DXGI access lost".into()));
                    break;
                }
                Err(CaptureError::Disconnected) => {
                    eprintln!("[gpu-pipeline] capture source disconnected");
                    let _ = event_tx.send(VideoPipelineEvent::CaptureEnded);
                    return;
                }
                Err(CaptureError::Other(msg)) => {
                    let _ = event_tx.send(VideoPipelineEvent::Error(format!("Capture: {}", msg)));
                    break;
                }
                Err(CaptureError::Timeout) => continue,
            };

            // ── Thumbnail tick (every 5s, before release_current_frame) ──
            let now = Instant::now();
            if now.duration_since(last_thumbnail_time) >= thumbnail_interval {
                last_thumbnail_time = now;
                if let Some(ref reader) = self.thumb_reader {
                    match reader.capture_jpeg(&self.context, &frame.texture) {
                        Ok(jpeg) => {
                            eprintln!("[gpu-pipeline] thumbnail {} bytes", jpeg.len());
                            let _ = event_tx.send(VideoPipelineEvent::ThumbnailReady(jpeg));
                        }
                        Err(e) => eprintln!("[gpu-pipeline] thumbnail failed: {}", e),
                    }
                }
            }

            // ── Acquire pool frame, blit BGRA→NV12, submit ──
            let pool_frame = match self.encoder.acquire_pool_frame() {
                Ok(f) => f,
                Err(e) => {
                    self.capture.release_current_frame();
                    let _ = event_tx.send(VideoPipelineEvent::Error(format!("acquire_pool_frame: {}", e)));
                    break;
                }
            };
            let nv12_tex = pool_frame.texture();
            if let Err(e) = self.converter.blit_into(&self.context, &frame.texture, &nv12_tex) {
                self.capture.release_current_frame();
                let _ = event_tx.send(VideoPipelineEvent::Error(format!("blit_into: {}", e)));
                break;
            }
            drop(nv12_tex);
            self.capture.release_current_frame();

            match self.encoder.encode_d3d11_frame(pool_frame) {
                Ok(Some(encoded)) => {
                    if self_preview {
                        let _ = event_tx.send(VideoPipelineEvent::EncodedFrame {
                            data: encoded.data.clone(),
                            is_keyframe: encoded.is_keyframe,
                            frame_id,
                            codec: self.encoder.codec as u8,
                            description: encoded.avcc_description.clone(),
                        });
                    }
                    Self::send_packets(&socket, &sender_id, frame_id,
                                       &encoded, self.encoder.codec as u8);
                    frame_id = frame_id.wrapping_add(1);
                }
                Ok(None) => {} // encoder still buffering
                Err(e) => {
                    let _ = event_tx.send(VideoPipelineEvent::Error(format!("encode_d3d11_frame: {}", e)));
                    break;
                }
            }
        }

        // Flush remaining buffered frames (best-effort; FEC dropped on flush).
        for encoded in self.encoder.flush() {
            Self::send_packets(&socket, &sender_id, frame_id,
                               &encoded, self.encoder.codec as u8);
            frame_id = frame_id.wrapping_add(1);
        }

        if shutdown_requested {
            let _ = event_tx.send(VideoPipelineEvent::Stopped);
        }
        eprintln!("[gpu-pipeline] exit");
    }

    /// Packetize + UDP-send + emit FEC for one EncodedFrame. Mirrors the
    /// CPU pipeline's inline send loop in video_pipeline.rs, kept in
    /// sync with the same FEC group size + pacing.
    fn send_packets(
        socket: &UdpSocket,
        sender_id: &str,
        frame_id: u32,
        encoded: &super::encoder::EncodedFrame,
        codec_byte: u8,
    ) {
        let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;

        let mut fec_payload = [0u8; UDP_MAX_PAYLOAD];
        let mut fec_size_xor: u16 = 0;
        let mut fec_group_start: u16 = 0;
        let mut fec_group_count: u16 = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = UdpVideoPacket::new_with_codec(
                sender_id, frame_id, i as u16, total,
                encoded.is_keyframe, codec_byte, chunk,
            );
            let _ = socket.send(&pkt.to_bytes());

            for (j, &b) in chunk.iter().enumerate() { fec_payload[j] ^= b; }
            fec_size_xor ^= chunk.len() as u16;
            fec_group_count += 1;

            if fec_group_count == FEC_GROUP_SIZE {
                let fec_pkt = UdpFecPacket::new(
                    sender_id, frame_id, fec_group_start,
                    fec_group_count, fec_size_xor, &fec_payload,
                );
                let _ = socket.send(&fec_pkt.to_bytes());
                fec_payload = [0u8; UDP_MAX_PAYLOAD];
                fec_size_xor = 0;
                fec_group_start = i as u16 + 1;
                fec_group_count = 0;
            }

            // Pace large frames to avoid bursting the server's UDP buffer.
            if total > 10 && i % 10 == 9 {
                std::thread::sleep(Duration::from_micros(500));
            }
        }

        if fec_group_count > 1 {
            let fec_pkt = UdpFecPacket::new(
                sender_id, frame_id, fec_group_start,
                fec_group_count, fec_size_xor, &fec_payload,
            );
            let _ = socket.send(&fec_pkt.to_bytes());
        }
    }

    /// Plan C swap on the GPU path: build a fresh encoder for the new codec
    /// + dimensions, rebuild VideoProcessor if dims changed, send
    /// StreamCodecChangedNotify, atomically replace, force a keyframe so
    /// viewers can configure their decoder. Capture stays — it's BGRA at
    /// source dims regardless of encoder choice.
    fn perform_swap(
        &mut self,
        swap: &SwapEvent,
        ctx: &StreamerContext,
        selector: &CodecSelector,
        community_write_tx: Option<&tokio::sync::mpsc::Sender<Vec<u8>>>,
        jwt: Option<&str>,
    ) -> Result<(), String> {
        use crate::net::connection::build_packet;
        use crate::net::proto::{packet, StreamCodecChangedNotify};

        eprintln!("[gpu-codec-swap] {:?} → {:?} ({:?}) at {}x{}@{}",
            self.encoder.codec, swap.target.codec, swap.reason,
            swap.target.width, swap.target.height, swap.target.fps);

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

        // Rebuild VideoProcessor only when output dims changed; otherwise
        // the existing one still maps source → new encoder dims correctly.
        let dims_changed = new_config.width != self.enc_width
                        || new_config.height != self.enc_height;
        let new_converter = if dims_changed {
            Some(super::video_processor::VideoProcessor::new(
                &self.device, self.src_width, self.src_height,
                new_config.width, new_config.height,
            ).map_err(|e| format!("VideoProcessor rebuild: {}", e))?)
        } else { None };

        let device_raw = self.device.as_raw() as *mut std::ffi::c_void;
        let new_encoder = H264Encoder::new_d3d11(swap.target.codec, &new_config, device_raw)
            .map_err(|e| format!("rebuild encoder: {}", e))?;

        // Notify community server BEFORE swap so toast lines up with the
        // visual transition (matches CPU path semantics).
        if let (Some(tx), Some(jwt)) = (community_write_tx, jwt) {
            let reason_int = match swap.reason {
                SwapReason::WatcherJoinedLowCaps => 1,
                SwapReason::LimitingWatcherLeft => 2,
                SwapReason::StreamerInitiated => 3,
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
            let _ = tx.try_send(data);
        }

        if let Some(c) = new_converter { self.converter = c; }
        self.encoder = new_encoder;
        self.enc_width = new_config.width;
        self.enc_height = new_config.height;
        self.encoder.force_keyframe();
        selector.record_swap(swap.target);
        Ok(())
    }
}
