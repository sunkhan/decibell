//! Media engines.
//!
//! Three engines compose the streaming pipeline:
//!  - `VoiceEngine` owns the voice + media UDP sockets and the audio
//!    pipeline. The voice event bridge also forwards reassembled video
//!    frames coming off the media socket out to JS via the stream bus.
//!  - `VideoEngine` (PR8) is now a thin wrapper: the renderer drives
//!    `getDisplayMedia` + `WebCodecs.VideoEncoder` and ships encoded
//!    chunks to native via IPC; VideoEngine packetises them onto the
//!    media socket. No native capture, no native encoder, no FFmpeg.
//!  - `AudioStreamEngine` owns the share-audio send pipeline (platform
//!    capture → Opus encode → STREAM_AUDIO packets on the voice socket).
//!
//! Why the video send path moved to the renderer (PR8): the FFmpeg-in-
//! Rust path collided with Electron's bundled `libffmpeg.so`. Even with
//! `RTLD_DEEPBIND` to disambiguate `avcodec_*` symbols, having two
//! libavcodec/libavutil chains in one process produced allocator-
//! mismatch SIGSEGVs in unrelated voice code (cpal/PipeWire). Chromium's
//! `WebCodecs.VideoEncoder` lives inside the GPU process and avoids
//! the dual-FFmpeg situation entirely; it also gets us hardware H.264 +
//! AV1 encode (and HEVC where Chromium supports it) without us having
//! to maintain platform capture backends.

pub mod audio_device;
pub mod audio_stream_pipeline;
pub mod capture;
#[cfg(target_os = "linux")]
pub mod capture_audio_pipewire;
#[cfg(target_os = "windows")]
pub mod capture_audio_wasapi;
pub mod caps;
pub mod codec;
pub mod codec_selection;
pub mod jitter;
pub mod packet;
pub mod peer;
pub mod source_id;
#[cfg(target_os = "windows")]
pub mod encoder_probe;
#[cfg(target_os = "windows")]
pub mod gpu_pipeline;
#[cfg(target_os = "windows")]
pub mod video_processor;
#[cfg(target_os = "windows")]
pub mod bitrate_preset;
#[cfg(target_os = "windows")]
pub mod encoder;
#[cfg(target_os = "windows")]
pub mod capture_wgc;
#[cfg(target_os = "windows")]
pub mod encoder_thread;
#[cfg(target_os = "windows")]
pub mod thumbnail;
pub mod pipeline;
pub mod speaking;
pub mod video_packet;
pub mod video_pipeline;
pub mod video_receiver;

use std::net::UdpSocket;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use pipeline::{ControlMessage, VoiceEvent};

use crate::events;

/// VoiceEngine — owns both the voice UDP socket (server_port + 1) and
/// the media UDP socket (server_port + 2). The voice socket carries
/// AUDIO / STREAM_AUDIO / PING traffic; the media socket carries
/// VIDEO / FEC / KEYFRAME_REQUEST / NACK. A separate `decibell-video-recv`
/// thread reassembles incoming video frames and pushes them through the
/// shared event channel.
pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    video_recv_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    voice_socket: Arc<UdpSocket>,
    media_socket: Arc<UdpSocket>,
    sender_id: String,
    is_muted: bool,
    is_deafened: bool,
    was_muted_before_deafen: bool,
}

impl VoiceEngine {
    pub fn start(
        server_host: &str,
        server_port: u16,
        jwt: &str,
        voice_bitrate_bps: i32,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // Voice UDP port is server_port + 1, media UDP port is server_port + 2.
        let voice_udp_addr = format!("{}:{}", server_host, server_port + 1);
        let media_udp_addr = format!("{}:{}", server_host, server_port + 2);

        // Sender ID: last 31 chars of JWT (mirrors the server-side identity).
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        let voice_socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Voice UDP bind failed: {}", e))?;
        voice_socket
            .connect(&voice_udp_addr)
            .map_err(|e| format!("Voice UDP connect failed: {}", e))?;

        let media_socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Media UDP bind failed: {}", e))?;
        media_socket
            .connect(&media_udp_addr)
            .map_err(|e| format!("Media UDP connect failed: {}", e))?;

        configure_udp_socket(&voice_socket);
        configure_udp_socket(&media_socket);
        let voice_socket = Arc::new(voice_socket);
        let media_socket = Arc::new(media_socket);

        // Clone before move so the video recv thread can push into the
        // same event channel as the audio pipeline.
        let event_tx_video = event_tx.clone();

        let voice_socket_for_audio = voice_socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(
                    voice_socket_for_audio,
                    sender_id_for_audio,
                    voice_bitrate_bps,
                    control_rx,
                    event_tx,
                );
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;

        let media_socket_for_video_recv = media_socket.clone();
        let sender_id_for_video = sender_id.clone();
        let video_recv_thread = thread::Builder::new()
            .name("decibell-video-recv".to_string())
            .spawn(move || {
                run_video_recv_thread(
                    media_socket_for_video_recv,
                    sender_id_for_video,
                    event_tx_video,
                );
            })
            .map_err(|e| format!("Failed to spawn video recv thread: {}", e))?;

        // Voice + video event bridge. Runs on Tokio's blocking pool so it
        // never holds a worker thread (a previous `tokio::spawn` +
        // `block_in_place` pattern in tauri-client permanently stole
        // runtime workers).
        let event_bridge = tokio::task::spawn_blocking(move || loop {
            match event_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(event) => match event {
                    VoiceEvent::SpeakingChanged(username, speaking) => {
                        events::emit_voice_user_speaking(username, speaking);
                    }
                    VoiceEvent::UserStateChanged(username, is_muted, is_deafened) => {
                        events::emit_voice_user_state_changed(username, is_muted, is_deafened);
                    }
                    VoiceEvent::InputLevel(db) => {
                        events::emit_voice_input_level(db);
                    }
                    VoiceEvent::PingMeasured(ms) => {
                        events::emit_voice_ping_updated(ms);
                    }
                    VoiceEvent::ConnectionStats {
                        latency_ms,
                        packet_loss_pct,
                    } => {
                        events::emit_voice_connection_stats(latency_ms, packet_loss_pct);
                    }
                    VoiceEvent::VideoFrameReady(frame) => {
                        // PR8: WebCodecs encoders set `description` on
                        // keyframes (hvcC / av1C — for H.264, avcC if the
                        // encoder is configured AVCC, otherwise inline
                        // SPS/PPS in Annex B). Receiver-side
                        // strip_keyframe_description fills frame.description
                        // when the sender prefixed it with the magic tag;
                        // otherwise None and the renderer's
                        // WebCodecs.VideoDecoder reads from the bitstream.
                        // No more native avcC fallback — encoder.rs is gone.
                        events::send_stream_frame(events::StreamFrame {
                            username: frame.streamer_username,
                            codec: frame.codec,
                            keyframe: frame.is_keyframe,
                            timestamp: (frame.frame_id as i64) * 33_333,
                            data: frame.data,
                            description: frame.description,
                        });
                    }
                    VoiceEvent::KeyframeRequested => {
                        // PR8: forward to the renderer instead of the
                        // (deleted) native encoder. StreamCapture's
                        // listener calls VideoEncoder.encode(frame, {keyFrame: true})
                        // on the next capture frame.
                        events::send("keyframe_requested", serde_json::Value::Null);
                    }
                    VoiceEvent::Error(msg) => {
                        events::emit_voice_error(msg);
                    }
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        });

        Ok(VoiceEngine {
            audio_thread: Some(audio_thread),
            video_recv_thread: Some(video_recv_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            voice_socket,
            media_socket,
            sender_id,
            is_muted: false,
            is_deafened: false,
            was_muted_before_deafen: false,
        })
    }

    pub fn stop(&mut self) {
        let _ = self.control_tx.send(ControlMessage::Shutdown);
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
        }
        // Video recv thread exits when its socket read errors out after
        // the underlying socket is dropped.
        if let Some(handle) = self.video_recv_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.event_bridge.take() {
            handle.abort();
        }
    }

    pub fn set_mute(&mut self, muted: bool) {
        self.is_muted = muted;
        let _ = self.control_tx.send(ControlMessage::SetMute(muted));
    }

    pub fn set_deafen(&mut self, deafened: bool) {
        self.is_deafened = deafened;
        let _ = self.control_tx.send(ControlMessage::SetDeafen(deafened));
        if deafened {
            self.was_muted_before_deafen = self.is_muted;
            self.is_muted = true;
        } else {
            self.is_muted = self.was_muted_before_deafen;
        }
    }

    pub fn set_voice_threshold(&self, db: f32) {
        let _ = self.control_tx.send(ControlMessage::SetVoiceThreshold(db));
    }

    pub fn set_stream_volume(&self, volume: f32) {
        let _ = self.control_tx.send(ControlMessage::SetStreamVolume(volume));
    }

    pub fn set_stream_stereo(&self, enabled: bool) {
        let _ = self.control_tx.send(ControlMessage::SetStreamStereo(enabled));
    }

    pub fn set_input_device(&self, name: Option<String>) {
        let _ = self.control_tx.send(ControlMessage::SetInputDevice(name));
    }

    pub fn set_output_device(&self, name: Option<String>) {
        let _ = self.control_tx.send(ControlMessage::SetOutputDevice(name));
    }

    pub fn set_separate_stream_output(&self, enabled: bool, device: Option<String>) {
        let _ = self.control_tx
            .send(ControlMessage::SetSeparateStreamOutput(enabled, device));
    }

    pub fn set_stream_output_device(&self, name: Option<String>) {
        let _ = self.control_tx
            .send(ControlMessage::SetStreamOutputDevice(name));
    }

    pub fn set_user_volume(&self, username: String, gain: f32) {
        let _ = self.control_tx.send(ControlMessage::SetUserVolume(username, gain));
    }

    pub fn set_aec_enabled(&self, enabled: bool) {
        let _ = self.control_tx.send(ControlMessage::SetAecEnabled(enabled));
    }

    pub fn set_noise_suppression_level(&self, level: u8) {
        let _ = self.control_tx
            .send(ControlMessage::SetNoiseSuppressionLevel(level));
    }

    pub fn set_agc_enabled(&self, enabled: bool) {
        let _ = self.control_tx.send(ControlMessage::SetAgcEnabled(enabled));
    }

    pub fn is_muted(&self) -> bool {
        self.is_muted
    }

    pub fn is_deafened(&self) -> bool {
        self.is_deafened
    }

    pub fn muted_before_deafen(&self) -> bool {
        self.was_muted_before_deafen
    }

    pub fn set_muted_before_deafen(&mut self, v: bool) {
        self.was_muted_before_deafen = v;
    }

    pub fn voice_socket(&self) -> Arc<UdpSocket> {
        self.voice_socket.clone()
    }

    pub fn media_socket(&self) -> Arc<UdpSocket> {
        self.media_socket.clone()
    }

    pub fn sender_id(&self) -> &str {
        &self.sender_id
    }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Dedicated thread for video packet reassembly + NACK/PLI bookkeeping.
/// Reads directly from the media UDP socket — VIDEO, FEC,
/// KEYFRAME_REQUEST, NACK. Reassembled frames flow back to the voice
/// event bridge via `event_tx`.
fn run_video_recv_thread(
    socket: Arc<UdpSocket>,
    sender_id: String,
    event_tx: mpsc::Sender<pipeline::VoiceEvent>,
) {
    use std::time::{Duration, Instant};
    use video_packet::{UdpKeyframeRequest, UdpNackPacket, UdpVideoPacket};
    use video_receiver::VideoReceiver;

    let mut video_receiver = VideoReceiver::new();
    let mut video_streamer_username: Option<String> = None;
    let mut last_pli_time = Instant::now() - Duration::from_secs(10);
    let mut has_received_keyframe = false;
    let mut video_frames_received: u64 = 0;
    let mut last_maintenance = Instant::now();

    // Periodic media-socket PING — registers + refreshes this client's
    // media UDP endpoint on the server. Without this, pure watchers
    // (who never send video) stay unregistered on the server's media
    // side and receive no relayed frames. Fire one immediately, then
    // every 3s matching the voice PING cadence.
    let mut last_media_ping = Instant::now() - Duration::from_secs(10);
    let media_ping_interval = Duration::from_secs(3);

    // For HEVC/AV1 keyframes the sender prepends a magic-tagged
    // length-prefixed hvcC / av1C blob:
    //   [MAGIC][u32 BE: desc_len][desc][bitstream]
    // Strip it here so frame.data carries only the bitstream and
    // frame.description carries the WebCodecs decoder configuration
    // record. H.264 keyframes don't use this — the bridge derives avcC
    // from inline SPS/PPS NALs.
    let strip_keyframe_description =
        |mut frame: video_receiver::ReassembledFrame| -> video_receiver::ReassembledFrame {
            let magic = &video_packet::WIRE_DESCRIPTION_MAGIC;
            if frame.is_keyframe
                && (frame.codec == 3 || frame.codec == 4)
                && frame.data.len() >= magic.len() + 4
                && &frame.data[..magic.len()] == magic.as_slice()
            {
                let len_off = magic.len();
                let len = u32::from_be_bytes([
                    frame.data[len_off],
                    frame.data[len_off + 1],
                    frame.data[len_off + 2],
                    frame.data[len_off + 3],
                ]) as usize;
                let payload_off = len_off + 4;
                if len > 0 && len < 1024 && frame.data.len() >= payload_off + len {
                    frame.description = Some(frame.data[payload_off..payload_off + len].to_vec());
                    frame.data.drain(..payload_off + len);
                }
            }
            frame
        };
    let emit_frame = |frame: video_receiver::ReassembledFrame| {
        let frame = strip_keyframe_description(frame);
        let _ = event_tx.send(pipeline::VoiceEvent::VideoFrameReady(frame));
    };

    let _ = socket.set_read_timeout(Some(Duration::from_millis(5)));
    let mut recv_buf = [0u8; std::mem::size_of::<UdpVideoPacket>()];

    loop {
        match socket.recv(&mut recv_buf) {
            Ok(n) if n >= 1 => {
                let packet_type = recv_buf[0];

                if packet_type == video_packet::PACKET_TYPE_VIDEO {
                    if let Some(pkt) = UdpVideoPacket::from_bytes(&recv_buf[..n]) {
                        let username = pkt.sender_username();
                        if video_streamer_username.as_deref() != Some(&username) {
                            has_received_keyframe = false;
                        }
                        video_streamer_username = Some(username.clone());
                        if let Some(frame) = video_receiver.process_packet(&pkt) {
                            video_frames_received += 1;
                            if frame.is_keyframe || video_frames_received % 300 == 1 {
                                eprintln!(
                                    "[video-recv] Frame {} reassembled: {} bytes, keyframe={} (total={})",
                                    frame.frame_id,
                                    frame.data.len(),
                                    frame.is_keyframe,
                                    video_frames_received
                                );
                            }
                            if frame.is_keyframe {
                                has_received_keyframe = true;
                            }
                            emit_frame(frame);
                        }
                    }
                } else if packet_type == video_packet::PACKET_TYPE_FEC {
                    let header_size = std::mem::size_of::<video_packet::UdpFecPacket>()
                        - video_packet::UDP_MAX_PAYLOAD;
                    if n >= header_size {
                        let fec_pkt = unsafe {
                            let mut pkt: video_packet::UdpFecPacket = std::mem::zeroed();
                            let copy_len = n.min(std::mem::size_of::<video_packet::UdpFecPacket>());
                            std::ptr::copy_nonoverlapping(
                                recv_buf.as_ptr(),
                                &mut pkt as *mut video_packet::UdpFecPacket as *mut u8,
                                copy_len,
                            );
                            pkt
                        };
                        if let Some(frame) = video_receiver.process_fec_packet(&fec_pkt) {
                            video_frames_received += 1;
                            eprintln!(
                                "[video-recv] Frame {} completed via FEC: {} bytes, keyframe={}",
                                frame.frame_id, frame.data.len(), frame.is_keyframe
                            );
                            if frame.is_keyframe {
                                has_received_keyframe = true;
                            }
                            emit_frame(frame);
                        }
                    }
                } else if packet_type == video_packet::PACKET_TYPE_KEYFRAME_REQUEST
                    && n >= std::mem::size_of::<UdpKeyframeRequest>()
                {
                    eprintln!("[video-recv] Keyframe request received, signaling encoder");
                    let _ = event_tx.send(pipeline::VoiceEvent::KeyframeRequested);
                }
            }
            Ok(_) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::ConnectionReset
                    || e.raw_os_error() == Some(997)
                    || e.raw_os_error() == Some(10054) => {}
            Err(e) => {
                eprintln!("[video-recv] Socket error: {}", e);
                break;
            }
        }

        if last_media_ping.elapsed() >= media_ping_interval {
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let ping = packet::UdpAudioPacket::new_ping(&sender_id, ts_ns);
            let _ = socket.send(&ping.to_bytes());
            last_media_ping = Instant::now();
        }

        if last_maintenance.elapsed() > Duration::from_millis(100) {
            if let Some(ref target) = video_streamer_username {
                let (nacks, need_pli) = video_receiver.check_missing();
                for (frame_id, missing) in &nacks {
                    let nack_pkt = UdpNackPacket::new(&sender_id, target, *frame_id, missing);
                    let _ = socket.send(&nack_pkt.to_bytes());
                }
                let pli_interval = if has_received_keyframe {
                    Duration::from_secs(1)
                } else {
                    Duration::from_millis(500)
                };
                if need_pli && last_pli_time.elapsed() > pli_interval {
                    eprintln!(
                        "[video-recv] Sending keyframe request (PLI) to '{}' (has_keyframe={})",
                        target, has_received_keyframe
                    );
                    let pli_pkt = UdpKeyframeRequest::new(&sender_id, target);
                    let _ = socket.send(&pli_pkt.to_bytes());
                    last_pli_time = Instant::now();
                }
            }
            video_receiver.cleanup_stale();
            last_maintenance = Instant::now();
        }
    }
}

/// VideoEngine (PR8) — thin send-side wrapper. The renderer drives
/// capture + encode via Chromium's `getDisplayMedia` +
/// `WebCodecs.VideoEncoder` and ships encoded chunks through the
/// `send_video_frame` napi command, which forwards them here for
/// packetisation onto the media UDP socket. No native capture, no
/// native encoder, no codec selector inside the engine — Plan C
/// codec negotiation now lives in the renderer too (CodecSelector
/// reads watcher caps from the voice presence event and asks the
/// VideoEncoder to reconfigure when the LCD changes).
pub struct VideoEngine {
    sender: Arc<video_pipeline::VideoSender>,
    self_username: String,
    /// JS-side encoder may emit an `EncodedFrame` self-preview pump too
    /// — but the local self-preview is just the raw VideoFrame painted
    /// to a canvas in the StreamCapture component, so the receive-side
    /// stream_frame fanout only carries remote streams now.
    _phantom: std::marker::PhantomData<()>,
    /// Windows-only: native capture loop, encoder thread, and a handle
    /// to the encoder's force-keyframe AtomicBool. None on Linux/macOS
    /// (those platforms keep the renderer-encoded path).
    #[cfg(target_os = "windows")]
    win_capture: Option<capture_wgc::Capture>,
    #[cfg(target_os = "windows")]
    win_encoder_thread: Option<encoder_thread::EncoderThread>,
    #[cfg(target_os = "windows")]
    win_force_keyframe: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Tokio task that drains thumbnail JPEGs from the encoder thread
    /// and ships them to the community server. Aborted on stop_windows.
    #[cfg(target_os = "windows")]
    win_thumbnail_task: Option<tokio::task::JoinHandle<()>>,
}

impl VideoEngine {
    /// Start a streaming session. PR8 keeps this trivial: just record
    /// who we are so log output can mention it, and clone a sender for
    /// packetisation. Codec selection + capture + encode live entirely
    /// renderer-side.
    pub fn start(
        socket: Arc<UdpSocket>,
        sender_id: String,
        self_username: String,
    ) -> Self {
        let sender = Arc::new(video_pipeline::VideoSender::new(socket, sender_id));
        // Publish the sender to the hot-path slot so `send_video_frame`
        // can reach it without taking the AppState mutex on every
        // encoded chunk. Cleared on stop/drop below.
        video_pipeline::set_frame_sink(sender.clone());
        VideoEngine {
            sender,
            self_username,
            _phantom: std::marker::PhantomData,
            #[cfg(target_os = "windows")]
            win_capture: None,
            #[cfg(target_os = "windows")]
            win_encoder_thread: None,
            #[cfg(target_os = "windows")]
            win_force_keyframe: None,
            #[cfg(target_os = "windows")]
            win_thumbnail_task: None,
        }
    }

    /// Windows-only: spin up the native capture + encoder pipeline.
    /// Source id is the Chromium desktopCapturer id ("screen:N:0" or
    /// "window:HWND:0"). Encoder name comes from probe_native_encoders.
    /// Returns (width, height) — same shape the renderer-encoded path
    /// returned, so JS can keep its existing announcement to the server.
    #[cfg(target_os = "windows")]
    pub fn start_windows(
        &mut self,
        source_id: &str,
        encoder_name: &str,
        codec_wire_byte: u8,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        server_id: String,
        channel_id: String,
    ) -> Result<(u32, u32), String> {
        let target = source_id::parse(source_id)
            .map_err(|e| format!("source id '{}': {:?}", source_id, e))?;
        let gpu = gpu_pipeline::GpuDevice::create()?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<
            windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        >(2);

        // Thumbnail channel: encoder thread try_sends JPEGs here every
        // 3s; tokio task below drains and ships them to the community
        // server. Depth=1 + try_send means a slow server drops the new
        // thumb instead of back-pressuring the encoder.
        let (thumb_tx, mut thumb_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let thumb_server_id = server_id.clone();
        let thumb_channel_id = channel_id.clone();
        let thumb_task = tokio::spawn(async move {
            while let Some(jpeg) = thumb_rx.recv().await {
                let state_arc = crate::state::shared();
                let s = state_arc.lock().await;
                let Some(client) = s.communities.get(&thumb_server_id) else {
                    continue;
                };
                if let Err(e) =
                    client.send_stream_thumbnail(&thumb_channel_id, &jpeg).await
                {
                    log::warn!("[encoder/thumb] server send failed: {e}");
                }
            }
        });

        let capture = capture_wgc::Capture::start(&gpu, target, tx)?;
        let encoder_thread = encoder_thread::EncoderThread::start(
            gpu,
            encoder_thread::EncoderThreadConfig {
                encoder_name: encoder_name.to_string(),
                codec_wire_byte,
                width,
                height,
                fps,
                bitrate_kbps,
                local_username: self.self_username.clone(),
                video_sender: self.sender.clone(),
                thumbnail_tx: thumb_tx,
            },
            rx,
        )?;
        self.win_force_keyframe = Some(encoder_thread.force_keyframe_handle());
        self.win_capture = Some(capture);
        self.win_encoder_thread = Some(encoder_thread);
        self.win_thumbnail_task = Some(thumb_task);
        // server_id + channel_id were consumed into the tokio task above;
        // silence unused warnings if the task is the only consumer.
        let _ = (server_id, channel_id);
        Ok((width, height))
    }

    /// Windows-only: stop the native pipeline. Joins both threads.
    #[cfg(target_os = "windows")]
    pub fn stop_windows(&mut self) {
        if let Some(c) = self.win_capture.take() {
            c.stop();
        }
        if let Some(e) = self.win_encoder_thread.take() {
            e.stop();
        }
        // Encoder thread drop releases the thumbnail Sender, which
        // makes thumb_rx.recv() return None and the tokio task exits
        // naturally. abort() is belt-and-suspenders in case the task
        // is stuck inside an in-flight server send when stop fires.
        if let Some(h) = self.win_thumbnail_task.take() {
            h.abort();
        }
        self.win_force_keyframe = None;
    }

    /// Windows-only: nudge the encoder thread to emit a keyframe on the
    /// next frame. Wired from force_keyframe napi command.
    #[cfg(target_os = "windows")]
    pub fn request_keyframe(&self) {
        if let Some(flag) = &self.win_force_keyframe {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Send a single encoded chunk produced by the renderer's
    /// `WebCodecs.VideoEncoder`. The renderer-side `StreamCapture`
    /// also paints a self-preview locally from raw `VideoFrame`s, so
    /// we don't fan out a self-preview through the stream bus.
    pub fn send_encoded_frame(
        &self,
        codec_byte: u8,
        is_keyframe: bool,
        data: &[u8],
    ) -> (u32, u32) {
        self.sender.send_frame(codec_byte, is_keyframe, data)
    }

    pub fn self_username(&self) -> &str {
        &self.self_username
    }

    pub fn stop(&mut self) {
        // Windows native pipeline: stop capture + encoder threads.
        // Linux/macOS keep the renderer-encoded path; nothing to tear
        // down there (StreamCapture handles it renderer-side). We DO
        // need to clear the hot-path frame sink so a stray post-stop
        // frame from the renderer (rare but possible during shutdown
        // ordering) drops on the floor instead of being packetised
        // onto a dead socket.
        #[cfg(target_os = "windows")]
        self.stop_windows();
        video_pipeline::clear_frame_sink();
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// AudioStreamEngine — owns the share-audio capture + encode + send
/// pipeline. Frames travel as STREAM_AUDIO packets on the voice UDP
/// socket (not the media socket — the server demuxes by packet type).
pub struct AudioStreamEngine {
    pipeline_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<audio_stream_pipeline::AudioStreamControl>,
    /// Linux PipeWire path returns a cleanup closure to restore default
    /// audio routing on stop. None when no cleanup is needed.
    #[cfg(target_os = "linux")]
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl AudioStreamEngine {
    pub fn start(
        frame_rx: mpsc::Receiver<capture::AudioFrame>,
        socket: Arc<UdpSocket>,
        sender_id: String,
        bitrate_kbps: u32,
        #[cfg(target_os = "linux")] cleanup: Option<Box<dyn FnOnce() + Send>>,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let pipeline_thread = thread::Builder::new()
            .name("decibell-stream-audio".to_string())
            .spawn(move || {
                audio_stream_pipeline::run_audio_stream_pipeline(
                    frame_rx,
                    control_rx,
                    event_tx,
                    socket,
                    sender_id,
                    bitrate_kbps,
                );
            })
            .expect("spawn audio stream pipeline thread");

        let event_bridge = tokio::task::spawn_blocking(move || loop {
            match event_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(audio_stream_pipeline::AudioStreamEvent::Error(msg)) => {
                    events::emit_voice_error(format!("Stream audio: {}", msg));
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        });

        AudioStreamEngine {
            pipeline_thread: Some(pipeline_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            #[cfg(target_os = "linux")]
            cleanup,
        }
    }

    pub fn stop(&mut self) {
        let _ = self
            .control_tx
            .send(audio_stream_pipeline::AudioStreamControl::Shutdown);
        if let Some(h) = self.pipeline_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.event_bridge.take() {
            h.abort();
        }
        #[cfg(target_os = "linux")]
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

impl Drop for AudioStreamEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Apply 4 MB send/recv buffers + DSCP EF (expedited forwarding)
/// markings to the UDP socket so voice/media packets get priority
/// queueing on a quality-of-service-aware network. Cross-platform:
/// setsockopt on Unix, WSAIoctl + setsockopt on Windows.
/// SIO_UDP_CONNRESET is also disabled on Windows so a bogus ICMP
/// unreachable from one peer doesn't tear down the entire socket.
fn configure_udp_socket(socket: &UdpSocket) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = socket.as_raw_fd();
        let buf_size: libc::c_int = 4 * 1024 * 1024;
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            let dscp_ef: libc::c_int = 0xB8;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_TOS,
                &dscp_ef as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        let sock = socket.as_raw_socket();
        let buf_size: i32 = 4 * 1024 * 1024;
        unsafe {
            let _ = windows::Win32::Networking::WinSock::setsockopt(
                windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                windows::Win32::Networking::WinSock::SO_RCVBUF as i32,
                Some(std::slice::from_raw_parts(
                    &buf_size as *const i32 as *const u8,
                    std::mem::size_of::<i32>(),
                )),
            );
            let _ = windows::Win32::Networking::WinSock::setsockopt(
                windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                windows::Win32::Networking::WinSock::SO_SNDBUF as i32,
                Some(std::slice::from_raw_parts(
                    &buf_size as *const i32 as *const u8,
                    std::mem::size_of::<i32>(),
                )),
            );
            let dscp_ef: i32 = 0xB8;
            let _ = windows::Win32::Networking::WinSock::setsockopt(
                windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                windows::Win32::Networking::WinSock::IPPROTO_IP.0 as i32,
                windows::Win32::Networking::WinSock::IP_TOS as i32,
                Some(std::slice::from_raw_parts(
                    &dscp_ef as *const i32 as *const u8,
                    std::mem::size_of::<i32>(),
                )),
            );
            let wsa_sock = windows::Win32::Networking::WinSock::SOCKET(sock as usize);
            const SIO_UDP_CONNRESET: u32 = 0x9800000C;
            let false_val: u32 = 0;
            let mut bytes_returned: u32 = 0;
            let _ = windows::Win32::Networking::WinSock::WSAIoctl(
                wsa_sock,
                SIO_UDP_CONNRESET,
                Some(&false_val as *const u32 as *const std::ffi::c_void),
                std::mem::size_of::<u32>() as u32,
                None,
                0,
                &mut bytes_returned,
                None,
                None,
            );
        }
    }
}
