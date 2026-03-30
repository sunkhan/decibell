pub mod audio_stream_pipeline;
pub mod capture;
#[cfg(target_os = "linux")]
pub mod capture_audio_pipewire;
#[cfg(target_os = "linux")]
pub mod capture_pipewire;
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

use std::net::UdpSocket;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use tauri::{AppHandle, Emitter};

use pipeline::{ControlMessage, VoiceEvent};

pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    socket: Arc<UdpSocket>,
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
            }
        }
        let socket = Arc::new(socket);

        let socket_for_audio = socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(socket_for_audio, sender_id_for_audio, control_rx, event_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;

        // Voice event bridge: poll event_rx and emit Tauri events
        let event_bridge = tokio::spawn(async move {
            loop {
                let rx_result = tokio::task::block_in_place(|| {
                    event_rx.recv_timeout(std::time::Duration::from_millis(50))
                });

                match rx_result {
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
                        VoiceEvent::PingMeasured(ms) => {
                            let _ = app.emit("voice_ping_updated", serde_json::json!({
                                "latencyMs": ms,
                            }));
                        }
                        VoiceEvent::VideoFrameReady(frame) => {
                            eprintln!("[video-bridge] Emitting stream_frame: user='{}', {} bytes, keyframe={}",
                                frame.streamer_username, frame.data.len(), frame.is_keyframe);
                            use base64::Engine;
                            let b64_data = base64::engine::general_purpose::STANDARD.encode(&frame.data);
                            // For keyframes, extract avcC description (SPS/PPS) from the
                            // AVCC-formatted data. WebCodecs needs this to configure the decoder.
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
                                "data": b64_data,
                                "timestamp": frame.frame_id as u64 * 33_333,
                                "keyframe": frame.is_keyframe,
                                "description": b64_desc,
                            }));
                        }
                        VoiceEvent::KeyframeRequested => {
                            // Forward PLI to the video encoder if streaming
                            use tauri::Manager;
                            let state = app.state::<crate::state::SharedState>();
                            let s = state.lock().await;
                            if let Some(ref engine) = s.video_engine {
                                engine.force_keyframe();
                                eprintln!("[video-bridge] Keyframe request forwarded to encoder");
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
            event_bridge: Some(event_bridge),
            control_tx,
            socket,
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

    pub fn set_user_volume(&self, username: String, gain: f32) {
        let _ = self.control_tx.send(ControlMessage::SetUserVolume(username, gain));
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

pub struct VideoEngine {
    pipeline_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    pipeline_control_tx: mpsc::Sender<video_pipeline::VideoPipelineControl>,
}

impl VideoEngine {
    /// Start the video send pipeline: encode frames from capture and send via UDP.
    pub fn start(
        frame_rx: std::sync::mpsc::Receiver<capture::RawFrame>,
        socket: Arc<UdpSocket>,
        sender_id: String,
        config: encoder::EncoderConfig,
        target_fps: u32,
        app: AppHandle,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let pipeline_thread = thread::Builder::new()
            .name("decibell-video".to_string())
            .spawn(move || {
                video_pipeline::run_video_send_pipeline(
                    frame_rx,
                    control_rx,
                    event_tx,
                    socket,
                    sender_id,
                    config,
                    target_fps,
                );
            })
            .expect("spawn video pipeline thread");

        // Bridge video pipeline events to Tauri
        let event_bridge = tokio::spawn(async move {
            loop {
                let rx_result = tokio::task::block_in_place(|| {
                    event_rx.recv_timeout(std::time::Duration::from_millis(50))
                });
                match rx_result {
                    Ok(video_pipeline::VideoPipelineEvent::Error(msg)) => {
                        let _ = app.emit("voice_error", serde_json::json!({
                            "message": format!("Video: {}", msg),
                        }));
                    }
                    Ok(video_pipeline::VideoPipelineEvent::ThumbnailReady(jpeg)) => {
                        // Send thumbnail to community server for broadcast
                        use tauri::Manager;
                        let state = app.state::<crate::state::SharedState>();
                        let s = state.lock().await;
                        // Find the connected server and channel
                        if let (Some(server_id), Some(channel_id)) = (
                            s.connected_voice_server.as_ref(),
                            s.connected_voice_channel.as_ref(),
                        ) {
                            if let Some(client) = s.communities.get(server_id) {
                                let _ = client.send_stream_thumbnail(channel_id, &jpeg).await;
                            }
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

        let event_bridge = tokio::spawn(async move {
            loop {
                let rx_result = tokio::task::block_in_place(|| {
                    event_rx.recv_timeout(std::time::Duration::from_millis(50))
                });
                match rx_result {
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
