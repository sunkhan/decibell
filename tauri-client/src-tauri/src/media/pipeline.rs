use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::{HashMap, VecDeque};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::codec::{
    OpusDecoder, OpusEncoder, StereoOpusDecoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE,
    SAMPLE_RATE, STEREO_FRAME_SAMPLES, STEREO_FRAME_SIZE,
};
use super::packet::{
    UdpAudioPacket, PACKET_TOTAL_SIZE, PACKET_TYPE_AUDIO, PACKET_TYPE_PING,
    PACKET_TYPE_STREAM_AUDIO,
};
use super::speaking::SpeakingDetector;
use super::video_packet::{UdpVideoPacket, PACKET_TYPE_VIDEO, PACKET_TYPE_KEYFRAME_REQUEST};
use super::video_receiver::ReassembledFrame;

// ── Linear resampler ─────────────────────────────────────────────────────────

/// Stateful linear-interpolation resampler. Carries fractional phase across
/// calls so there is no drift even with non-integer rate ratios (e.g. 44100↔48000).
struct LinearResampler {
    ratio: f64, // from_rate / to_rate — input samples consumed per output sample
    phase: f64,
    prev_sample: f64,
    passthrough: bool,
}

impl LinearResampler {
    fn new(from_rate: u32, to_rate: u32) -> Self {
        LinearResampler {
            ratio: from_rate as f64 / to_rate as f64,
            phase: 0.0,
            prev_sample: 0.0,
            passthrough: from_rate == to_rate,
        }
    }

    fn process(&mut self, input: &[f64], output: &mut Vec<f64>) {
        if input.is_empty() { return; }
        while self.phase < input.len() as f64 {
            let idx = self.phase as usize;
            let frac = self.phase - idx as f64;
            let s0 = if idx == 0 { self.prev_sample } else { input[idx - 1] };
            let s1 = input[idx];
            output.push(s0 + (s1 - s0) * frac);
            self.phase += self.ratio;
        }
        self.phase -= input.len() as f64;
        self.prev_sample = *input.last().unwrap();
    }
}

// ── Control / Event messages ──────────────────────────────────────────────────

pub enum ControlMessage {
    SetMute(bool),
    SetDeafen(bool),
    SetVoiceThreshold(f32), // dB threshold (-60 to 0); below this, send silence
    SetStreamVolume(f32),   // 0.0 to 1.0 — viewer-side stream audio volume
    SetUserVolume(String, f32), // username, linear gain (dB-converted on frontend)
    Shutdown,
}

pub enum VoiceEvent {
    SpeakingChanged(String, bool),
    UserStateChanged(String, bool, bool), // username, muted, deafened
    PingMeasured(u32),
    VideoFrameReady(ReassembledFrame),
    KeyframeRequested,
    Error(String),
}

// Flags byte prepended to audio payload
const FLAG_MUTED: u8 = 0x01;
const FLAG_DEAFENED: u8 = 0x02;

// ── Jitter buffer ────────────────────────────────────────────────────────────
//
// Holds incoming packets for a short time before decoding, so that late/
// out-of-order packets can be reordered. When a packet is truly lost, the
// Opus decoder's PLC (Packet Loss Concealment) fills the gap smoothly.

const JITTER_DEPTH: usize = 3; // packets to buffer before starting playback (60ms)
const JITTER_MAX: usize = 20;  // safety cap — force-drain if buffer grows past this

struct JitterBuffer {
    packets: HashMap<u16, Vec<u8>>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
}

impl JitterBuffer {
    fn new() -> Self {
        Self { packets: HashMap::new(), next_seq: 0, initialized: false, ready: false }
    }

    /// Insert a packet. Ignores packets behind the play cursor.
    fn push(&mut self, seq: u16, data: Vec<u8>) {
        if !self.initialized {
            self.next_seq = seq;
            self.initialized = true;
        }
        // Only accept if seq is at or ahead of the play cursor
        let diff = seq.wrapping_sub(self.next_seq);
        if diff < 32768 {
            self.packets.insert(seq, data);
        }
        if !self.ready && self.packets.len() >= JITTER_DEPTH {
            self.ready = true;
        }
        // Force-drain excess so the buffer can't grow unbounded.
        // If next_seq points to a gap (no packet), jump to the earliest actual
        // entry first — otherwise the while loop would spin through thousands
        // of empty sequence numbers before hitting a real packet.
        while self.packets.len() > JITTER_MAX {
            if !self.packets.contains_key(&self.next_seq) {
                if let Some(&earliest) = self.packets.keys()
                    .min_by_key(|&&s| s.wrapping_sub(self.next_seq))
                {
                    self.next_seq = earliest;
                } else {
                    break;
                }
            }
            self.packets.remove(&self.next_seq);
            self.next_seq = self.next_seq.wrapping_add(1);
        }
    }

    /// Pop the next frame. Returns:
    /// - `Some(Some(data))` — packet present, decode normally
    /// - `Some(None)` — packet missing, caller should do PLC
    /// - `None` — buffer not ready yet (still filling)
    fn drain(&mut self) -> Option<Option<Vec<u8>>> {
        if !self.ready { return None; }
        if self.packets.is_empty() {
            // Buffer drained completely — require re-buffering
            self.ready = false;
            return None;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        Some(self.packets.remove(&seq))
    }
}

// ── Per-remote-peer state ─────────────────────────────────────────────────────

struct RemotePeer {
    decoder: OpusDecoder,
    speaking: SpeakingDetector,
    last_packet_time: Instant,
    voice_jitter: JitterBuffer,
    voice_drain_time: Instant,
    stream_audio_decoder: Option<StereoOpusDecoder>,
    stream_jitter: JitterBuffer,
    stream_drain_time: Instant,
}

// ── Main blocking pipeline entry-point ───────────────────────────────────────

/// Runs the audio pipeline on the calling thread (should be a dedicated OS thread).
/// The socket must already be connected to the server (bind + connect done by caller).
/// Video packets are forwarded to `video_packet_tx` for processing on a separate thread,
/// keeping the audio loop fast and preventing video reassembly from causing audio choppiness.
pub fn run_audio_pipeline(
    socket: Arc<UdpSocket>,
    sender_id: String,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
    video_packet_tx: std::sync::mpsc::Sender<Vec<u8>>,
) {
    // Socket timeout is set by the dedicated recv thread — not needed here.
    // The audio loop uses channel-based recv (non-blocking try_recv).

    // ── Opus encoder ──────────────────────────────────────────────────────────
    let encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!(
                "Opus encoder init failed: {}",
                e
            )));
            return;
        }
    };

    // ── CPAL host + devices ───────────────────────────────────────────────────
    let host = cpal::default_host();

    // Output device is mandatory (listen-only still needs a sink).
    let output_device = match host.default_output_device() {
        Some(d) => d,
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No audio output device found".to_string(),
            ));
            return;
        }
    };

    // Use the device's default output config — Windows WASAPI is picky about
    // exact sample rate / channel count. We adapt in the callback instead.
    use cpal::traits::DeviceTrait;
    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Output device: {}ch @ {}Hz (sample format: {:?})",
                cfg.channels, cfg.sample_rate.0, default_cfg.sample_format()
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            // Last resort: try 48kHz stereo
            eprintln!("[pipeline] default_output_config failed ({}), trying 48kHz stereo", e);
            let cfg = cpal::StreamConfig {
                channels: 2,
                sample_rate: cpal::SampleRate(SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Default,
            };
            (cfg, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;

    // ── Shared buffers ────────────────────────────────────────────────────────
    let capture_buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let playback_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
    // Separate buffer for stream audio so we can MIX (sum) it with voice
    // instead of concatenating, which caused choppy alternating 20ms chunks.
    let stream_playback_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));

    const BUF_CAP: usize = FRAME_SIZE * 48; // ~1s at 48kHz, extra headroom for jitter buffer

    // ── Input (capture) stream ────────────────────────────────────────────────
    let input_device_opt = host.default_input_device();
    let cap_buf_in = Arc::clone(&capture_buf);

    let input_stream_opt: Option<cpal::Stream> = match input_device_opt {
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No microphone found — running in listen-only mode".to_string(),
            ));
            None
        }
        Some(input_device) => {
            // Use device's default input config for best compatibility
            let (input_cfg, input_channels) = match input_device.default_input_config() {
                Ok(default_cfg) => {
                    eprintln!(
                        "[pipeline] Input device: {}ch @ {}Hz (sample format: {:?})",
                        default_cfg.channels(), default_cfg.sample_rate().0, default_cfg.sample_format()
                    );
                    (cpal::StreamConfig {
                        channels: default_cfg.channels(),
                        sample_rate: default_cfg.sample_rate(),
                        buffer_size: cpal::BufferSize::Default,
                    }, default_cfg.channels())
                }
                Err(_) => {
                    (cpal::StreamConfig {
                        channels: 1,
                        sample_rate: cpal::SampleRate(SAMPLE_RATE),
                        buffer_size: cpal::BufferSize::Default,
                    }, 1u16)
                }
            };

            let in_ch = input_channels;
            let input_sample_rate = input_cfg.sample_rate.0;
            let mut capture_resampler = LinearResampler::new(input_sample_rate, SAMPLE_RATE);
            match input_device.build_input_stream(
                &input_cfg,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Downmix to mono f64
                    let mono: Vec<f64> = if in_ch == 1 {
                        data.iter().map(|&s| s as f64).collect()
                    } else {
                        data.chunks_exact(in_ch as usize)
                            .map(|frame| {
                                let sum: f64 = frame.iter().map(|&s| s as f64).sum();
                                sum / in_ch as f64
                            })
                            .collect()
                    };

                    // Resample to 48kHz if needed, then push i16 into capture_buf
                    if capture_resampler.passthrough {
                        if let Ok(mut buf) = cap_buf_in.lock() {
                            for &s in &mono {
                                if buf.len() >= BUF_CAP { break; }
                                buf.push((s * 32767.0) as i16);
                            }
                        }
                    } else {
                        let mut resampled = Vec::with_capacity(
                            (mono.len() as f64 / capture_resampler.ratio) as usize + 2
                        );
                        capture_resampler.process(&mono, &mut resampled);
                        if let Ok(mut buf) = cap_buf_in.lock() {
                            for &s in &resampled {
                                if buf.len() >= BUF_CAP { break; }
                                buf.push((s * 32767.0) as i16);
                            }
                        }
                    }
                },
                |e| {
                    eprintln!("[pipeline] capture stream error: {}", e);
                },
                None,
            ) {
                Ok(stream) => {
                    if let Err(e) = stream.play() {
                        eprintln!("[pipeline] failed to start capture stream: {}", e);
                        None
                    } else {
                        Some(stream)
                    }
                }
                Err(e) => {
                    eprintln!("[pipeline] build_input_stream failed: {}", e);
                    None
                }
            }
        }
    };

    // ── Output (playback) stream ──────────────────────────────────────────────
    let play_buf_out = Arc::clone(&playback_buf);
    let stream_buf_out = Arc::clone(&stream_playback_buf);

    let out_ch = output_channels;
    let mut playback_resampler = LinearResampler::new(SAMPLE_RATE, output_sample_rate);
    // Use f32 output — Windows WASAPI defaults to f32; i16 is often unsupported.
    let output_stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mono_frames_needed = data.len() / out_ch as usize;

            if playback_resampler.passthrough {
                // No resampling needed — output is already 48kHz
                // Mix voice + stream audio by summing both buffers.
                // Use try_lock to avoid blocking the real-time audio thread —
                // output silence if the recv loop is currently writing.
                let (mut voice_buf, mut stream_buf) = match (play_buf_out.try_lock(), stream_buf_out.try_lock()) {
                    (Ok(v), Ok(s)) => (v, s),
                    _ => {
                        for sample in data.iter_mut() { *sample = 0.0; }
                        return;
                    }
                };
                if out_ch == 1 {
                    for sample in data.iter_mut() {
                        let v = voice_buf.pop_front().unwrap_or(0) as i32;
                        let s = stream_buf.pop_front().unwrap_or(0) as i32;
                        let mixed = (v + s).clamp(-32768, 32767);
                        *sample = mixed as f32 / 32768.0;
                    }
                } else {
                    for frame in data.chunks_exact_mut(out_ch as usize) {
                        let v = voice_buf.pop_front().unwrap_or(0) as i32;
                        let s = stream_buf.pop_front().unwrap_or(0) as i32;
                        let mixed = (v + s).clamp(-32768, 32767) as f32 / 32768.0;
                        for ch in frame.iter_mut() {
                            *ch = mixed;
                        }
                    }
                }
                return;
            }

            // Resample 48kHz → output_sample_rate
            // Drain from both voice and stream buffers, mix, then resample
            let needed_in = ((mono_frames_needed as f64) * playback_resampler.ratio + 2.0).ceil() as usize;
            let input_48k: Vec<f64> = match (play_buf_out.try_lock(), stream_buf_out.try_lock()) {
                (Ok(mut voice_buf), Ok(mut stream_buf)) => {
                    let voice_take = needed_in.min(voice_buf.len());
                    let stream_take = needed_in.min(stream_buf.len());
                    let take = voice_take.max(stream_take);
                    (0..take).map(|_| {
                        let v = voice_buf.pop_front().unwrap_or(0) as f64 / 32768.0;
                        let s = stream_buf.pop_front().unwrap_or(0) as f64 / 32768.0;
                        (v + s).clamp(-1.0, 1.0)
                    }).collect()
                }
                _ => {
                    for sample in data.iter_mut() { *sample = 0.0; }
                    return;
                }
            };

            let mut resampled = Vec::with_capacity(mono_frames_needed + 2);
            playback_resampler.process(&input_48k, &mut resampled);

            let mut i = 0;
            if out_ch == 1 {
                for sample in data.iter_mut() {
                    *sample = if i < resampled.len() { resampled[i] as f32 } else { 0.0 };
                    i += 1;
                }
            } else {
                for frame in data.chunks_exact_mut(out_ch as usize) {
                    let s = if i < resampled.len() { resampled[i] as f32 } else { 0.0 };
                    for ch in frame.iter_mut() {
                        *ch = s;
                    }
                    i += 1;
                }
            }
        },
        |e| {
            eprintln!("[pipeline] playback stream error: {}", e);
        },
        None,
    ) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!(
                "Failed to build output stream: {}",
                e
            )));
            return;
        }
    };
    if let Err(e) = output_stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!(
            "Failed to start output stream: {}",
            e
        )));
        return;
    }

    // ── Local state ───────────────────────────────────────────────────────────
    let mut muted = false;
    let mut deafened = false;
    let mut was_muted_before_deafen = false;
    let mut voice_threshold_db: f32 = -50.0; // dB threshold; below this, send silence
    let mut stream_volume: f32 = 1.0; // 0.0–1.0 viewer-side stream audio volume
    let mut user_volumes: HashMap<String, f32> = HashMap::new(); // username → linear gain

    let mut sequence: u16 = 0;
    let mut local_speaking = SpeakingDetector::new();
    let mut remote_peers: HashMap<String, RemotePeer> = HashMap::new();

    let mut last_ping_time = Instant::now();
    let ping_interval = Duration::from_secs(3);

    // ── Dedicated UDP recv thread ───────────────────────────────────────────
    // Reads all incoming packets and dispatches by type into separate channels.
    // This keeps the audio processing loop free from socket I/O and prevents
    // video packet floods (100+ per keyframe) from stalling audio decode.
    let (audio_pkt_tx, audio_pkt_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    let recv_socket = Arc::clone(&socket);
    let recv_video_tx = video_packet_tx.clone();
    let recv_event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("decibell-udp-recv".to_string())
        .spawn(move || {
            udp_recv_thread(recv_socket, audio_pkt_tx, recv_video_tx, recv_event_tx);
        })
        .expect("spawn recv thread");

    // ── Main loop ─────────────────────────────────────────────────────────────
    'main: loop {
        let loop_start = Instant::now();

        // 1. Drain control messages ────────────────────────────────────────────
        loop {
            match control_rx.try_recv() {
                Ok(ControlMessage::Shutdown) => break 'main,
                Ok(ControlMessage::SetMute(m)) => {
                    if !deafened {
                        muted = m;
                    } else {
                        // While deafened, track what the user wants
                        was_muted_before_deafen = m;
                    }
                }
                Ok(ControlMessage::SetDeafen(d)) => {
                    if d && !deafened {
                        // Entering deafen: remember mute state, force mute
                        was_muted_before_deafen = muted;
                        deafened = true;
                        muted = true;
                    } else if !d && deafened {
                        // Leaving deafen: restore mute state
                        deafened = false;
                        muted = was_muted_before_deafen;
                    }
                }
                Ok(ControlMessage::SetVoiceThreshold(db)) => {
                    voice_threshold_db = db;
                }
                Ok(ControlMessage::SetStreamVolume(v)) => {
                    stream_volume = v.clamp(0.0, 1.0);
                }
                Ok(ControlMessage::SetUserVolume(username, gain)) => {
                    eprintln!("[pipeline] SetUserVolume: '{}' → gain={:.3}", username, gain);
                    user_volumes.insert(username, gain);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // 2. Capture & encode → send UDP ──────────────────────────────────────
        {
            let frame_opt: Option<[i16; FRAME_SIZE]> = {
                let mut buf = capture_buf.lock().unwrap();
                if buf.len() >= FRAME_SIZE {
                    let mut frame = [0i16; FRAME_SIZE];
                    frame.copy_from_slice(&buf[..FRAME_SIZE]);
                    buf.drain(..FRAME_SIZE);
                    Some(frame)
                } else {
                    None
                }
            };

            if let Some(frame) = frame_opt {
                // Compute RMS in dB for threshold check
                let rms = {
                    let sum_sq: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
                    (sum_sq / frame.len() as f64).sqrt() as f32
                };
                let rms_db = if rms > 0.0 {
                    20.0 * (rms / 32768.0).log10()
                } else {
                    -96.0
                };
                let above_threshold = !muted && rms_db >= voice_threshold_db;

                // Speaking detection based on threshold
                if let Some(state) = local_speaking.process_threshold(above_threshold) {
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), state));
                }

                let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                let encode_result = if muted || !above_threshold {
                    encoder.encode_silence(&mut opus_out)
                } else {
                    encoder.encode(&frame, &mut opus_out)
                };

                match encode_result {
                    Ok(len) => {
                        // Prepend flags byte: muted | deafened
                        let flags = if muted { FLAG_MUTED } else { 0 }
                            | if deafened { FLAG_DEAFENED } else { 0 };
                        let mut flagged = [0u8; MAX_OPUS_FRAME_SIZE + 1];
                        flagged[0] = flags;
                        flagged[1..1 + len].copy_from_slice(&opus_out[..len]);
                        let packet =
                            UdpAudioPacket::new_audio(&sender_id, sequence, &flagged[..1 + len]);
                        let _ = socket.send(&packet.to_bytes());
                        sequence = sequence.wrapping_add(1);
                    }
                    Err(e) => {
                        let _ = event_tx.send(VoiceEvent::Error(format!("Encode error: {}", e)));
                    }
                }
            } else if input_stream_opt.is_none() {
                // No mic — still send silence to keep the UDP session alive
                let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                if let Ok(len) = encoder.encode_silence(&mut opus_out) {
                    let flags = FLAG_MUTED; // no mic = effectively muted
                    let mut flagged = [0u8; MAX_OPUS_FRAME_SIZE + 1];
                    flagged[0] = flags;
                    flagged[1..1 + len].copy_from_slice(&opus_out[..len]);
                    let packet =
                        UdpAudioPacket::new_audio(&sender_id, sequence, &flagged[..1 + len]);
                    let _ = socket.send(&packet.to_bytes());
                    sequence = sequence.wrapping_add(1);
                }
            }
        }

        // 3. Send ping every 3s ────────────────────────────────────────────────
        if last_ping_time.elapsed() >= ping_interval {
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let ping_pkt = UdpAudioPacket::new_ping(&sender_id, ts_ns);
            let _ = socket.send(&ping_pkt.to_bytes());
            last_ping_time = Instant::now();
        }

        // 4. Drain audio packets from recv thread ─────────────────────────────
        // The dedicated recv thread reads the UDP socket and dispatches packets
        // by type. We only see audio/ping/keyframe-request packets here — video
        // packets go directly to the video thread, never touching this loop.
        loop {
            match audio_pkt_rx.try_recv() {
                Ok(raw) => {
                    if raw.len() == PACKET_TOTAL_SIZE {
                        if let Some(pkt) = UdpAudioPacket::from_bytes(&raw) {
                            let username = pkt.sender_username();

                            if pkt.packet_type == PACKET_TYPE_PING {
                                let payload = pkt.payload_data();
                                if payload.len() >= 8 {
                                    let sent_ns = u64::from_le_bytes(payload[..8].try_into().unwrap_or([0; 8]));
                                    let now_ns = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_nanos() as u64;
                                    let rtt_ms = (now_ns.saturating_sub(sent_ns) / 1_000_000) as u32;
                                    let _ = event_tx.send(VoiceEvent::PingMeasured(rtt_ms));
                                }
                            } else if username == sender_id {
                                // Ignore our own reflected audio packets
                            } else if pkt.packet_type == PACKET_TYPE_AUDIO {
                                let now = Instant::now();
                                let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                    RemotePeer {
                                        decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                            OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                        }),
                                        speaking: SpeakingDetector::new(),
                                        last_packet_time: now,
                                        voice_jitter: JitterBuffer::new(),
                                        voice_drain_time: now,
                                        stream_audio_decoder: None,
                                        stream_jitter: JitterBuffer::new(),
                                        stream_drain_time: now,
                                    }
                                });
                                peer.last_packet_time = now;

                                let raw_payload = pkt.payload_data();
                                let (flags, opus_data) = if raw_payload.len() > 1 {
                                    (raw_payload[0], &raw_payload[1..])
                                } else {
                                    (0u8, raw_payload)
                                };
                                let peer_muted = flags & FLAG_MUTED != 0;
                                let peer_deafened = flags & FLAG_DEAFENED != 0;
                                let _ = event_tx.send(VoiceEvent::UserStateChanged(
                                    username.clone(), peer_muted, peer_deafened,
                                ));

                                peer.voice_jitter.push(pkt.sequence, opus_data.to_vec());
                            } else if pkt.packet_type == PACKET_TYPE_STREAM_AUDIO {
                                if username != sender_id {
                                    let now = Instant::now();
                                    let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                        RemotePeer {
                                            decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                                OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                            }),
                                            speaking: SpeakingDetector::new(),
                                            last_packet_time: now,
                                            voice_jitter: JitterBuffer::new(),
                                            voice_drain_time: now,
                                            stream_audio_decoder: None,
                                            stream_jitter: JitterBuffer::new(),
                                            stream_drain_time: now,
                                        }
                                    });
                                    peer.last_packet_time = now;

                                    if peer.stream_audio_decoder.is_none() {
                                        peer.stream_audio_decoder = StereoOpusDecoder::new().ok();
                                    }

                                    peer.stream_jitter.push(pkt.sequence, pkt.payload_data().to_vec());
                                }
                            }
                        }
                    } else if !raw.is_empty() && raw[0] == PACKET_TYPE_KEYFRAME_REQUEST {
                        eprintln!("[recv] Keyframe request received, signaling encoder");
                        let _ = event_tx.send(VoiceEvent::KeyframeRequested);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // 4b. Drain jitter buffers → decode → push to playback ──────────────
        let drain_now = Instant::now();
        let frame_dur = Duration::from_millis(20);
        // Cap how far behind drain times can fall. Without this, a peer whose
        // jitter buffer empties (e.g. network stutter) accumulates a large
        // time debt, and when packets resume the loop fires dozens of PLC
        // frames in a single burst — audible as a glitch.
        let max_behind = Duration::from_millis(100);
        for (username, peer) in remote_peers.iter_mut() {
            if drain_now.duration_since(peer.voice_drain_time) > max_behind {
                peer.voice_drain_time = drain_now - max_behind;
            }
            if drain_now.duration_since(peer.stream_drain_time) > max_behind {
                peer.stream_drain_time = drain_now - max_behind;
            }
            // ── Voice jitter buffer ──
            while drain_now.duration_since(peer.voice_drain_time) >= frame_dur {
                peer.voice_drain_time += frame_dur;
                let opus_opt = match peer.voice_jitter.drain() {
                    Some(v) => v,
                    None => break, // not ready or empty
                };
                let mut pcm = [0i16; FRAME_SIZE];
                let decode_ok = match &opus_opt {
                    Some(data) => peer.decoder.decode(data, &mut pcm).is_ok(),
                    None => peer.decoder.decode(&[], &mut pcm).is_ok(), // PLC
                };
                if decode_ok {
                    let rms = {
                        let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
                        (sum_sq / pcm.len() as f64).sqrt() as f32
                    };
                    let rms_db = if rms > 0.0 { 20.0 * (rms / 32768.0).log10() } else { -96.0 };
                    if let Some(state) = peer.speaking.process_threshold(rms_db >= -50.0) {
                        let _ = event_tx.send(VoiceEvent::SpeakingChanged(username.clone(), state));
                    }
                    if !deafened {
                        let gain = user_volumes.get(username.as_str()).copied().unwrap_or(1.0);
                        if let Ok(mut pbuf) = playback_buf.lock() {
                            let remaining = BUF_CAP.saturating_sub(pbuf.len());
                            let take = FRAME_SIZE.min(remaining);
                            for &s in &pcm[..take] {
                                let scaled = ((s as f32) * gain) as i32;
                                pbuf.push_back(scaled.clamp(-32768, 32767) as i16);
                            }
                        }
                    }
                }
            }

            // ── Stream audio jitter buffer ──
            if let Some(ref mut decoder) = peer.stream_audio_decoder {
                while drain_now.duration_since(peer.stream_drain_time) >= frame_dur {
                    peer.stream_drain_time += frame_dur;
                    let opus_opt = match peer.stream_jitter.drain() {
                        Some(v) => v,
                        None => break,
                    };
                    let mut pcm = [0i16; STEREO_FRAME_SAMPLES];
                    let decode_ok = match &opus_opt {
                        Some(data) => decoder.decode(data, &mut pcm).is_ok(),
                        None => decoder.decode(&[], &mut pcm).is_ok(), // PLC
                    };
                    if decode_ok && !deafened {
                        if let Ok(mut sbuf) = stream_playback_buf.lock() {
                            let remaining = BUF_CAP.saturating_sub(sbuf.len());
                            let take = STEREO_FRAME_SIZE.min(remaining);
                            for i in 0..take {
                                let l = pcm[i * 2] as i32;
                                let r = pcm[i * 2 + 1] as i32;
                                let mono = ((l + r) / 2) as i16;
                                let scaled = ((mono as f32) * stream_volume) as i16;
                                sbuf.push_back(scaled);
                            }
                        }
                    }
                }
            }
        }

        // 5. Clean up stale remote peers (no packet for > 5s) ─────────────────
        let stale_timeout = Duration::from_secs(5);
        let mut to_remove: Vec<String> = Vec::new();
        for (name, peer) in &remote_peers {
            if peer.last_packet_time.elapsed() > stale_timeout {
                to_remove.push(name.clone());
            }
        }
        for name in to_remove {
            if let Some(mut peer) = remote_peers.remove(&name) {
                // Emit speaking-stopped if they were still marked speaking
                if peer.speaking.is_speaking() {
                    peer.speaking.reset();
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged(name, false));
                }
            }
        }

        // 6. Sleep if loop finished under 5ms ──────────────────────────────────
        let elapsed = loop_start.elapsed();
        let target = Duration::from_millis(5);
        if elapsed < target {
            std::thread::sleep(target - elapsed);
        }
    }

    // Streams are dropped here, which stops CPAL.
    // Dropping audio_pkt_rx disconnects the channel, causing the recv thread to exit.
    drop(audio_pkt_rx);
    drop(output_stream);
    drop(input_stream_opt);
}

// ── Dedicated UDP receive thread ─────────────────────────────────────────────
//
// Reads all incoming packets from the shared UDP socket and dispatches them
// by type into separate channels. This thread does NO processing — it just
// classifies and forwards, keeping the socket buffer drained at all times.
//
// Architecture (matching Discord's model):
//   recv thread → audio channel → audio processing thread
//                → video channel → video reassembly thread

fn udp_recv_thread(
    socket: Arc<UdpSocket>,
    audio_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    video_tx: std::sync::mpsc::Sender<Vec<u8>>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    const VIDEO_PACKET_SIZE: usize = std::mem::size_of::<UdpVideoPacket>();
    const RECV_BUF_SIZE: usize = if VIDEO_PACKET_SIZE > PACKET_TOTAL_SIZE {
        VIDEO_PACKET_SIZE
    } else {
        PACKET_TOTAL_SIZE
    };

    // Short timeout so we notice channel disconnection promptly
    if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(1))) {
        let _ = event_tx.send(VoiceEvent::Error(format!("recv thread: set_read_timeout: {}", e)));
        return;
    }

    let mut buf = [0u8; RECV_BUF_SIZE];

    loop {
        match socket.recv(&mut buf) {
            Ok(n) if n >= 1 => {
                let packet_type = buf[0];

                if packet_type == PACKET_TYPE_VIDEO && n == VIDEO_PACKET_SIZE {
                    // Video → video reassembly thread
                    if video_tx.send(buf[..n].to_vec()).is_err() {
                        break; // video thread gone
                    }
                } else {
                    // Audio, ping, stream audio, keyframe request → audio thread
                    match audio_tx.try_send(buf[..n].to_vec()) {
                        Ok(()) => {}
                        Err(std::sync::mpsc::TrySendError::Full(_)) => {
                            // Audio thread is behind — drop this packet.
                            // The jitter buffer's PLC will smooth the gap.
                        }
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                            break; // audio thread exited
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.raw_os_error() == Some(997) =>
            {
                // No data available — loop back to check for more
            }
            Err(e) => {
                eprintln!("[recv-thread] Socket error: {}", e);
                break;
            }
        }
    }

    eprintln!("[recv-thread] Exiting");
}
