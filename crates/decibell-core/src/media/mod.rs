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
pub(crate) mod capture_chain;
pub mod call_crypto;
pub mod capture;
#[cfg(target_os = "linux")]
pub mod capture_audio_pipewire;
#[cfg(target_os = "windows")]
pub mod capture_audio_wasapi;
pub mod caps;
pub mod codec;
pub mod cue;
pub mod codec_selection;
pub mod debug_dump;
// DECIBELL_STREAM_DUMP: watched streams to .dbv files for the Qt client's spike (the `stream-dump` feature).
#[cfg(feature = "stream-dump")]
pub mod stream_dump;
pub mod jitter;
pub mod media_socket;
pub mod mic_test;
pub mod packet;
pub mod peer;
pub mod source_id;
pub mod stream_audio_filter;
pub mod stream_audio_mixer;
// Encoder backends: order, probe results, fallthrough, software limits
// (pure decisions, tested everywhere).
pub mod encoder_backend;
// The real probe (test encodes) and its cache.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod encoder_probe;
// SVT-AV1 through FFmpeg's libsvtav1: the software last resort.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod encoder_software;
#[cfg(target_os = "windows")]
pub mod gpu_pipeline;
// Captured D3D11 frames → CPU NV12 for the software encoder, never waiting
// on the GPU.
#[cfg(target_os = "windows")]
pub mod nv12_readback;
#[cfg(target_os = "windows")]
pub mod video_processor;
// Per-encoder option tables (pure data + tests).
pub mod bitrate_preset;
// Annex B / geometry helpers for the native encode path (tested everywhere).
pub mod encode_util;
#[cfg(target_os = "windows")]
pub mod encoder;
// Linux native FFmpeg encoder (NVENC / VAAPI / software SVT-AV1) — the
// Linux counterpart to the Windows `encoder` module.
#[cfg(target_os = "linux")]
pub mod encoder_linux;
// VAAPI input (upload + GPU convert) for AMD / Intel on Linux.
#[cfg(target_os = "linux")]
mod encoder_vaapi;
// Synthetic-frame encoder harness: every backend's output checked and
// decoded (tests only).
#[cfg(all(test, target_os = "linux"))]
mod encoder_harness;
// Procedural screen content and the encoder quality bench (tests only).
#[cfg(all(test, target_os = "linux"))]
mod encoder_content;
#[cfg(all(test, target_os = "linux"))]
mod encoder_bench;
// The harnesses' synthetic screen and output checks (tests only).
#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod encoder_checks;
// PipeWire / XDG-portal screen capture (universal Linux capture path).
#[cfg(target_os = "linux")]
pub mod capture_pipewire;
// wlr-screencopy fast path for wlroots compositors (Hyprland/Niri/Sway).
#[cfg(target_os = "linux")]
pub mod capture_wlr_screencopy;
// Linux encode loop wiring capture → encoder → VideoSender + self-preview.
#[cfg(target_os = "linux")]
pub mod encoder_thread_linux;
#[cfg(target_os = "windows")]
pub mod capture_wgc;
// Borderless monitor capture (DXGI Desktop Duplication) — preferred over
// WGC for screens because Win10's WGC draws a mandatory yellow border.
#[cfg(target_os = "windows")]
pub mod capture_dxgi;
// Pointer shape → straight-alpha BGRA for capture_dxgi; cross-platform
// so the unit tests run on the Linux dev box.
pub mod cursor_blend;
// GPU cursor compositor (one alpha-blended quad, no CPU↔GPU sync).
#[cfg(target_os = "windows")]
pub mod cursor_gpu;
#[cfg(target_os = "windows")]
pub mod encoder_thread;
#[cfg(target_os = "windows")]
pub mod thumbnail;
// Thumbnail downscale + capped JPEG encode on a worker thread, shared by
// the Windows and Linux native capture paths.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod thumb_encode;
pub mod pipeline;
pub mod punch;
pub mod speaking;
pub mod stun;
pub mod udp_recv;
#[cfg(test)]
pub mod voice_sim_tests;
pub mod video_packet;
pub mod video_pipeline;
pub mod video_receiver;
mod video_recv;
mod video_recv_host;
pub use video_recv_host::VideoRecvDemand;
pub mod voice_gate;
pub mod default_device_watch;
pub mod frame_crypto;
// Recycled frame buffers for the video receive path (off unless the host enables it).
pub mod frame_pool;

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use media_socket::MediaSocket;
use pipeline::{ControlMessage, VoiceEvent};

use crate::events;

/// The media identity: the relay stamps each sender's account uid, in
/// decimal, into `sender_id` (uid design), and a P2P call peer sends its own.
/// Every per-sender key below (watches, peers, volumes, frame keys) is that
/// string, so a rename never touches the media plane. `"__local__"` (our own
/// speaking state) is 0; anything that isn't a uid is None.
pub fn media_uid(key: &str) -> Option<i64> {
    if key == "__local__" {
        return Some(0);
    }
    key.parse::<i64>().ok().filter(|u| *u > 0)
}

/// Media keys (uids) of the remote streams the local user is currently
/// watching. The video receive thread drops reassembled frames whose streamer
/// isn't in this set: the client never validated that a relayed sender
/// matched an active watch, so a spoofed / over-relayed sender could be surfaced as another user;
/// this also skips the reassembly→IPC→renderer hops for frames nobody wants.
/// Own-stream self-preview bypasses this entirely (it flows from the encoder
/// thread's TSFN, not the receive thread). Lock-free reads on the hot path.
static WATCHED_STREAMS: std::sync::OnceLock<
    arc_swap::ArcSwap<std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

fn watched_streams() -> &'static arc_swap::ArcSwap<std::collections::HashSet<String>> {
    WATCHED_STREAMS
        .get_or_init(|| arc_swap::ArcSwap::from_pointee(std::collections::HashSet::new()))
}

/// Writers of WATCHED_STREAMS hold this: each is a load-clone-store, so two at
/// once could lose one's change (readers stay lock-free). It also orders the
/// video receive threads' start / stop (`video_recv_host`) after the changes
/// that caused them.
static WATCH_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Apply `change` (true when it changed the set) and let every live voice
/// session start or stop its video receive thread to match.
fn update_watched(change: impl FnOnce(&mut std::collections::HashSet<String>) -> bool) {
    let _w = WATCH_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let cur = watched_streams().load();
    let mut next = (**cur).clone();
    if !change(&mut next) {
        return;
    }
    watched_streams().store(Arc::new(next));
    video_recv_host::reconcile_all();
}

fn anything_watched() -> bool {
    !watched_streams().load().is_empty()
}

/// Mark a remote streamer as watched (frames for them are forwarded to JS).
/// The first watch of a session starts its video receive thread.
pub fn watch_stream_add(username: &str) {
    if watched_streams().load().contains(username) {
        return;
    }
    update_watched(|set| set.insert(username.to_string()));
}

/// Stop forwarding a streamer's frames. The last one stops the session's
/// video receive thread (unless it streams).
pub fn watch_stream_remove(username: &str) {
    if !watched_streams().load().contains(username) {
        return;
    }
    update_watched(|set| set.remove(username));
}

/// Drop all watches (voice session ended / switched channels).
pub fn watched_streams_clear() {
    update_watched(|set| {
        let had = !set.is_empty();
        set.clear();
        had
    });
}

pub(crate) fn is_watched(username: &str) -> bool {
    watched_streams().load().contains(username)
}

/// VoiceEngine — owns both the voice UDP socket (server_port + 1) and
/// the media UDP socket (server_port + 2). The voice socket carries
/// AUDIO / STREAM_AUDIO / PING traffic; the media socket carries
/// VIDEO / FEC / KEYFRAME_REQUEST / NACK. A separate `decibell-video-recv`
/// thread reassembles incoming video frames and pushes them through the
/// shared event channel.
pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    /// The video receive thread: it runs only while a stream is watched or
    /// sent in this session, or for the whole of a P2P call
    /// (`video_recv_host`). Shut down (stopped, woken, joined) in `stop`.
    video_recv: Arc<video_recv_host::VideoRecvHost>,
    voice_socket: Arc<MediaSocket>,
    media_socket: Arc<MediaSocket>,
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
        initial_input_device: Option<String>,
        initial_output_device: Option<String>,
        ring: frame_crypto::SharedKeyRing,
    ) -> Result<Self, String> {
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
        // Community relay: transparent (Plain) transport — bytes on the wire
        // are exactly what they were before MediaSocket existed.
        let voice_socket = Arc::new(MediaSocket::plain(voice_socket, &sender_id));
        let media_socket = Arc::new(MediaSocket::plain(media_socket, &sender_id));

        // Community channels are MLS-encrypted: the pipelines seal / open
        // every media payload with the ring's epoch keys (frame_crypto.rs).
        Self::start_with_sockets(
            voice_socket,
            media_socket,
            sender_id,
            voice_bitrate_bps,
            initial_input_device,
            initial_output_device,
            Some(ring),
        )
    }

    /// P2P DM call: both sockets are already sealed with the per-call keys
    /// and have their peer address locked by the hole punch. `sender_id`
    /// is our own username — there is no relay to rewrite it, so the
    /// peer's pipeline sees exactly what a relayed packet would carry.
    pub fn start_p2p(
        voice_socket: Arc<MediaSocket>,
        media_socket: Arc<MediaSocket>,
        self_username: String,
        voice_bitrate_bps: i32,
        initial_input_device: Option<String>,
        initial_output_device: Option<String>,
    ) -> Result<Self, String> {
        Self::start_with_sockets(
            voice_socket,
            media_socket,
            self_username,
            voice_bitrate_bps,
            initial_input_device,
            initial_output_device,
            None,
        )
    }

    /// Shared tail of `start` / `start_p2p`: spawn the audio pipeline, the
    /// video receive thread and the event bridge over two ready sockets.
    fn start_with_sockets(
        voice_socket: Arc<MediaSocket>,
        media_socket: Arc<MediaSocket>,
        sender_id: String,
        voice_bitrate_bps: i32,
        initial_input_device: Option<String>,
        initial_output_device: Option<String>,
        ring: Option<frame_crypto::SharedKeyRing>,
    ) -> Result<Self, String> {
        // This session takes the microphone: a mic test outside voice ends
        // before the pipeline opens it.
        mic_test::voice_session_started();
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let ring_audio = ring.clone();
        let ring_video = ring;

        // Clone before move so the video recv thread can push into the
        // same event channel as the audio pipeline.
        let event_tx_video = event_tx.clone();

        let voice_socket_for_audio = voice_socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let watch_tx = control_tx.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                // Lives exactly as long as this pipeline: follows the OS
                // default devices for whatever is set to "Default".
                let _default_watch = default_device_watch::DefaultDeviceWatch::start(watch_tx);
                // Guard the whole pipeline. A panic here (e.g. a virtual audio
                // driver reporting a config that trips a resampler or a
                // downmix assumption) used to silently terminate this thread,
                // leaving a "connected" call with dead audio and no error shown.
                // Catch it and surface an actionable message instead.
                let panic_tx = event_tx.clone();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pipeline::run_audio_pipeline(
                        voice_socket_for_audio,
                        sender_id_for_audio,
                        voice_bitrate_bps,
                        initial_input_device,
                        initial_output_device,
                        control_rx,
                        event_tx,
                        ring_audio,
                    );
                }));
                if let Err(e) = outcome {
                    let msg = e
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| e.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown error".to_string());
                    log::error!("[voice] audio pipeline panicked: {}", msg);
                    let _ = panic_tx.send(VoiceEvent::Error(format!(
                        "Audio engine crashed ({}) — try a different device in Settings → Audio",
                        msg
                    )));
                }
            })
            .map_err(|e| {
                mic_test::voice_session_ended();
                format!("Failed to spawn audio thread: {}", e)
            })?;

        // The video receive thread starts when it is first needed (a watch,
        // a stream sent on this media socket), or now for a P2P call.
        // Monotonic epoch for its stream-frame presentation timestamps (see
        // video_recv.rs), kept across its restarts.
        let video_recv = video_recv_host::VideoRecvHost::start(
            video_recv_host::VideoRecvParams {
                socket: media_socket.clone(),
                sender_id: sender_id.clone(),
                ring: ring_video,
                ts_epoch: std::time::Instant::now(),
                always: media_socket.is_sealed(),
            },
            event_tx_video,
            Box::new(anything_watched),
        );

        // Voice event bridge (video frames go from the receive thread straight
        // to the event sink). Runs on Tokio's blocking pool so it
        // never holds a worker thread (a previous `tokio::spawn` +
        // `block_in_place` pattern in tauri-client permanently stole
        // runtime workers).
        // It blocks in `recv` (no timeout to poll): it ends when every
        // sender is gone, i.e. once the audio, voice-receive and video-receive
        // threads have all exited.
        let event_bridge = crate::rt::spawn_blocking(move || loop {
            match event_rx.recv() {
                Ok(event) => match event {
                    VoiceEvent::SpeakingChanged(key, speaking) => {
                        if let Some(uid) = media_uid(&key) {
                            events::emit_voice_user_speaking(uid, speaking);
                        }
                    }
                    VoiceEvent::UserStateChanged(key, is_muted, is_deafened) => {
                        if let Some(uid) = media_uid(&key) {
                            events::emit_voice_user_state_changed(uid, is_muted, is_deafened);
                        }
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
                    VoiceEvent::KeyframeRequested => {
                        // PR8: forward to the renderer instead of the
                        // (deleted) native encoder. StreamCapture's
                        // listener calls VideoEncoder.encode(frame, {keyFrame: true})
                        // on the next capture frame.
                        events::emit_keyframe_requested();
                    }
                    VoiceEvent::Error(msg) => {
                        events::emit_voice_error(msg);
                    }
                },
                Err(std::sync::mpsc::RecvError) => break,
            }
        });

        Ok(VoiceEngine {
            audio_thread: Some(audio_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            video_recv,
            voice_socket,
            media_socket,
            sender_id,
            is_muted: false,
            is_deafened: false,
            was_muted_before_deafen: false,
        })
    }

    pub fn stop(&mut self) {
        // NOTE: the watched-streams set is deliberately NOT cleared here.
        // stop() runs on a blocking thread after the leave / hang-up command
        // has returned, and the next session (a new call, a new channel) may
        // already have registered its watches — clearing late wiped them and
        // the receive thread then dropped every frame the new stream sent.
        // The command sites clear synchronously instead (leave_voice_channel,
        // join_voice_channel, call_end, logout, shutdown).
        let _ = self.control_tx.send(ControlMessage::Shutdown);
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
            mic_test::voice_session_ended();
        }
        // Stop the video recv thread for good (flag, wake, join) and drop
        // its event sender, so the bridge below can end. Receive errors
        // never end that loop (review A2), only its stop flag does.
        self.video_recv.shut_down();
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

    /// Live Opus bitrate change (bps) on the running pipeline.
    pub fn set_voice_bitrate(&self, bps: i32) {
        let _ = self.control_tx.send(ControlMessage::SetVoiceBitrate(bps));
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

    pub fn voice_socket(&self) -> Arc<MediaSocket> {
        self.voice_socket.clone()
    }

    /// Keep this session's video receive thread running while a stream is
    /// sent on its media socket (the `VideoEngine` holds it): it brings the
    /// watchers' keyframe requests and answers the relay's bind challenges.
    pub fn video_recv_demand(&self) -> VideoRecvDemand {
        self.video_recv.demand()
    }

    /// While the video receive thread runs: becomes true once the relay
    /// echoed its media PING, i.e. bound our media endpoint. `watch_stream`
    /// waits for it (bounded) before asking for a stream.
    pub fn video_path_bound(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.video_recv.bound()
    }

    pub fn media_socket(&self) -> Arc<MediaSocket> {
        self.media_socket.clone()
    }

    /// Time since the last datagram (authenticated, in P2P mode) arrived on
    /// the voice socket — the peer-loss watchdog for DM calls reads this.
    pub fn voice_last_rx_age(&self) -> std::time::Duration {
        self.voice_socket.last_rx_age()
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
/// KEYFRAME_REQUEST, NACK. Whole frames go straight to the event sink
/// (`events::send_stream_frame`, through `video_recv::VideoRecvPath`);
/// keyframe requests and errors go to the voice event bridge via `event_tx`.
fn run_video_recv_thread(
    socket: Arc<MediaSocket>,
    sender_id: String,
    event_tx: mpsc::Sender<pipeline::VoiceEvent>,
    stop: Arc<AtomicBool>,
    waker: Arc<udp_recv::Waker>,
    ring: Option<frame_crypto::SharedKeyRing>,
    ts_epoch: std::time::Instant,
    bound: tokio::sync::watch::Sender<bool>,
) {
    use std::time::{Duration, Instant};
    use udp_recv::Ready;
    use video_packet::{UdpKeyframeRequest, UdpNackPacket};

    let mut path = video_recv::VideoRecvPath::new(ring, ts_epoch, frame_pool::global());
    let mut deliver = events::send_stream_frame;
    let mut last_maintenance = Instant::now();
    // 25ms cadence (was 100ms): with a 50ms NACK timeout, a 100ms poll put
    // retransmit requests 100–150ms behind the loss; 25ms reacts within
    // ~75ms. Only while the receiver has something to act on
    // (`VideoReceiver::has_pending`): a frame in reassembly, a held frame,
    // a keyframe request waiting out its throttle.
    const MAINTENANCE_EVERY: Duration = Duration::from_millis(25);

    // Periodic media-socket PING — binds + keeps this client's media UDP
    // endpoint on the server. Without this, pure watchers (who never send
    // video) stay unbound on the server's media side and receive no relayed
    // frames. The relay answers a PING from an unbound address with a bind
    // challenge (review S11), which MediaSocket::recv echoes; once bound, it
    // echoes the PING itself. Fire one immediately, then every 500 ms until
    // the first echo (so a lost challenge costs half a second), then every
    // 3s matching the voice PING cadence.
    let mut last_media_ping: Option<Instant> = None;
    let mut media_ping_answered = false;
    let media_ping_interval = Duration::from_secs(3);
    const MEDIA_PING_INTERVAL_UNANSWERED: Duration = Duration::from_millis(500);

    // The loop reads only once `wait` says a datagram is there; the read
    // timeout is a guard that never fires in normal operation (READ_GUARD).
    if let Err(e) = socket.set_read_timeout(Some(udp_recv::READ_GUARD)) {
        log::error!("[video-recv] set_read_timeout: {}", e);
        let _ = event_tx.send(pipeline::VoiceEvent::Error(format!(
            "Video receive couldn't start ({}) — screen share playback is unavailable",
            e
        )));
        return;
    }
    // Every receive error is transient; only the stop flag ends this loop
    // (review A2, udp_recv.rs). Datagrams are read in place: on the P2P path
    // they are opened where they landed (`MediaSocket::recv_ready`).
    let mut recv_buf = [0u8; udp_recv::RECV_BUF_SIZE];
    let mut recv_errors = udp_recv::RecvErrors::new("video-recv");

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let ping_every = if media_ping_answered {
            media_ping_interval
        } else {
            MEDIA_PING_INTERVAL_UNANSWERED
        };
        let media_ping_due = match last_media_ping {
            None => true,
            Some(t) => t.elapsed() >= ping_every,
        };
        if media_ping_due {
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let ping = packet::UdpAudioPacket::new_ping(&sender_id, ts_ns);
            let _ = socket.send(&ping.to_bytes());
            last_media_ping = Some(Instant::now());
        }

        if last_maintenance.elapsed() >= MAINTENANCE_EVERY {
            let (nacks, pli_targets) = path.receiver().check_missing();
            for nack in &nacks {
                let nack_pkt =
                    UdpNackPacket::new(&sender_id, &nack.target, nack.frame_id, &nack.missing);
                let _ = socket.send(&nack_pkt.to_bytes());
            }
            // PLI throttling is per sender inside check_missing; every
            // name returned is due a keyframe request now.
            for target in &pli_targets {
                log::info!("[video-recv] Sending keyframe request (PLI) to '{}'", target);
                let pli_pkt = UdpKeyframeRequest::new(&sender_id, target);
                let _ = socket.send(&pli_pkt.to_bytes());
            }
            path.receiver().cleanup_stale();
            last_maintenance = Instant::now();
        }

        // Sleep until a datagram, the stop's wake, or the next timed job:
        // the next PING, the next maintenance pass while one has work, the
        // pass that forgets a quiet streamer.
        let now = Instant::now();
        let mut next = last_media_ping.map_or(now, |t| t + ping_every);
        if path.receiver().has_pending() {
            next = next.min(last_maintenance + MAINTENANCE_EVERY);
        }
        if let Some(at) = path.receiver().idle_cleanup_at() {
            next = next.min(at.max(last_maintenance + MAINTENANCE_EVERY));
        }
        match udp_recv::wait(&socket, &waker, Some(next.saturating_duration_since(now))) {
            Ok(Ready::Socket) => {}
            Ok(Ready::Woken | Ready::Timeout) => continue,
            Err(e) => {
                recv_errors.handle(&e);
                continue;
            }
        }
        match socket.recv_ready(&mut recv_buf) {
            // Cut short to fit the buffer: bigger than anything we send.
            Ok(inner) if udp_recv::is_oversized(inner.len()) => recv_errors.ok(),
            Ok(inner) if !inner.is_empty() => {
                recv_errors.ok();
                match path.on_datagram(&recv_buf[inner], &mut deliver) {
                    video_recv::Control::KeyframeRequested => {
                        log::debug!("[video-recv] Keyframe request received, signaling encoder");
                        let _ = event_tx.send(pipeline::VoiceEvent::KeyframeRequested);
                    }
                    // Our PING's echo: the relay only echoes to a bound
                    // endpoint, so from here on the 3 s cadence will do.
                    // (Bind challenges are answered inside MediaSocket,
                    // which hands this loop an empty read for them.)
                    video_recv::Control::PingEcho => {
                        if !media_ping_answered {
                            media_ping_answered = true;
                            bound.send_replace(true);
                        }
                    }
                    video_recv::Control::None => {}
                }
            }
            Ok(_) => recv_errors.ok(),
            Err(e) => recv_errors.handle(&e),
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
    /// Process-unique id of this streaming session, so a caller cleaning
    /// up after a failed start only removes ITS engine from AppState.
    session_id: u64,
    /// Community (server_id, channel_id) the stream is announced in —
    /// None in a DM call (no community). Lets `stop_screen_share` notify
    /// the community when called without ids (the main process stopping
    /// an orphaned share after a renderer reload/crash), and tells the
    /// native thumbnail sender where to post, so a stream carried into
    /// another channel posts there.
    announce_target: AnnounceTarget,
    /// Keeps the voice session's video receive thread running while we
    /// stream (watchers' keyframe requests come in on it). Swapped for the
    /// new session's on a channel move (`retarget`).
    recv_demand: std::sync::Mutex<Option<VideoRecvDemand>>,
    /// JS-side encoder may emit an `EncodedFrame` self-preview pump too
    /// — but the local self-preview is just the raw VideoFrame painted
    /// to a canvas in the StreamCapture component, so the receive-side
    /// stream_frame fanout only carries remote streams now.
    _phantom: std::marker::PhantomData<()>,
    /// Windows-only: native capture loop, encoder thread, and a handle
    /// to the encoder's force-keyframe AtomicBool. None on Linux/macOS
    /// (those platforms keep the renderer-encoded path).
    #[cfg(target_os = "windows")]
    win_capture: Option<WinCapture>,
    #[cfg(target_os = "windows")]
    win_encoder_thread: Option<encoder_thread::EncoderThread>,
    #[cfg(target_os = "windows")]
    win_force_keyframe: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Tokio task that drains thumbnail JPEGs from the encoder thread
    /// and ships them to the community server. Aborted on stop_windows.
    #[cfg(target_os = "windows")]
    win_thumbnail_task: Option<tokio::task::JoinHandle<()>>,
    /// Linux-only: native PipeWire/portal capture + FFmpeg
    /// (NVENC/VAAPI/libx264) encode thread, the keyframe flag handle, and
    /// the thumbnail drain task. None unless the native path is engaged
    /// (the renderer WebCodecs path stays as the fallback).
    #[cfg(target_os = "linux")]
    lin_encoder_thread: Option<encoder_thread_linux::LinuxEncoderThread>,
    #[cfg(target_os = "linux")]
    lin_force_keyframe: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(target_os = "linux")]
    lin_thumbnail_task: Option<tokio::task::JoinHandle<()>>,
}

/// Which native capture backend is feeding the Windows encoder thread.
/// WGC is the preferred backend (native cursor compositing, HDR and
/// rotation handling) and is used everywhere it can capture without the
/// yellow border — Win11 / Server 2022+, where IsBorderRequired exists.
/// On Windows 10 the WGC border is mandatory, so monitors go through
/// DXGI Desktop Duplication instead, falling back to WGC (border and
/// all) when duplication can't start (cross-adapter output, rotated or
/// HDR-without-DuplicateOutput1 display, exclusive fullscreen). Single
/// windows always use WGC — duplication only does whole outputs.
#[cfg(target_os = "windows")]
enum WinCapture {
    Wgc(capture_wgc::Capture),
    Dxgi(capture_dxgi::Capture),
}

#[cfg(target_os = "windows")]
impl WinCapture {
    fn stop(self) {
        match self {
            WinCapture::Wgc(c) => c.stop(),
            WinCapture::Dxgi(c) => c.stop(),
        }
    }

    fn size(&self) -> (u32, u32) {
        match self {
            WinCapture::Wgc(c) => c.size(),
            WinCapture::Dxgi(c) => c.size(),
        }
    }
}

/// Why one attempt of `start_windows` did not start.
#[cfg(target_os = "windows")]
enum WinStartError {
    /// The device or the capture could not start: nothing another encoder
    /// would change, the start fails with this.
    Capture(String),
    /// The encoder was not tried (software above its limits).
    Skipped(String),
    /// The encoder could not be opened: the next choice is tried.
    Encoder(String),
}

/// The community (server_id, channel_id) a stream is announced in, shared by
/// the engine and its native thumbnail sender. None in a DM call.
type AnnounceTarget = Arc<arc_swap::ArcSwapOption<(String, String)>>;

/// Drain thumbnail JPEGs from a native encoder thread and post each one to
/// the channel the stream is announced in at that moment (nothing in a DM
/// call). The channel is bounded at 1 and the encoder thread uses try_send,
/// so a slow server drops a thumbnail instead of back-pressuring the encoder.
/// The task ends once the encoder thread drops its sender.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn spawn_thumbnail_sender(
    target: AnnounceTarget,
) -> (tokio::sync::mpsc::Sender<Vec<u8>>, tokio::task::JoinHandle<()>) {
    let (thumb_tx, mut thumb_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let task = crate::rt::spawn(async move {
        while let Some(jpeg) = thumb_rx.recv().await {
            let Some(target) = target.load_full() else {
                continue;
            };
            let (server_id, channel_id) = &*target;
            // Clone the write channel + build the packet under the lock,
            // then release it before awaiting the send — holding AppState
            // across the send stalls every other command.
            let parts = {
                let state_arc = crate::state::shared();
                let s = state_arc.lock().await;
                s.communities
                    .get(server_id)
                    .and_then(|c| c.thumbnail_send_parts(channel_id, &jpeg))
            };
            if let Some((tx, data)) = parts {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await;
            }
        }
    });
    (thumb_tx, task)
}

impl VideoEngine {
    /// Start a streaming session. PR8 keeps this trivial: just record
    /// who we are so log output can mention it, and clone a sender for
    /// packetisation. Codec selection + capture + encode live entirely
    /// renderer-side.
    pub fn start(
        socket: Arc<MediaSocket>,
        sender_id: String,
        self_username: String,
        ring: Option<frame_crypto::SharedKeyRing>,
        recv_demand: Option<VideoRecvDemand>,
    ) -> Self {
        let sender = Arc::new(video_pipeline::VideoSender::new(socket, sender_id, ring));
        // Publish the sender to the hot-path slot so `send_video_frame`
        // can reach it without taking the AppState mutex on every
        // encoded chunk. Cleared on stop/drop below.
        video_pipeline::set_frame_sink(sender.clone());
        static NEXT_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        VideoEngine {
            sender,
            self_username,
            session_id: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            announce_target: AnnounceTarget::default(),
            recv_demand: std::sync::Mutex::new(recv_demand),
            _phantom: std::marker::PhantomData,
            #[cfg(target_os = "windows")]
            win_capture: None,
            #[cfg(target_os = "windows")]
            win_encoder_thread: None,
            #[cfg(target_os = "windows")]
            win_force_keyframe: None,
            #[cfg(target_os = "windows")]
            win_thumbnail_task: None,
            #[cfg(target_os = "linux")]
            lin_encoder_thread: None,
            #[cfg(target_os = "linux")]
            lin_force_keyframe: None,
            #[cfg(target_os = "linux")]
            lin_thumbnail_task: None,
        }
    }

    /// Re-point the video sender at a new media socket and the new channel's
    /// MLS key ring. Used when the stream is carried into a new voice
    /// channel: the new VoiceEngine has a fresh media socket and the channel
    /// its own group, and swapping both here re-routes outgoing frames
    /// without tearing down (and re-prompting) the capture pipeline.
    /// `recv_demand` is the new session's: its video receive thread starts
    /// (watchers' keyframe requests), the old session's is let go.
    pub fn retarget(
        &self,
        socket: Arc<MediaSocket>,
        ring: Option<frame_crypto::SharedKeyRing>,
        recv_demand: VideoRecvDemand,
    ) {
        self.sender.retarget(socket, ring);
        let old = self
            .recv_demand
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(recv_demand);
        drop(old);
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// The stream's frame rate, which large frames are paced by.
    pub fn set_frame_rate(&self, fps: u32) {
        self.sender.set_frame_rate(fps);
    }

    /// Record the community channel the stream is announced in (start, and
    /// again on move_stream_to_channel). None for a DM-call stream.
    pub fn set_announce_target(&mut self, target: Option<(String, String)>) {
        self.announce_target.store(target.map(Arc::new));
    }

    /// (server_id, channel_id) the stream is announced in, if any.
    pub fn announce_target(&self) -> Option<(String, String)> {
        self.announce_target.load_full().map(|t| (*t).clone())
    }

    /// Windows-only: spin up the native capture + encoder pipeline.
    /// Source id is the Chromium desktopCapturer id ("screen:N:0" or
    /// "window:HWND:0"). `choices` are the encoders to try in order
    /// (`encoder_probe::plan`): each is opened on its own vendor's adapter
    /// with its own capture, and the first whose encoder opens runs the
    /// stream; a capture that cannot start ends the attempt (it is not the
    /// encoder's fault). `width`×`height` 0×0 means "Source": the captured
    /// source's own size, kept within the choice's ceiling. `detail` tunes
    /// the encoder for a 5 fps stream. Returns the encoded (width, height)
    /// and the encoder the stream started on, which is what gets announced.
    #[cfg(target_os = "windows")]
    pub fn start_windows(
        &mut self,
        source_id: &str,
        choices: &[encoder_thread::EncoderChoice],
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        detail: bool,
        include_cursor: bool,
    ) -> Result<(u32, u32, encoder_backend::StartedEncoder), String> {
        let target = source_id::parse(source_id)
            .map_err(|e| format!("source id '{}': {:?}", source_id, e))?;
        let mut errors: Vec<String> = Vec::new();
        for (i, choice) in choices.iter().enumerate() {
            // The software encoder may take over inside the thread when it
            // comes later in the list (its size limits are checked there
            // against the size this attempt encodes at).
            let software_later = choices[i + 1..]
                .iter()
                .any(|c| c.candidate.backend == encoder_backend::Backend::Software);
            match self.start_windows_on(
                &target,
                choice,
                software_later,
                width,
                height,
                fps,
                bitrate_kbps,
                detail,
                include_cursor,
            ) {
                Ok((w, h)) => {
                    log::info!("[stream] encoding {:?} with {}", choice.codec, choice.label);
                    let started = encoder_backend::StartedEncoder {
                        codec: choice.codec,
                        backend: choice.candidate.backend,
                        label: choice.label.clone(),
                    };
                    return Ok((w, h, started));
                }
                Err(WinStartError::Capture(e)) => return Err(e),
                Err(WinStartError::Skipped(why)) => {
                    log::warn!("[stream] {} not used: {why}", choice.label);
                    errors.push(format!("{}: {why}", choice.label));
                }
                Err(WinStartError::Encoder(e)) => {
                    encoder_probe::note_failed(choice.candidate, choice.codec, &e);
                    errors.push(format!("{}: {e}", choice.label));
                }
            }
        }
        Err(format!("No encoder could be opened ({})", errors.join("; ")))
    }

    /// One attempt of `start_windows`: device, capture and encoder thread
    /// for one choice. Leaves nothing running when it fails.
    #[cfg(target_os = "windows")]
    #[allow(clippy::too_many_arguments)]
    fn start_windows_on(
        &mut self,
        target: &source_id::CaptureTarget,
        choice: &encoder_thread::EncoderChoice,
        software_later: bool,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        detail: bool,
        include_cursor: bool,
    ) -> Result<(u32, u32), WinStartError> {
        let encoder_name = choice.encoder_name.as_str();
        // Open the device on the encoder vendor's GPU: on hybrid systems
        // the default adapter is often the other vendor's, and NVENC/AMF
        // can't open on a foreign device (nor Quick Sync on a non-Intel
        // one). Falls back to the default, which is also what the software
        // encoder captures on.
        let gpu = gpu_pipeline::GpuDevice::create_for_vendor(
            gpu_pipeline::vendor_for_encoder(encoder_name),
        )
        .map_err(WinStartError::Capture)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<
            windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        >(2);

        // WGC wherever it can go borderless (Win11+ — better cursor/HDR/
        // rotation handling); on Win10, where the WGC border is mandatory,
        // monitors switch to DXGI Desktop Duplication, degrading back to
        // WGC when duplication can't start. Window capture is WGC-only —
        // duplication does whole outputs.
        let capture = match *target {
            source_id::CaptureTarget::Monitor(idx) if !capture_wgc::borderless_supported() => {
                match capture_dxgi::Capture::start(&gpu, idx, tx.clone(), include_cursor, fps) {
                    Ok(c) => {
                        log::info!("[stream] monitor capture via DXGI duplication (Win10 borderless)");
                        WinCapture::Dxgi(c)
                    }
                    Err(e) => {
                        log::warn!(
                            "[stream] DXGI duplication unavailable ({e}); falling back to WGC"
                        );
                        WinCapture::Wgc(
                            capture_wgc::Capture::start(
                                &gpu,
                                source_id::CaptureTarget::Monitor(idx),
                                tx,
                                include_cursor,
                                fps,
                            )
                            .map_err(WinStartError::Capture)?,
                        )
                    }
                }
            }
            _ => WinCapture::Wgc(
                capture_wgc::Capture::start(&gpu, *target, tx, include_cursor, fps)
                    .map_err(WinStartError::Capture)?,
            ),
        };
        let (width, height) = if width == 0 || height == 0 {
            let (src_w, src_h) = capture.size();
            let size = encode_util::source_encode_size(src_w, src_h, choice.ceiling.0, choice.ceiling.1);
            log::info!("[stream] source {src_w}x{src_h} encodes at {}x{}", size.0, size.1);
            size
        } else {
            (width, height)
        };
        // Software above what the CPU is allowed: not opened, and not a
        // failure of the encoder.
        let cpus = encoder_probe::logical_cpus();
        let software = choice.candidate.backend == encoder_backend::Backend::Software;
        if software {
            if let Some(why) = encoder_backend::software_refusal(width, height, fps, cpus) {
                capture.stop();
                return Err(WinStartError::Skipped(why));
            }
        }
        let software_fallback = !software
            && software_later
            && encoder_backend::software_refusal(width, height, fps, cpus).is_none();

        // Thumbnail channel: the encoder thread try_sends JPEGs on its
        // schedule; the task posts them to the channel the stream is
        // announced in (followed across move_stream_to_channel).
        let (thumb_tx, thumb_task) = spawn_thumbnail_sender(self.announce_target.clone());

        let encoder_thread = match encoder_thread::EncoderThread::start(
            gpu,
            encoder_thread::EncoderThreadConfig {
                candidate: choice.candidate,
                codec: choice.codec,
                software_fallback,
                encoder_name: encoder_name.to_string(),
                codec_wire_byte: choice.codec as u8,
                width,
                height,
                fps,
                bitrate_kbps,
                detail,
                local_username: Arc::from(self.self_username.as_str()),
                video_sender: self.sender.clone(),
                thumbnail_tx: thumb_tx,
            },
            rx,
        ) {
            Ok(t) => t,
            Err(e) => {
                // The capture is already running and WinCapture has no
                // Drop: stop it here or its thread is orphaned (and keeps
                // the source captured). The thumbnail task would end on
                // its own once thumb_tx is gone; abort it anyway.
                capture.stop();
                thumb_task.abort();
                return Err(WinStartError::Encoder(e));
            }
        };
        self.win_force_keyframe = Some(encoder_thread.force_keyframe_handle());
        self.win_capture = Some(capture);
        self.win_encoder_thread = Some(encoder_thread);
        self.win_thumbnail_task = Some(thumb_task);
        Ok((width, height))
    }

    /// Windows-only: stop the native pipeline. Joins both threads (the
    /// encoder join is bounded — see EncoderThread::stop).
    #[cfg(target_os = "windows")]
    pub fn stop_windows(&mut self) {
        // Flag the encoder thread FIRST: stopping the capture disconnects
        // its frame channel, and an encoder that sees the disconnect
        // before its stop flag reports a spurious `native_stream_failed`,
        // which the renderer turns into a stop of the NEXT session.
        if let Some(e) = &self.win_encoder_thread {
            e.signal_stop();
        }
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

    /// Linux-only: spin up the native PipeWire/portal capture + FFmpeg
    /// (NVENC / VAAPI / software) encode pipeline. `source_id` is forwarded
    /// to the capture backend; the portal path ignores it (its own dialog
    /// picks the source), the wlr path parses it. `choices` are the
    /// encoders to try in order (`encoder_probe::plan`): the first that
    /// opens runs the stream, and one that fails before its first packet
    /// hands over to the next. `width`×`height` 0×0 means "Source": the
    /// negotiated capture size, kept within `ceiling`. `detail` tunes the
    /// encoder for a 5 fps stream. Returns the encoded (width, height) and
    /// the encoder the stream started on, which is what gets announced.
    #[cfg(target_os = "linux")]
    pub async fn start_linux(
        &mut self,
        source_id: &str,
        choices: Vec<encoder_thread_linux::EncoderChoice>,
        width: u32,
        height: u32,
        ceiling: (u32, u32),
        fps: u32,
        bitrate_kbps: u32,
        detail: bool,
        include_cursor: bool,
    ) -> Result<(u32, u32, encoder_backend::StartedEncoder), String> {
        let config = capture::CaptureConfig {
            target_fps: fps,
            target_width: width,
            target_height: height,
            include_cursor,
        };

        // wlr-screencopy is the fast path on wlroots compositors
        // (Hyprland/Niri/Sway); everywhere else (KDE/GNOME) we go through
        // the XDG ScreenCast portal, whose own dialog handles selection.
        let capture = if capture_wlr_screencopy::is_available()
            && capture_wlr_screencopy::owns_source(source_id)
        {
            log::info!("[video-linux] capture via wlr-screencopy");
            capture_wlr_screencopy::start_capture(source_id, &config).await?
        } else {
            log::info!("[video-linux] capture via xdg-desktop-portal");
            capture_pipewire::start_capture(source_id, &config).await?
        };
        let (width, height) = if width == 0 || height == 0 {
            let size = encode_util::source_encode_size(capture.width, capture.height, ceiling.0, ceiling.1);
            log::info!(
                "[video-linux] source {}x{} encodes at {}x{}",
                capture.width, capture.height, size.0, size.1
            );
            size
        } else {
            (width, height)
        };

        // Thumbnail drain task, as on Windows: posts to the channel the
        // stream is announced in at the time.
        let (thumb_tx, thumb_task) = spawn_thumbnail_sender(self.announce_target.clone());

        let thread = encoder_thread_linux::LinuxEncoderThread::start(
            encoder_thread_linux::LinuxEncoderThreadConfig {
                choices,
                width,
                height,
                fps,
                bitrate_kbps,
                detail,
                local_username: Arc::from(self.self_username.as_str()),
                video_sender: self.sender.clone(),
                thumbnail_tx: thumb_tx,
            },
            capture,
        )?;

        let started = thread.started().clone();
        self.lin_force_keyframe = Some(thread.force_keyframe_handle());
        self.lin_encoder_thread = Some(thread);
        self.lin_thumbnail_task = Some(thumb_task);
        Ok((width, height, started))
    }

    /// Linux-only: tear down the native pipeline. The encode thread's
    /// stop() flags it, ends the capture (which may sit paused with no
    /// frame to notice a dropped receiver on) and joins it; dropping the
    /// thumbnail Sender ends the drain task naturally, with abort() as
    /// belt-and-suspenders.
    #[cfg(target_os = "linux")]
    pub fn stop_linux(&mut self) {
        log::info!("[video-linux] stop_linux: tearing down native pipeline");
        if let Some(t) = self.lin_encoder_thread.take() {
            t.stop();
        }
        if let Some(h) = self.lin_thumbnail_task.take() {
            h.abort();
        }
        self.lin_force_keyframe = None;
    }

    /// Linux-only: nudge the encode thread to emit a keyframe.
    #[cfg(target_os = "linux")]
    pub fn request_keyframe(&self) {
        if let Some(flag) = &self.lin_force_keyframe {
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
        #[cfg(target_os = "linux")]
        self.stop_linux();
        // Only OUR sink: this runs on a blocking thread after
        // stop_screen_share returned, and the next stream may already own
        // the slot (see clear_frame_sink_if).
        video_pipeline::clear_frame_sink_if(&self.sender);
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
    /// The platform capture feeding `frame_rx`: re-filterable while live
    /// (`set_filter`), torn down on drop (Linux: null sink + taps removed;
    /// Windows: loopback clients stopped). None when the frames come from
    /// a source with no teardown of its own.
    capture: Option<Box<dyn stream_audio_filter::StreamAudioCapture>>,
}

impl AudioStreamEngine {
    pub fn start(
        frame_rx: mpsc::Receiver<capture::AudioFrame>,
        socket: Arc<MediaSocket>,
        sender_id: String,
        bitrate_kbps: u32,
        capture: Option<Box<dyn stream_audio_filter::StreamAudioCapture>>,
        ring: Option<frame_crypto::SharedKeyRing>,
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
                    ring,
                );
            })
            .expect("spawn audio stream pipeline thread");

        // Blocks in `recv`; ends when the pipeline thread (the only sender)
        // exits.
        let event_bridge = crate::rt::spawn_blocking(move || loop {
            match event_rx.recv() {
                Ok(audio_stream_pipeline::AudioStreamEvent::Error(msg)) => {
                    events::emit_voice_error(format!("Stream audio: {}", msg));
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvError) => break,
            }
        });

        AudioStreamEngine {
            pipeline_thread: Some(pipeline_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            capture,
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
        // Pipeline first (its frame receiver is gone, so capture threads
        // see Disconnected), then the capture handle: joins threads and
        // removes any temporary audio routing.
        self.capture.take();
    }

    /// Replace the application filter on the live capture. No-op when the
    /// capture backend has no filter (or there is no capture handle).
    pub fn set_filter(&self, filter: stream_audio_filter::StreamAudioFilter) {
        if let Some(c) = &self.capture {
            c.set_filter(filter);
        }
    }

    /// Re-point the STREAM_AUDIO sends at a new voice socket and the new
    /// channel's MLS key ring — used when the stream follows the user into
    /// a new voice channel.
    pub fn retarget(&self, socket: Arc<MediaSocket>, ring: Option<frame_crypto::SharedKeyRing>) {
        let _ = self
            .control_tx
            .send(audio_stream_pipeline::AudioStreamControl::Retarget(socket, ring));
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
pub fn configure_udp_socket(socket: &UdpSocket) {
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

#[cfg(test)]
mod watch_set_tests {
    use super::*;

    /// The watch set and the frame-sink set were updated by load, clone,
    /// store: two writers at once could each store a set without the other's
    /// change. Many threads adding and then removing their own keys at once
    /// must leave every addition visible, then every removal.
    #[test]
    fn concurrent_writers_never_lose_an_update() {
        let keys = |t: usize| (0..40).map(move |i| format!("race-{t}-{i}"));
        let run = |add: bool| {
            let threads: Vec<_> = (0..8)
                .map(|t| {
                    std::thread::spawn(move || {
                        for k in keys(t) {
                            if add {
                                watch_stream_add(&k);
                                events::set_stream_frame_sink(&k, true);
                            } else {
                                watch_stream_remove(&k);
                                events::set_stream_frame_sink(&k, false);
                            }
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
        };
        run(true);
        for t in 0..8 {
            for k in keys(t) {
                assert!(is_watched(&k), "{k} lost from the watch set");
                assert!(events::is_stream_frame_sink(&k), "{k} lost from the frame sinks");
            }
        }
        run(false);
        for t in 0..8 {
            for k in keys(t) {
                assert!(!is_watched(&k) && !events::is_stream_frame_sink(&k), "{k} still there");
            }
        }
    }
}

#[cfg(test)]
mod media_key_tests {
    use super::media_uid;

    #[test]
    fn media_keys_are_uids() {
        assert_eq!(media_uid("1042"), Some(1042));
        assert_eq!(media_uid("__local__"), Some(0));
        // A name (an older relay) or junk names nobody.
        assert_eq!(media_uid("alice"), None);
        assert_eq!(media_uid("0"), None);
        assert_eq!(media_uid("-7"), None);
        assert_eq!(media_uid(""), None);
    }
}

#[cfg(test)]
mod video_recv_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The media socket across a relay restart: its PINGs to the closed port
    /// come back as ECONNREFUSED (Linux), which used to end the loop and
    /// video with it. It must keep running and act on the relay's next
    /// datagram, and only the stop flag ends it.
    #[cfg(target_os = "linux")]
    #[test]
    fn video_recv_survives_connection_refused() {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(relay_addr).unwrap();
        let client_addr = client.local_addr().unwrap();
        drop(relay);

        let socket = Arc::new(MediaSocket::plain(client, "7"));
        let (event_tx, event_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(udp_recv::Waker::new().unwrap());
        let handle = {
            let (socket, stop, waker) = (socket.clone(), stop.clone(), waker.clone());
            let (bound, _) = tokio::sync::watch::channel(false);
            thread::spawn(move || run_video_recv_thread(socket, "7".into(), event_tx, stop, waker, None, Instant::now(), bound))
        };
        for _ in 0..20 {
            let _ = socket.send(b"\x05ping");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!handle.is_finished(), "video receive ended on ECONNREFUSED");

        let relay = UdpSocket::bind(relay_addr).unwrap();
        let pli = video_packet::UdpKeyframeRequest::new("9", "7").to_bytes();
        relay.send_to(&pli, client_addr).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut asked = false;
        while !asked && Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(VoiceEvent::KeyframeRequested) => asked = true,
                Ok(VoiceEvent::Error(m)) => panic!("error event: {m}"),
                _ => {}
            }
        }
        assert!(asked, "datagram after the errors never reached the loop");

        stop.store(true, Ordering::Relaxed);
        waker.wake();
        handle.join().unwrap();
    }
}
