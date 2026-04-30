pub mod audio_device;
pub mod audio_stream_pipeline;
pub mod bitrate_preset;
pub mod caps;
pub mod capture;
pub mod codec_selection;
#[cfg(target_os = "linux")]
pub mod capture_audio_pipewire;
#[cfg(target_os = "linux")]
pub mod capture_pipewire;
#[cfg(target_os = "linux")]
pub mod gpu_interop;
#[cfg(target_os = "windows")]
pub mod capture_audio_wasapi;
#[cfg(target_os = "windows")]
pub mod capture_wgc;
#[cfg(target_os = "windows")]
pub mod video_processor;
#[cfg(target_os = "windows")]
pub mod thumbnail_reader;
#[cfg(target_os = "windows")]
pub mod gpu_capture;
#[cfg(target_os = "windows")]
pub mod gpu_pipeline;
#[cfg(target_os = "windows")]
pub mod capture_dxgi;
pub mod codec;
pub mod encoder;
pub mod jitter;
pub mod packet;
pub mod peer;
pub mod pipeline;
pub mod speaking;
pub mod video_packet;
pub mod video_pipeline;
pub mod video_receiver;
#[cfg(target_os = "linux")]
pub mod video_decoder;

use std::net::UdpSocket;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use tauri::{AppHandle, Emitter};

use pipeline::{ControlMessage, VoiceEvent};

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
    /// Set when VideoEngine starts so the voice event bridge can forward
    /// keyframe requests without locking AppState.
    keyframe_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<video_pipeline::VideoPipelineControl>>>>,
}

impl VoiceEngine {
    pub fn start(
        server_host: &str,
        server_port: u16,
        jwt: &str,
        voice_bitrate_bps: i32,
        app: AppHandle,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // Voice UDP port is server_port + 1, media UDP port is server_port + 2
        let voice_udp_addr = format!("{}:{}", server_host, server_port + 1);
        let media_udp_addr = format!("{}:{}", server_host, server_port + 2);

        // Sender ID: last 31 chars of JWT
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        // Voice socket — carries AUDIO, STREAM_AUDIO, PING
        let voice_socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Voice UDP bind failed: {}", e))?;
        voice_socket
            .connect(&voice_udp_addr)
            .map_err(|e| format!("Voice UDP connect failed: {}", e))?;

        // Media socket — carries VIDEO, FEC, KEYFRAME_REQUEST, NACK
        let media_socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Media UDP bind failed: {}", e))?;
        media_socket
            .connect(&media_udp_addr)
            .map_err(|e| format!("Media UDP connect failed: {}", e))?;

        // Apply buffer sizes and DSCP to both sockets
        fn configure_udp_socket(socket: &UdpSocket) {
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                let fd = socket.as_raw_fd();
                let buf_size: libc::c_int = 4 * 1024 * 1024;
                unsafe {
                    libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF,
                        &buf_size as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                    libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF,
                        &buf_size as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                    let dscp_ef: libc::c_int = 0xB8;
                    libc::setsockopt(fd, libc::IPPROTO_IP, libc::IP_TOS,
                        &dscp_ef as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t);
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
                            &buf_size as *const i32 as *const u8, std::mem::size_of::<i32>(),
                        )),
                    );
                    let _ = windows::Win32::Networking::WinSock::setsockopt(
                        windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                        windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                        windows::Win32::Networking::WinSock::SO_SNDBUF as i32,
                        Some(std::slice::from_raw_parts(
                            &buf_size as *const i32 as *const u8, std::mem::size_of::<i32>(),
                        )),
                    );
                    let dscp_ef: i32 = 0xB8;
                    let _ = windows::Win32::Networking::WinSock::setsockopt(
                        windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                        windows::Win32::Networking::WinSock::IPPROTO_IP.0 as i32,
                        windows::Win32::Networking::WinSock::IP_TOS as i32,
                        Some(std::slice::from_raw_parts(
                            &dscp_ef as *const i32 as *const u8, std::mem::size_of::<i32>(),
                        )),
                    );
                    let wsa_sock = windows::Win32::Networking::WinSock::SOCKET(sock as usize);
                    const SIO_UDP_CONNRESET: u32 = 0x9800000C;
                    let mut false_val: u32 = 0;
                    let mut bytes_returned: u32 = 0;
                    let _ = windows::Win32::Networking::WinSock::WSAIoctl(
                        wsa_sock, SIO_UDP_CONNRESET,
                        Some(&false_val as *const u32 as *const std::ffi::c_void),
                        std::mem::size_of::<u32>() as u32, None, 0,
                        &mut bytes_returned, None, None,
                    );
                }
            }
        }
        configure_udp_socket(&voice_socket);
        configure_udp_socket(&media_socket);
        let voice_socket = Arc::new(voice_socket);
        let media_socket = Arc::new(media_socket);

        // Clone event_tx before it's moved into the audio thread — the video
        // recv thread needs its own sender into the same event channel.
        let event_tx_video = event_tx.clone();

        let voice_socket_for_audio = voice_socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(voice_socket_for_audio, sender_id_for_audio, voice_bitrate_bps, control_rx, event_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;

        // Dedicated video recv thread: reads from media socket, reassembles frames, sends NACKs/PLI
        let media_socket_for_video_recv = media_socket.clone();
        let sender_id_for_video = sender_id.clone();
        let video_recv_thread = thread::Builder::new()
            .name("decibell-video-recv".to_string())
            .spawn(move || {
                run_video_recv_thread(media_socket_for_video_recv, sender_id_for_video, event_tx_video);
            })
            .map_err(|e| format!("Failed to spawn video recv thread: {}", e))?;

        // Shared slot for forwarding keyframe requests to the video encoder.
        // Set by set_keyframe_sender() when VideoEngine starts.
        let keyframe_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<video_pipeline::VideoPipelineControl>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let keyframe_tx_for_bridge = keyframe_tx.clone();

        // Voice event bridge: runs on Tokio's blocking thread pool so it never
        // consumes a worker thread. Previous `tokio::spawn` + `block_in_place`
        // permanently stole a Tokio worker, starving the runtime over time.
        let event_bridge = tokio::task::spawn_blocking(move || {
            loop {
                match event_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(event) => match event {
                        VoiceEvent::SpeakingChanged(username, speaking) => {
                            let _ = app.emit("voice_user_speaking", serde_json::json!({
                                "username": username,
                                "speaking": speaking,
                            }));
                        }
                        VoiceEvent::UserStateChanged(username, is_muted, is_deafened) => {
                            let _ = app.emit("voice_user_state_changed", serde_json::json!({
                                "username": username,
                                "isMuted": is_muted,
                                "isDeafened": is_deafened,
                            }));
                        }
                        VoiceEvent::InputLevel(db) => {
                            let _ = app.emit("voice_input_level", serde_json::json!({
                                "db": db,
                            }));
                        }
                        VoiceEvent::PingMeasured(ms) => {
                            let _ = app.emit("voice_ping_updated", serde_json::json!({
                                "latencyMs": ms,
                            }));
                        }
                        VoiceEvent::VideoFrameReady(frame) => {
                            if frame.is_keyframe {
                                eprintln!("[video-bridge] Emitting keyframe: user='{}', {} bytes",
                                    frame.streamer_username, frame.data.len());
                            }
                            use base64::Engine;
                            let b64_data = base64::engine::general_purpose::STANDARD.encode(&frame.data);
                            // HEVC / AV1: receive thread already stripped the
                            // length-prefixed hvcC / av1C from the keyframe data
                            // and stashed it in frame.description. Use it as-is.
                            // H.264: build avcC by parsing the inline SPS/PPS
                            // NAL units from the keyframe bitstream (existing
                            // path; kept for back-compat with older clients
                            // that don't prepend a description on the wire).
                            let description = frame.description.clone().or_else(|| {
                                if frame.is_keyframe && frame.codec <= 2 {
                                    encoder::extract_avcc_description_from_avcc(&frame.data)
                                } else {
                                    None
                                }
                            });
                            let b64_desc = description.as_ref().map(|d| {
                                eprintln!("[video-bridge] description ({} bytes) for codec {}", d.len(), frame.codec);
                                base64::engine::general_purpose::STANDARD.encode(d)
                            });
                            let _ = app.emit("stream_frame", serde_json::json!({
                                "username": frame.streamer_username,
                                "format": "h264",
                                "data": b64_data,
                                "timestamp": frame.frame_id as u64 * 33_333,
                                "keyframe": frame.is_keyframe,
                                "description": b64_desc,
                                // Plan B: codec byte from the per-packet header. React reads
                                // this to pick the WebCodecs codec string (avc1/hev1/av01).
                                "codec": frame.codec,
                            }));
                        }
                        #[cfg(target_os = "linux")]
                        VoiceEvent::VideoFrameDecoded(username, jpeg_data, frame_id, is_keyframe) => {
                            use base64::Engine;
                            let b64_jpeg = base64::engine::general_purpose::STANDARD.encode(&jpeg_data);
                            let _ = app.emit("stream_frame", serde_json::json!({
                                "username": username,
                                "format": "jpeg",
                                "data": b64_jpeg,
                                "timestamp": frame_id as u64 * 33_333,
                                "keyframe": is_keyframe,
                            }));
                        }
                        VoiceEvent::KeyframeRequested => {
                            if let Ok(guard) = keyframe_tx_for_bridge.lock() {
                                if let Some(ref tx) = *guard {
                                    let _ = tx.send(video_pipeline::VideoPipelineControl::ForceKeyframe);
                                    eprintln!("[video-bridge] Keyframe request forwarded to encoder");
                                }
                            }
                        }
                        VoiceEvent::Error(msg) => {
                            let _ = app.emit("voice_error", serde_json::json!({
                                "message": msg,
                            }));
                        }
                    },
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
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
            keyframe_tx,
        })
    }

    pub fn stop(&mut self) {
        let _ = self.control_tx.send(ControlMessage::Shutdown);
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
        }
        // Video recv thread exits when its media socket read times out after the pipeline shuts down.
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
        let _ = self.control_tx.send(ControlMessage::SetSeparateStreamOutput(enabled, device));
    }

    pub fn set_stream_output_device(&self, name: Option<String>) {
        let _ = self.control_tx.send(ControlMessage::SetStreamOutputDevice(name));
    }

    pub fn set_user_volume(&self, username: String, gain: f32) {
        let _ = self.control_tx.send(ControlMessage::SetUserVolume(username, gain));
    }

    pub fn set_aec_enabled(&self, enabled: bool) {
        let _ = self.control_tx.send(ControlMessage::SetAecEnabled(enabled));
    }

    pub fn set_noise_suppression_level(&self, level: u8) {
        let _ = self.control_tx.send(ControlMessage::SetNoiseSuppressionLevel(level));
    }

    pub fn set_agc_enabled(&self, enabled: bool) {
        let _ = self.control_tx.send(ControlMessage::SetAgcEnabled(enabled));
    }

    /// Set the video encoder's control channel so keyframe requests from
    /// remote viewers can be forwarded without locking AppState.
    pub fn set_keyframe_sender(&self, tx: mpsc::Sender<video_pipeline::VideoPipelineControl>) {
        if let Ok(mut guard) = self.keyframe_tx.lock() {
            *guard = Some(tx);
        }
    }

    /// Clear the keyframe sender (when VideoEngine stops).
    pub fn clear_keyframe_sender(&self) {
        if let Ok(mut guard) = self.keyframe_tx.lock() {
            *guard = None;
        }
    }

    pub fn is_muted(&self) -> bool { self.is_muted }
    pub fn is_deafened(&self) -> bool { self.is_deafened }
    pub fn muted_before_deafen(&self) -> bool { self.was_muted_before_deafen }
    pub fn set_muted_before_deafen(&mut self, v: bool) { self.was_muted_before_deafen = v; }
    pub fn voice_socket(&self) -> Arc<UdpSocket> { self.voice_socket.clone() }
    pub fn media_socket(&self) -> Arc<UdpSocket> { self.media_socket.clone() }
    pub fn sender_id(&self) -> &str { &self.sender_id }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Dedicated thread for video packet reassembly, NACK/PLI.
/// Reads directly from the media UDP socket (VIDEO, FEC, KEYFRAME_REQUEST, NACK).
fn run_video_recv_thread(
    socket: Arc<UdpSocket>,
    sender_id: String,
    event_tx: std::sync::mpsc::Sender<pipeline::VoiceEvent>,
) {
    use video_packet::{UdpVideoPacket, UdpKeyframeRequest, UdpNackPacket};
    use video_receiver::VideoReceiver;
    use std::time::{Duration, Instant};

    let mut video_receiver = VideoReceiver::new();
    let mut video_streamer_username: Option<String> = None;
    let mut last_pli_time = Instant::now() - Duration::from_secs(10);
    let mut has_received_keyframe = false;
    let mut video_frames_received: u64 = 0;
    let mut last_maintenance = Instant::now();

    // Periodic media-socket PING so the server learns and refreshes this
    // client's media UDP endpoint. Without this, pure watchers (who never
    // send video themselves) stay unregistered on the server's media side
    // and receive no relayed frames. Fire one immediately, then every 3s
    // matching the voice PING cadence.
    let mut last_media_ping = Instant::now() - Duration::from_secs(10);
    let media_ping_interval = Duration::from_secs(3);

    // Linux: codec-aware decoder for frames (WebKitGTK lacks WebCodecs).
    // Rebuilt when the per-frame codec byte changes (Plan C swap mid-stream)
    // since each codec needs its own ffmpeg decoder + HW backend.
    #[cfg(target_os = "linux")]
    let mut linux_decoder: Option<video_decoder::VideoDecoder> = None;
    #[cfg(target_os = "linux")]
    let mut last_decode_time = Instant::now();

    // Helper: emit a reassembled video frame.
    // Linux: decode H.264 / HEVC / AV1 → JPEG, emit VideoFrameDecoded.
    // Windows: pass raw bitstream via VideoFrameReady for WebCodecs.
    //
    // For HEVC/AV1 keyframes the sender prepends a magic-tagged
    // length-prefixed hvcC / av1C blob to the bitstream:
    //   [MAGIC: encoder::WIRE_DESCRIPTION_MAGIC][u32 BE: desc_len][desc][bitstream]
    // We strip that here so frame.data contains only the actual codec
    // bitstream and frame.description carries the WebCodecs decoder
    // configuration record. H.264 keyframes don't use this — the
    // receiver builds avcC by parsing inline SPS/PPS NALs from the
    // bitstream (existing path, kept for back-compat).
    //
    // The magic prefix lets older senders (which don't prepend) coexist
    // with newer receivers — keyframes without the magic pass through
    // unchanged. The magic byte 0 (0xDE) can't collide with an HEVC
    // NAL length high byte at realistic frame sizes or an AV1 OBU
    // header byte (forbidden bit forces byte 0 < 0x80).
    let strip_keyframe_description = |mut frame: video_receiver::ReassembledFrame| -> video_receiver::ReassembledFrame {
        let magic = &encoder::WIRE_DESCRIPTION_MAGIC;
        // codec 3 = HEVC, 4 = AV1
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
    let mut emit_frame = |frame: video_receiver::ReassembledFrame| {
        let frame = strip_keyframe_description(frame);
        #[cfg(target_os = "linux")]
        {
            // Codec dispatch: convert per-frame codec byte to CodecKind.
            // If our active decoder doesn't match (first frame, or Plan C
            // mid-stream swap), drop and rebuild for the new codec.
            let frame_codec = match frame.codec {
                1 => caps::CodecKind::H264Hw,
                2 => caps::CodecKind::H264Sw,
                3 => caps::CodecKind::H265,
                4 => caps::CodecKind::Av1,
                _ => caps::CodecKind::H264Hw, // legacy frames pre-Plan-B default
            };
            let need_rebuild = match &linux_decoder {
                None => true,
                // H264Hw and H264Sw share the same ffmpeg decoder, so don't
                // rebuild between them (just a wire-codec metadata swap).
                Some(dec) => {
                    let active = dec.codec();
                    let cur_h264 = matches!(active, caps::CodecKind::H264Hw | caps::CodecKind::H264Sw);
                    let new_h264 = matches!(frame_codec, caps::CodecKind::H264Hw | caps::CodecKind::H264Sw);
                    !((cur_h264 && new_h264) || active == frame_codec)
                }
            };
            if need_rebuild {
                linux_decoder = None;
                match video_decoder::VideoDecoder::new(frame_codec) {
                    Ok(dec) => {
                        eprintln!("[video-recv] decoder built for {:?}", frame_codec);
                        linux_decoder = Some(dec);
                    }
                    Err(e) => eprintln!("[video-recv] failed to build {:?} decoder: {}", frame_codec, e),
                }
            }
            if let Some(ref mut dec) = linux_decoder {
                // AV1 keyframes need a Sequence Header OBU for ffmpeg to
                // configure the decoder. The streamer's encoder has
                // AV_CODEC_FLAG_GLOBAL_HEADER set for AV1 so the SH
                // lives in extradata, not inline in keyframes — on the
                // wire we ship it as the av1C in frame.description (its
                // configOBUs portion, bytes 4..end, IS the SH OBU).
                // Prepend it to the bitstream on AV1 keyframes so ffmpeg
                // sees the SH and configures itself. Idempotent across
                // subsequent keyframes; the decoder accepts redundant SH
                // OBUs without re-init.
                let prepended_owned;
                let bitstream: &[u8] = if frame.is_keyframe
                    && frame.codec == 4
                    && frame.description.as_ref().map(|d| d.len() > 4).unwrap_or(false)
                {
                    let desc = frame.description.as_ref().unwrap();
                    let mut combined = Vec::with_capacity((desc.len() - 4) + frame.data.len());
                    combined.extend_from_slice(&desc[4..]);
                    combined.extend_from_slice(&frame.data);
                    prepended_owned = combined;
                    &prepended_owned
                } else {
                    &frame.data
                };

                // Rate-limit emit: skip JPEG-encoding delta frames if the
                // last one went out <25ms ago (~40fps cap to the React side).
                // We still feed every frame into the decoder so references
                // stay correct.
                let elapsed = last_decode_time.elapsed();
                if !frame.is_keyframe && elapsed < Duration::from_millis(25) {
                    let _ = dec.decode_to_jpeg(bitstream);
                } else if let Some(jpeg) = dec.decode_to_jpeg(bitstream) {
                    last_decode_time = Instant::now();
                    let _ = event_tx.send(pipeline::VoiceEvent::VideoFrameDecoded(
                        frame.streamer_username,
                        jpeg,
                        frame.frame_id,
                        frame.is_keyframe,
                    ));
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = event_tx.send(pipeline::VoiceEvent::VideoFrameReady(frame));
        }
    };

    // Read directly from the media socket with a short timeout for periodic maintenance
    let _ = socket.set_read_timeout(Some(Duration::from_millis(5)));
    let mut recv_buf = [0u8; std::mem::size_of::<UdpVideoPacket>()];

    loop {
        // Read from media socket
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
                                eprintln!("[video-recv] Frame {} reassembled: {} bytes, keyframe={} (total={})",
                                    frame.frame_id, frame.data.len(), frame.is_keyframe, video_frames_received);
                            }
                            if frame.is_keyframe { has_received_keyframe = true; }
                            emit_frame(frame);
                        }
                    }
                } else if packet_type == video_packet::PACKET_TYPE_FEC {
                    let header_size = std::mem::size_of::<video_packet::UdpFecPacket>() - video_packet::UDP_MAX_PAYLOAD;
                    if n >= header_size {
                        // Parse FEC packet — safe to reinterpret since it's repr(C, packed)
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
                            eprintln!("[video-recv] Frame {} completed via FEC: {} bytes, keyframe={}",
                                frame.frame_id, frame.data.len(), frame.is_keyframe);
                            if frame.is_keyframe { has_received_keyframe = true; }
                            emit_frame(frame);
                        }
                    }
                } else if packet_type == video_packet::PACKET_TYPE_KEYFRAME_REQUEST && n >= std::mem::size_of::<UdpKeyframeRequest>() {
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
                    || e.raw_os_error() == Some(10054)
            => {}
            Err(e) => {
                eprintln!("[video-recv] Socket error: {}", e);
                break;
            }
        }

        // Periodic media-socket PING — registers/refreshes this client's media
        // endpoint on the server so video relay can reach us.
        if last_media_ping.elapsed() >= media_ping_interval {
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let ping = packet::UdpAudioPacket::new_ping(&sender_id, ts_ns);
            let _ = socket.send(&ping.to_bytes());
            last_media_ping = Instant::now();
        }

        // Periodic maintenance: NACKs, PLI, stale cleanup
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
                    eprintln!("[video-recv] Sending keyframe request (PLI) to '{}' (has_keyframe={})", target, has_received_keyframe);
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

pub struct VideoEngine {
    pipeline_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    pipeline_control_tx: mpsc::Sender<video_pipeline::VideoPipelineControl>,
}

impl VideoEngine {
    /// Start the video send pipeline: encode frames from capture and send via UDP.
    /// `thumbnail_write_tx` is a cloned mpsc sender for the community server connection,
    /// allowing the event bridge to send thumbnails without locking AppState.
    pub fn start(
        frame_rx: std::sync::mpsc::Receiver<capture::RawFrame>,
        #[cfg(target_os = "linux")]
        gpu_frame_rx: Option<std::sync::mpsc::Receiver<capture::DmaBufFrame>>,
        socket: Arc<UdpSocket>,
        sender_id: String,
        config: encoder::EncoderConfig,
        target_fps: u32,
        target_codec: caps::CodecKind,
        app: AppHandle,
        thumbnail_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        thumbnail_channel_id: Option<String>,
        self_username: String,
        // Plan C: codec negotiation context. When None, the pipeline runs
        // without a CodecSelector — used by paths that don't have a
        // matching community connection (defensive; production always
        // passes Some(...) from start_screen_share).
        ctx: Option<video_pipeline::StreamerContext>,
        // Channel for sending the StreamCodecChangedNotify packet to the
        // community server when a codec swap completes.
        community_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        // JWT for the community connection — needed to wrap the notify
        // packet with proper auth.
        jwt: Option<String>,
        // Plan C: streamer's current encode caps (post-toggle filtering)
        // and toggles, for the CodecSelector. When None, the selector is
        // not created and no auto-negotiation runs.
        encode_caps: Option<Vec<caps::CodecCap>>,
        toggles: Option<codec_selection::Toggles>,
        enforced_codec: Option<caps::CodecKind>,
        // Plan C: shared handles cloned from AppState so the pipeline
        // never re-locks AppState during operation.
        watcher_event_tx: tokio::sync::broadcast::Sender<crate::state::WatcherEvent>,
        voice_caps_cache: Arc<std::sync::RwLock<std::collections::HashMap<String, caps::PeerCaps>>>,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // ── Plan C: build the CodecSelector and spawn the watcher listener ──
        // The selector lives outside the pipeline thread; the pipeline
        // polls swap_rx for swap events. Watcher events come from the
        // community connection via a broadcast channel and feed the
        // selector inside a tokio task.
        let (swap_rx_for_pipeline, selector_for_pipeline) = if let (
            Some(c), Some(enc), Some(tog),
        ) = (ctx.as_ref(), encode_caps.as_ref(), toggles.as_ref()) {
            let initial = codec_selection::StreamSettings {
                codec: target_codec,
                width: config.width,
                height: config.height,
                fps: target_fps,
            };
            let (selector, swap_rx) = codec_selection::CodecSelector::new(
                initial, enforced_codec, enc.clone(), tog.clone(),
            );
            let selector = Arc::new(selector);

            // Spawn the watcher event listener — drives the selector.
            let mut watcher_rx = watcher_event_tx.subscribe();
            let selector_for_listener = selector.clone();
            let streamer_username = c.streamer_username.clone();
            tokio::spawn(async move {
                while let Ok(evt) = watcher_rx.recv().await {
                    if evt.streamer_username != streamer_username { continue; }
                    let watcher_decode = {
                        match voice_caps_cache.read() {
                            Ok(cache) => cache.get(&evt.watcher_username)
                                .map(|c| c.decode.clone()),
                            Err(_) => None,
                        }
                    };
                    let watcher_decode = match watcher_decode {
                        Some(d) => d,
                        None => {
                            eprintln!("[codec-listener] watcher caps unknown for {}", evt.watcher_username);
                            continue;
                        }
                    };
                    const JOINED: i32 = 1; // chatproj::StreamWatcherNotify::JOINED
                    const LEFT: i32 = 2;   // chatproj::StreamWatcherNotify::LEFT
                    match evt.action {
                        JOINED => selector_for_listener.on_watcher_joined(evt.watcher_username, watcher_decode),
                        LEFT => selector_for_listener.on_watcher_left(&evt.watcher_username),
                        _ => {}
                    }
                }
            });
            (Some(swap_rx), Some(selector))
        } else {
            (None, None)
        };

        let ctx_for_pipeline = ctx.clone();
        let community_write_tx_for_pipeline = community_write_tx.clone();
        let jwt_for_pipeline = jwt.clone();

        let pipeline_thread = thread::Builder::new()
            .name("decibell-video".to_string())
            .spawn(move || {
                video_pipeline::run_video_send_pipeline(
                    frame_rx,
                    #[cfg(target_os = "linux")]
                    gpu_frame_rx,
                    control_rx,
                    event_tx,
                    socket,
                    sender_id,
                    config,
                    target_fps,
                    target_codec,
                    ctx_for_pipeline,
                    community_write_tx_for_pipeline,
                    jwt_for_pipeline,
                    swap_rx_for_pipeline,
                    selector_for_pipeline,
                );
            })
            .expect("spawn video pipeline thread");

        // Bridge video pipeline events to Tauri — runs on the blocking pool
        // to avoid consuming a Tokio worker thread. Shared with start_gpu().
        let event_bridge = spawn_video_event_bridge(
            event_rx, app, thumbnail_write_tx, thumbnail_channel_id, self_username,
        );

        VideoEngine {
            pipeline_thread: Some(pipeline_thread),
            event_bridge: Some(event_bridge),
            pipeline_control_tx: control_tx,
        }
    }

    pub fn stop(&mut self) {
        let _ = self.pipeline_control_tx.send(video_pipeline::VideoPipelineControl::Shutdown);
        if let Some(h) = self.pipeline_thread.take() { let _ = h.join(); }
        if let Some(h) = self.event_bridge.take() { h.abort(); }
    }

    pub fn force_keyframe(&self) {
        let _ = self.pipeline_control_tx.send(video_pipeline::VideoPipelineControl::ForceKeyframe);
    }

    pub fn set_self_preview(&self, enabled: bool) {
        let _ = self.pipeline_control_tx.send(video_pipeline::VideoPipelineControl::SetSelfPreview(enabled));
    }

    /// Clone the pipeline control sender so external code can forward keyframe
    /// requests without holding a reference to VideoEngine.
    pub fn pipeline_control_tx(&self) -> mpsc::Sender<video_pipeline::VideoPipelineControl> {
        self.pipeline_control_tx.clone()
    }

    /// Windows-only: start the zero-copy GPU pipeline (capture + BGRA→NV12 +
    /// NVENC, all on the GPU). Returns Err on any D3D11/NVENC build step
    /// so caller can fall back to the CPU path. On success, the returned
    /// VideoEngine + (effective_width, effective_height) — needed because
    /// "source" resolution resolves to the capture surface's native dims
    /// only after the capture interface is opened.
    #[cfg(target_os = "windows")]
    pub fn start_gpu(
        source_id: String,
        target_codec: caps::CodecKind,
        config: encoder::EncoderConfig,
        target_fps: u32,
        socket: Arc<UdpSocket>,
        sender_id: String,
        app: AppHandle,
        thumbnail_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        thumbnail_channel_id: Option<String>,
        self_username: String,
        ctx: Option<video_pipeline::StreamerContext>,
        community_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        jwt: Option<String>,
        encode_caps: Option<Vec<caps::CodecCap>>,
        toggles: Option<codec_selection::Toggles>,
        enforced_codec: Option<caps::CodecKind>,
        watcher_event_tx: tokio::sync::broadcast::Sender<crate::state::WatcherEvent>,
        voice_caps_cache: Arc<std::sync::RwLock<std::collections::HashMap<String, caps::PeerCaps>>>,
    ) -> Result<(Self, u32, u32), String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // Build the pipeline first — failure here is the trigger for the
        // CPU fallback toast in start_screen_share.
        let pipeline = gpu_pipeline::GpuStreamingPipeline::build(
            target_codec, &source_id, config.clone(),
        )?;
        let eff_w = pipeline.effective_width();
        let eff_h = pipeline.effective_height();

        // ── Mirror VideoEngine::start's CodecSelector setup ──
        let (swap_rx_for_pipeline, selector_for_pipeline) = if let (
            Some(c), Some(enc), Some(tog),
        ) = (ctx.as_ref(), encode_caps.as_ref(), toggles.as_ref()) {
            let initial = codec_selection::StreamSettings {
                codec: target_codec,
                width: eff_w,
                height: eff_h,
                fps: target_fps,
            };
            let (selector, swap_rx) = codec_selection::CodecSelector::new(
                initial, enforced_codec, enc.clone(), tog.clone(),
            );
            let selector = Arc::new(selector);

            let mut watcher_rx = watcher_event_tx.subscribe();
            let selector_for_listener = selector.clone();
            let streamer_username = c.streamer_username.clone();
            tokio::spawn(async move {
                while let Ok(evt) = watcher_rx.recv().await {
                    if evt.streamer_username != streamer_username { continue; }
                    let watcher_decode = {
                        match voice_caps_cache.read() {
                            Ok(cache) => cache.get(&evt.watcher_username)
                                .map(|c| c.decode.clone()),
                            Err(_) => None,
                        }
                    };
                    let watcher_decode = match watcher_decode {
                        Some(d) => d,
                        None => continue,
                    };
                    const JOINED: i32 = 1;
                    const LEFT: i32 = 2;
                    match evt.action {
                        JOINED => selector_for_listener.on_watcher_joined(evt.watcher_username, watcher_decode),
                        LEFT => selector_for_listener.on_watcher_left(&evt.watcher_username),
                        _ => {}
                    }
                }
            });
            (Some(swap_rx), Some(selector))
        } else {
            (None, None)
        };

        let ctx_for_pipeline = ctx.clone();
        let community_write_tx_for_pipeline = community_write_tx.clone();
        let jwt_for_pipeline = jwt.clone();

        let pipeline_thread = thread::Builder::new()
            .name("decibell-video-gpu".to_string())
            .spawn(move || {
                pipeline.run(
                    control_rx,
                    event_tx,
                    socket,
                    sender_id,
                    ctx_for_pipeline,
                    community_write_tx_for_pipeline,
                    jwt_for_pipeline,
                    swap_rx_for_pipeline,
                    selector_for_pipeline,
                );
            })
            .map_err(|e| format!("spawn GPU pipeline thread: {}", e))?;

        let event_bridge = spawn_video_event_bridge(
            event_rx, app, thumbnail_write_tx, thumbnail_channel_id, self_username,
        );

        Ok((
            VideoEngine {
                pipeline_thread: Some(pipeline_thread),
                event_bridge: Some(event_bridge),
                pipeline_control_tx: control_tx,
            },
            eff_w,
            eff_h,
        ))
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the Tauri-side bridge that translates VideoPipelineEvents into
/// app.emit() calls + thumbnail packets. Runs on the blocking pool so
/// it never consumes a Tokio worker. Shared between VideoEngine::start
/// (CPU pipeline) and VideoEngine::start_gpu (Windows zero-copy path).
fn spawn_video_event_bridge(
    event_rx: mpsc::Receiver<video_pipeline::VideoPipelineEvent>,
    app: AppHandle,
    thumbnail_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    thumbnail_channel_id: Option<String>,
    self_username: String,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        loop {
            match event_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(video_pipeline::VideoPipelineEvent::CaptureEnded) => {
                    eprintln!("[video-engine] Capture source ended, emitting stream_capture_ended");
                    let _ = app.emit("stream_capture_ended", ());
                }
                Ok(video_pipeline::VideoPipelineEvent::Error(msg)) => {
                    let _ = app.emit("voice_error", serde_json::json!({
                        "message": format!("Video: {}", msg),
                    }));
                }
                Ok(video_pipeline::VideoPipelineEvent::ThumbnailReady(jpeg)) => {
                    if let (Some(ref tx), Some(ref ch_id)) = (&thumbnail_write_tx, &thumbnail_channel_id) {
                        use super::net::connection::build_packet;
                        use super::net::proto::*;
                        let data = build_packet(
                            packet::Type::StreamThumbnailUpdate,
                            packet::Payload::StreamThumbnailUpdate(StreamThumbnailUpdate {
                                channel_id: ch_id.clone(),
                                owner_username: String::new(),
                                thumbnail_data: jpeg,
                            }),
                            None,
                        );
                        let _ = tx.try_send(data);
                    }
                }
                Ok(video_pipeline::VideoPipelineEvent::EncodedFrame { data, is_keyframe, frame_id, codec, description }) => {
                    use base64::Engine;
                    let b64_data = base64::engine::general_purpose::STANDARD.encode(&data);
                    let b64_desc = description.as_ref().map(|d|
                        base64::engine::general_purpose::STANDARD.encode(d)
                    );
                    if frame_id % 60 == 0 || is_keyframe {
                        eprintln!("[self-preview-bridge] emit stream_frame user='{}' frame={} bytes={} keyframe={} codec={} desc={}",
                            self_username, frame_id, data.len(), is_keyframe, codec,
                            b64_desc.as_ref().map(|d| d.len()).unwrap_or(0));
                    }
                    let _ = app.emit("stream_frame", serde_json::json!({
                        "username": self_username,
                        "format": "h264",
                        "data": b64_data,
                        "timestamp": frame_id as u64 * 33_333,
                        "keyframe": is_keyframe,
                        "description": b64_desc,
                        "codec": codec,
                    }));
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    })
}

pub struct AudioStreamEngine {
    pipeline_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<audio_stream_pipeline::AudioStreamControl>,
    /// Linux-only: cleanup closure to restore PipeWire audio routing on stop.
    #[cfg(target_os = "linux")]
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl AudioStreamEngine {
    pub fn start(
        frame_rx: std::sync::mpsc::Receiver<capture::AudioFrame>,
        socket: Arc<UdpSocket>,
        sender_id: String,
        bitrate_kbps: u32,
        app: AppHandle,
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

        let event_bridge = tokio::task::spawn_blocking(move || {
            loop {
                match event_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(audio_stream_pipeline::AudioStreamEvent::Error(msg)) => {
                        let _ = app.emit("voice_error", serde_json::json!({
                            "message": format!("Stream audio: {}", msg),
                        }));
                    }
                    Ok(_) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
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
        let _ = self.control_tx.send(audio_stream_pipeline::AudioStreamControl::Shutdown);
        if let Some(h) = self.pipeline_thread.take() { let _ = h.join(); }
        if let Some(h) = self.event_bridge.take() { h.abort(); }
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
