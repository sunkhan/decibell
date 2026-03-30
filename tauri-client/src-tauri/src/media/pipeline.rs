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
use super::video_packet::{UdpVideoPacket, UdpKeyframeRequest, UdpNackPacket, PACKET_TYPE_VIDEO, PACKET_TYPE_KEYFRAME_REQUEST};
use super::video_receiver::{VideoReceiver, ReassembledFrame};

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

// ── Per-remote-peer state ─────────────────────────────────────────────────────

struct RemotePeer {
    decoder: OpusDecoder,
    speaking: SpeakingDetector,
    last_seq: u16,
    last_packet_time: Instant,
    stream_audio_decoder: Option<StereoOpusDecoder>,
    stream_audio_seq: u16,
}

// ── Main blocking pipeline entry-point ───────────────────────────────────────

/// Runs the audio pipeline on the calling thread (should be a dedicated OS thread).
/// The socket must already be connected to the server (bind + connect done by caller).
pub fn run_audio_pipeline(
    socket: Arc<UdpSocket>,
    sender_id: String,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // Set read timeout for non-blocking recv in the audio loop
    if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(5))) {
        let _ = event_tx.send(VoiceEvent::Error(format!(
            "UDP set_read_timeout failed: {}",
            e
        )));
        return;
    }

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

    // Buffer capacity sized for voice + stream audio mixed together.
    // 32 frames (~640ms at 48kHz) gives enough headroom when both voice
    // and stream audio are active, preventing silent drops during video bursts.
    const BUF_CAP: usize = FRAME_SIZE * 32;

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

    let out_ch = output_channels;
    let mut playback_resampler = LinearResampler::new(SAMPLE_RATE, output_sample_rate);
    // Use f32 output — Windows WASAPI defaults to f32; i16 is often unsupported.
    let output_stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mono_frames_needed = data.len() / out_ch as usize;

            if playback_resampler.passthrough {
                // No resampling needed — output is already 48kHz
                if let Ok(mut buf) = play_buf_out.lock() {
                    if out_ch == 1 {
                        for sample in data.iter_mut() {
                            let s = buf.pop_front().unwrap_or(0);
                            *sample = s as f32 / 32768.0;
                        }
                    } else {
                        for frame in data.chunks_exact_mut(out_ch as usize) {
                            let s = buf.pop_front().unwrap_or(0) as f32 / 32768.0;
                            for ch in frame.iter_mut() {
                                *ch = s;
                            }
                        }
                    }
                } else {
                    for sample in data.iter_mut() { *sample = 0.0; }
                }
                return;
            }

            // Resample 48kHz → output_sample_rate
            let needed_in = ((mono_frames_needed as f64) * playback_resampler.ratio + 2.0).ceil() as usize;
            let input_48k: Vec<f64> = if let Ok(mut buf) = play_buf_out.lock() {
                let take = needed_in.min(buf.len());
                buf.drain(..take).map(|s| s as f64 / 32768.0).collect()
            } else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
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

    let mut sequence: u16 = 0;
    let mut local_speaking = SpeakingDetector::new();
    let mut remote_peers: HashMap<String, RemotePeer> = HashMap::new();

    let mut last_ping_time = Instant::now();
    let ping_interval = Duration::from_secs(3);

    // Recv buffer sized for the largest packet type (video = 1445 > audio = 1437)
    const VIDEO_PACKET_SIZE: usize = std::mem::size_of::<UdpVideoPacket>();
    const RECV_BUF_SIZE: usize = if VIDEO_PACKET_SIZE > PACKET_TOTAL_SIZE { VIDEO_PACKET_SIZE } else { PACKET_TOTAL_SIZE };
    let mut recv_buf = [0u8; RECV_BUF_SIZE];

    let mut video_receiver = VideoReceiver::new();
    let mut last_video_cleanup = Instant::now();
    let mut video_streamer_username: Option<String> = None;
    let mut last_pli_time = Instant::now() - Duration::from_secs(10);

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

        // 4. Receive UDP packets ───────────────────────────────────────────────
        // Process up to MAX_PACKETS_PER_LOOP packets per iteration to prevent
        // video keyframe bursts (200+ packets) from starving audio processing.
        // The OS socket buffer (4MB) can hold many frames, so short-term
        // batching won't cause loss. We'll catch up in subsequent loop cycles.
        const MAX_PACKETS_PER_LOOP: usize = 64;
        let mut packets_this_loop = 0;
        loop {
            if packets_this_loop >= MAX_PACKETS_PER_LOOP { break; }
            match socket.recv(&mut recv_buf) {
                Ok(n) if n >= 1 => {
                    packets_this_loop += 1;
                    let packet_type = recv_buf[0];

                    if packet_type == PACKET_TYPE_VIDEO && n == VIDEO_PACKET_SIZE {
                        // ── Video packet ────────────────────────────────────
                        if let Some(pkt) = UdpVideoPacket::from_bytes(&recv_buf[..n]) {
                            let username = pkt.sender_username();
                            // Don't process our own reflected video packets
                            if username == sender_id {
                                // Skip own reflected video - but log once
                                static LOGGED_SELF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                                if !LOGGED_SELF.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                    eprintln!("[video-recv] Ignoring own video (sender='{}' == our id)", username);
                                }
                            } else {
                                video_streamer_username = Some(username.clone());
                                let fid = { pkt.frame_id };
                                let pidx = { pkt.packet_index };
                                let total = { pkt.total_packets };
                                if pidx == 0 {
                                    eprintln!("[video-recv] Got video pkt from '{}': frame={} ({} total pkts)", username, fid, total);
                                }
                                if let Some(frame) = video_receiver.process_packet(&pkt) {
                                    eprintln!("[video-recv] Frame {} reassembled: {} bytes, keyframe={}", frame.frame_id, frame.data.len(), frame.is_keyframe);
                                    let _ = event_tx.send(VoiceEvent::VideoFrameReady(frame));
                                }
                            }
                        }
                    } else if n == PACKET_TOTAL_SIZE {
                        // ── Audio or Ping packet ────────────────────────────
                        if let Some(pkt) = UdpAudioPacket::from_bytes(&recv_buf[..n]) {
                            let username = pkt.sender_username();

                            // Handle ping BEFORE sender_id filter (echoed pings have our own sender_id)
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
                                // Sequence duplicate/out-of-order check
                                let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                    RemotePeer {
                                        decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                            OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                        }),
                                        speaking: SpeakingDetector::new(),
                                        last_seq: pkt.sequence.wrapping_sub(1),
                                        last_packet_time: Instant::now(),
                                        stream_audio_decoder: None,
                                        stream_audio_seq: 0,
                                    }
                                });

                                let diff = pkt.sequence.wrapping_sub(peer.last_seq);
                                if diff == 0 || diff > 32768 {
                                    // skip stale/duplicate packet
                                } else {
                                    peer.last_seq = pkt.sequence;
                                    peer.last_packet_time = Instant::now();

                                    let raw_payload = pkt.payload_data();
                                    let (flags, opus_data) = if raw_payload.len() > 1 {
                                        (raw_payload[0], &raw_payload[1..])
                                    } else {
                                        (0u8, raw_payload)
                                    };
                                    let peer_muted = flags & FLAG_MUTED != 0;
                                    let peer_deafened = flags & FLAG_DEAFENED != 0;

                                    let _ = event_tx.send(VoiceEvent::UserStateChanged(
                                        username.clone(),
                                        peer_muted,
                                        peer_deafened,
                                    ));

                                    let mut pcm = [0i16; FRAME_SIZE];
                                    match peer.decoder.decode(opus_data, &mut pcm) {
                                        Ok(_) => {
                                            let rms = {
                                                let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
                                                (sum_sq / pcm.len() as f64).sqrt() as f32
                                            };
                                            let rms_db = if rms > 0.0 {
                                                20.0 * (rms / 32768.0).log10()
                                            } else {
                                                -96.0
                                            };
                                            let above = !peer_muted && rms_db >= -50.0;
                                            if let Some(state) = peer.speaking.process_threshold(above) {
                                                let _ = event_tx.send(VoiceEvent::SpeakingChanged(
                                                    username.clone(),
                                                    state,
                                                ));
                                            }

                                            if !deafened {
                                                let mut pbuf = playback_buf.lock().unwrap();
                                                let remaining = BUF_CAP.saturating_sub(pbuf.len());
                                                let take = FRAME_SIZE.min(remaining);
                                                for &s in &pcm[..take] {
                                                    pbuf.push_back(s);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            let _ = event_tx
                                                .send(VoiceEvent::Error(format!("Decode error: {}", e)));
                                        }
                                    }
                                }
                            } else if pkt.packet_type == PACKET_TYPE_STREAM_AUDIO {
                                // ── Stream audio packet ────────────────────────
                                if username != sender_id {
                                    let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                        RemotePeer {
                                            decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                                OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                            }),
                                            speaking: SpeakingDetector::new(),
                                            last_seq: 0,
                                            last_packet_time: Instant::now(),
                                            stream_audio_decoder: None,
                                            stream_audio_seq: 0,
                                        }
                                    });

                                    // Lazy-init stereo decoder on first stream audio packet
                                    if peer.stream_audio_decoder.is_none() {
                                        peer.stream_audio_decoder = StereoOpusDecoder::new().ok();
                                        // Set seq so first packet is accepted (not skipped as diff==0)
                                        peer.stream_audio_seq = pkt.sequence.wrapping_sub(1);
                                    }

                                    let diff = pkt.sequence.wrapping_sub(peer.stream_audio_seq);
                                    if diff == 0 || diff > 32768 {
                                        // skip stale/duplicate
                                    } else {
                                        peer.stream_audio_seq = pkt.sequence;
                                        peer.last_packet_time = Instant::now();

                                        if let Some(ref mut decoder) = peer.stream_audio_decoder {
                                            let opus_data = pkt.payload_data();
                                            let mut pcm = [0i16; STEREO_FRAME_SAMPLES];
                                            match decoder.decode(opus_data, &mut pcm) {
                                                Ok(_) => {
                                                    if !deafened {
                                                        let mut pbuf = playback_buf.lock().unwrap();
                                                        let remaining = BUF_CAP.saturating_sub(pbuf.len());
                                                        let take = STEREO_FRAME_SIZE.min(remaining);
                                                        for i in 0..take {
                                                            let l = pcm[i * 2] as i32;
                                                            let r = pcm[i * 2 + 1] as i32;
                                                            let mono = ((l + r) / 2) as i16;
                                                            let scaled = ((mono as f32) * stream_volume) as i16;
                                                            pbuf.push_back(scaled);
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!("[stream-audio-recv] Decode error: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if packet_type == PACKET_TYPE_KEYFRAME_REQUEST {
                        // Server relayed a PLI (keyframe request) from a watcher
                        eprintln!("[recv] Keyframe request received, signaling encoder");
                        let _ = event_tx.send(VoiceEvent::KeyframeRequested);
                    } else {
                        // Log unrecognized packets to help diagnose receive issues
                        static UNKNOWN_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                        let count = UNKNOWN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if count < 5 || count % 100 == 0 {
                            eprintln!("[recv] Unknown packet: type={}, size={} (expected video={} or audio={})",
                                packet_type, n, VIDEO_PACKET_SIZE, PACKET_TOTAL_SIZE);
                        }
                    }
                }
                Ok(_) => {}
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut
                        // Windows non-blocking sockets can return ERROR_IO_PENDING (997)
                        // instead of WouldBlock — treat it the same way.
                        || e.raw_os_error() == Some(997) =>
                {
                    break; // Socket buffer drained, continue main loop
                }
                Err(e) => {
                    let _ = event_tx.send(VoiceEvent::Error(format!("UDP recv error: {}", e)));
                    break;
                }
            }
        }

        // 4b. Periodic video receiver maintenance ────────────────────────────
        if last_video_cleanup.elapsed() > Duration::from_millis(100) {
            // Check for missing packets and send NACKs / PLI
            if let Some(ref target) = video_streamer_username {
                let (nacks, need_pli) = video_receiver.check_missing();
                for (frame_id, missing) in &nacks {
                    let nack_pkt = UdpNackPacket::new(&sender_id, target, *frame_id, missing);
                    let _ = socket.send(&nack_pkt.to_bytes());
                }
                // Rate-limit PLI to at most once per second
                if need_pli && last_pli_time.elapsed() > Duration::from_secs(1) {
                    eprintln!("[video-recv] Sending keyframe request (PLI) to '{}'", target);
                    let pli_pkt = UdpKeyframeRequest::new(&sender_id, target);
                    let _ = socket.send(&pli_pkt.to_bytes());
                    last_pli_time = Instant::now();
                }
            }
            video_receiver.cleanup_stale();
            last_video_cleanup = Instant::now();
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
    drop(output_stream);
    drop(input_stream_opt);
}
