pub mod audio_stream_pipeline;
pub mod capture;
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
pub mod capture_dxgi;
pub mod codec;
pub mod encoder;
pub mod packet;
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
    socket: Arc<UdpSocket>,
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
        app: AppHandle,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // UDP audio port is server_port + 1
        let udp_addr = format!("{}:{}", server_host, server_port + 1);

        // Sender ID: last 31 chars of JWT
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        // Create and connect UDP socket (shared between audio + video pipelines)
        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("UDP bind failed: {}", e))?;
        socket
            .connect(&udp_addr)
            .map_err(|e| format!("UDP connect failed: {}", e))?;
        // Increase socket buffers to handle video keyframe bursts.
        // Default ~208KB may be capped by net.core.rmem_max / wmem_max.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let fd = socket.as_raw_fd();
            let buf_size: libc::c_int = 4 * 1024 * 1024; // 4 MB (kernel may cap)
            unsafe {
                libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF,
                    &buf_size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF,
                    &buf_size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                // DSCP EF (Expedited Forwarding) — tells routers to prioritize
                // voice/video packets over bulk data (downloads, etc).
                let dscp_ef: libc::c_int = 0xB8; // DSCP 46 (EF) << 2
                libc::setsockopt(fd, libc::IPPROTO_IP, libc::IP_TOS,
                    &dscp_ef as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            let sock = socket.as_raw_socket();
            let buf_size: i32 = 4 * 1024 * 1024; // 4 MB
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
                // DSCP EF (Expedited Forwarding) — tells routers and the Windows
                // network stack to prioritize voice/video over bulk traffic.
                let dscp_ef: i32 = 0xB8; // DSCP 46 (EF) << 2
                let _ = windows::Win32::Networking::WinSock::setsockopt(
                    windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                    windows::Win32::Networking::WinSock::IPPROTO_IP.0 as i32,
                    windows::Win32::Networking::WinSock::IP_TOS as i32,
                    Some(std::slice::from_raw_parts(
                        &dscp_ef as *const i32 as *const u8,
                        std::mem::size_of::<i32>(),
                    )),
                );
                // Disable SIO_UDP_CONNRESET: Windows reports ICMP port-unreachable
                // errors as recv() failures (WSAECONNRESET/10054) on connected UDP
                // sockets. This kills the recv thread. Disabling this behavior
                // prevents spurious recv errors when the server briefly restarts
                // or a NAT mapping changes.
                let wsa_sock = windows::Win32::Networking::WinSock::SOCKET(sock as usize);
                const SIO_UDP_CONNRESET: u32 = 0x9800000C; // _WSAIOW(IOC_VENDOR, 12)
                let mut false_val: u32 = 0;
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
        let socket = Arc::new(socket);

        // Channel for forwarding raw video packets from the audio recv loop
        // to a dedicated video processing thread (prevents video reassembly
        // from blocking audio decode/playback).
        let (video_packet_tx, video_packet_rx) = mpsc::channel::<Vec<u8>>();

        // Clone event_tx before it's moved into the audio thread — the video
        // recv thread needs its own sender into the same event channel.
        let event_tx_video = event_tx.clone();

        let socket_for_audio = socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(socket_for_audio, sender_id_for_audio, control_rx, event_tx, video_packet_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;

        // Dedicated video recv thread: reassembles video frames, sends NACKs/PLI
        let socket_for_video_recv = socket.clone();
        let sender_id_for_video = sender_id.clone();
        let video_recv_thread = thread::Builder::new()
            .name("decibell-video-recv".to_string())
            .spawn(move || {
                run_video_recv_thread(video_packet_rx, socket_for_video_recv, sender_id_for_video, event_tx_video);
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
                            let b64_desc = if frame.is_keyframe {
                                encoder::extract_avcc_description_from_avcc(&frame.data)
                                    .map(|d| {
                                        eprintln!("[video-bridge] avcC description: {} bytes", d.len());
                                        base64::engine::general_purpose::STANDARD.encode(&d)
                                    })
                            } else {
                                None
                            };
                            let _ = app.emit("stream_frame", serde_json::json!({
                                "username": frame.streamer_username,
                                "format": "h264",
                                "data": b64_data,
                                "timestamp": frame.frame_id as u64 * 33_333,
                                "keyframe": frame.is_keyframe,
                                "description": b64_desc,
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
            socket,
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
        // Video recv thread exits when its video_packet_rx channel is dropped
        // (which happens when the audio thread exits and drops video_packet_tx).
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
    pub fn socket(&self) -> Arc<UdpSocket> { self.socket.clone() }
    pub fn sender_id(&self) -> &str { &self.sender_id }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Dedicated thread for video packet reassembly, NACK/PLI.
/// Receives raw video packet bytes from the audio recv loop via `packet_rx`,
/// keeping video processing completely off the audio thread.
fn run_video_recv_thread(
    packet_rx: std::sync::mpsc::Receiver<Vec<u8>>,
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

    // Linux: H.264 decoder for frames (WebKitGTK lacks WebCodecs) — tries GPU first
    #[cfg(target_os = "linux")]
    let mut sw_decoder: Option<video_decoder::H264Decoder> = None;
    #[cfg(target_os = "linux")]
    let mut sw_decoder_init_attempted = false;
    #[cfg(target_os = "linux")]
    let mut last_decode_time = Instant::now();

    // Helper: emit a reassembled video frame.
    // Linux: decode H.264 → JPEG on this thread, emit VideoFrameDecoded.
    // Windows: pass raw H.264 AVCC via VideoFrameReady for WebCodecs.
    let mut emit_frame = |frame: video_receiver::ReassembledFrame| {
        #[cfg(target_os = "linux")]
        {
            if !sw_decoder_init_attempted {
                sw_decoder_init_attempted = true;
                match video_decoder::H264Decoder::new() {
                    Ok(dec) => {
                        eprintln!("[video-recv] H.264 decoder initialized");
                        sw_decoder = Some(dec);
                    }
                    Err(e) => eprintln!("[video-recv] Failed to init H.264 decoder: {}", e),
                }
            }
            if let Some(ref mut dec) = sw_decoder {
                // Rate-limit: skip emitting delta frames if last decode was < 25ms ago (~40fps cap)
                let elapsed = last_decode_time.elapsed();
                if !frame.is_keyframe && elapsed < Duration::from_millis(25) {
                    // Still feed to decoder to keep reference frames correct
                    let _ = dec.decode_to_jpeg(&frame.data);
                } else if let Some(jpeg) = dec.decode_to_jpeg(&frame.data) {
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

    loop {
        // Drain available video packets (non-blocking after the first blocking recv)
        match packet_rx.recv_timeout(Duration::from_millis(5)) {
            Ok(buf) => {
                if let Some(pkt) = UdpVideoPacket::from_bytes(&buf) {
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

                // Drain any additional queued packets without blocking
                while let Ok(buf) = packet_rx.try_recv() {
                    if let Some(pkt) = UdpVideoPacket::from_bytes(&buf) {
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
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
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
        app: AppHandle,
        thumbnail_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        thumbnail_channel_id: Option<String>,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

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
                );
            })
            .expect("spawn video pipeline thread");

        // Bridge video pipeline events to Tauri — runs on the blocking pool
        // to avoid consuming a Tokio worker thread.
        let event_bridge = tokio::task::spawn_blocking(move || {
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
                        // Send thumbnail via cloned write_tx. Use try_send (non-blocking)
                        // since we're on the blocking pool and can't use .await.
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
                    Ok(_) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

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

    /// Clone the pipeline control sender so external code can forward keyframe
    /// requests without holding a reference to VideoEngine.
    pub fn pipeline_control_tx(&self) -> mpsc::Sender<video_pipeline::VideoPipelineControl> {
        self.pipeline_control_tx.clone()
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        self.stop();
    }
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
