use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ringbuf::{HeapRb, traits::{Consumer, Producer, Split, Observer}};
use rubato::Resampler;

use super::audio_device::{
    make_sinc_resampler, build_input_stream, build_output_stream,
    build_voice_output_stream, build_stream_output_stream, PeerList,
};
use super::codec::{
    OpusEncoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE,
    SAMPLE_RATE, STEREO_FRAME_SAMPLES, STEREO_FRAME_SIZE,
};
use super::peer::{PeerAudio, PeerOutput};
use arc_swap::ArcSwap;
use super::packet::{
    UdpAudioPacket, AUDIO_HEADER_SIZE, PACKET_TOTAL_SIZE, PACKET_TYPE_AUDIO, PACKET_TYPE_PING,
    PACKET_TYPE_STREAM_AUDIO,
};
use super::speaking::SpeakingDetector;
use super::video_receiver::ReassembledFrame;

// ── Control / Event messages ──────────────────────────────────────────────────

pub enum ControlMessage {
    SetMute(bool),
    SetDeafen(bool),
    SetVoiceThreshold(f32), // dB threshold (-60 to 0); below this, send silence
    SetStreamVolume(f32),   // 0.0 to 1.0 — viewer-side stream audio volume
    SetStreamStereo(bool),  // true = preserve L/R stereo in stream audio
    SetUserVolume(String, f32), // username, linear gain (dB-converted on frontend)
    SetInputDevice(Option<String>),  // None = system default
    SetOutputDevice(Option<String>), // None = system default
    /// (enabled, device_name) — when enabled, stream audio goes to a separate output device
    SetSeparateStreamOutput(bool, Option<String>),
    /// Change the stream output device (only effective when separate stream output is on)
    SetStreamOutputDevice(Option<String>),
    /// Voice processing toggles (AEC, NS, AGC)
    SetAecEnabled(bool),
    /// 0=off, 1=light(6dB), 2=moderate(12dB), 3=aggressive(18dB), 4=very aggressive(21dB)
    SetNoiseSuppressionLevel(u8),
    SetAgcEnabled(bool),
    Shutdown,
}

pub enum VoiceEvent {
    SpeakingChanged(String, bool),
    UserStateChanged(String, bool, bool), // username, muted, deafened
    /// Local microphone input level in dB (emitted ~every 50ms for the UI meter)
    InputLevel(f32),
    PingMeasured(u32),
    /// Periodic connection health snapshot for the user-panel telemetry
    /// popover. Emitted every 2s with the latest RTT and the audio packet
    /// loss percentage over the last sample window. latency_ms is None
    /// until the first PING reply lands.
    ConnectionStats { latency_ms: Option<u32>, packet_loss_pct: f32 },
    VideoFrameReady(ReassembledFrame),
    KeyframeRequested,
    Error(String),
}

// Flags byte prepended to audio payload
const FLAG_MUTED: u8 = 0x01;
const FLAG_DEAFENED: u8 = 0x02;

// Per-peer state lives in `media::peer::PeerAudio`. Voice mixing happens in
// the output audio callback which pulls from each peer's ring buffer.

// ── Main blocking pipeline entry-point ───────────────────────────────────────

/// Runs the audio pipeline on the calling thread (should be a dedicated OS thread).
/// The socket must already be connected to the server (bind + connect done by caller).
/// Video packets are forwarded to `video_packet_tx` for processing on a separate thread,
/// keeping the audio loop fast and preventing video reassembly from causing audio choppiness.
pub fn run_audio_pipeline(
    socket: Arc<UdpSocket>,
    sender_id: String,
    voice_bitrate_bps: i32,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // Socket timeout is set by the dedicated recv thread — not needed here.
    // The audio loop uses channel-based recv (non-blocking try_recv).

    // ── Opus encoder ──────────────────────────────────────────────────────────
    let encoder = match OpusEncoder::new(voice_bitrate_bps) {
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

    // ── Lock-free ring buffers ──────────────────────────────────────────────
    // SPSC ring buffers eliminate try_lock failures that caused silence pops.
    const BUF_CAP: usize = FRAME_SIZE * 48; // ~1s at 48kHz

    let capture_rb = HeapRb::<i16>::new(BUF_CAP);
    let (capture_prod, capture_cons) = capture_rb.split();
    let capture_prod = Arc::new(std::sync::Mutex::new(capture_prod));
    let capture_cons = Arc::new(std::sync::Mutex::new(capture_cons));

    // Per-peer voice rings live in each PeerAudio. The output callback reads
    // this atomically-swappable snapshot of current peers' consumers.
    let peers: PeerList = Arc::new(ArcSwap::from_pointee(Vec::new()));

    let stream_rb = HeapRb::<i16>::new(BUF_CAP);
    let (stream_prod, stream_cons) = stream_rb.split();
    let stream_prod = Arc::new(std::sync::Mutex::new(stream_prod));
    let stream_cons = Arc::new(std::sync::Mutex::new(stream_cons));

    // ── AEC render reference ring buffer ─────────────────────────────────────
    // Mono f32 at 48kHz — fed from the voice decode path in the main loop,
    // consumed by VoipAec3::handle_render_frame() before capture processing.
    let render_ref_rb = HeapRb::<f32>::new(BUF_CAP);
    let (render_ref_prod, render_ref_cons) = render_ref_rb.split();
    let render_ref_prod = Arc::new(std::sync::Mutex::new(render_ref_prod));
    let render_ref_cons = Arc::new(std::sync::Mutex::new(render_ref_cons));

    // ── Build output stream first (we need its sample rate for input matching) ─
    let stream_stereo = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (mut output_stream, mut output_sample_rate) = match build_output_stream(
        &host,
        None, // system default on startup
        Arc::clone(&peers),
        Arc::clone(&stream_cons),
        Arc::clone(&stream_stereo),
        Arc::clone(&render_ref_prod),
        &event_tx,
    ) {
        Some((stream, rate, _ch)) => (Some(stream), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No audio output device found".to_string(),
            ));
            return;
        }
    };

    // ── Build input (capture) stream ──────────────────────────────────────────
    let (mut input_stream_opt, mut input_sample_rate): (Option<cpal::Stream>, u32) = match build_input_stream(
        &host,
        None, // system default on startup
        Arc::clone(&capture_prod),
    ) {
        Some((s, rate)) => (Some(s), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No microphone found — running in listen-only mode".to_string(),
            ));
            (None, SAMPLE_RATE)
        }
    };

    // ── Main-loop resamplers (all DSP off the audio callback threads) ─────────
    // Capture: input_device_rate → 48kHz (for Opus encoding)
    let mut capture_resampler = if input_sample_rate == SAMPLE_RATE {
        None
    } else {
        eprintln!("[pipeline] Capture resampler: {}Hz → {}Hz", input_sample_rate, SAMPLE_RATE);
        Some(make_sinc_resampler(input_sample_rate, SAMPLE_RATE, 480, 1))
    };
    let mut capture_accum: Vec<f64> = Vec::new();

    // Voice resampling is per-peer now — each PeerAudio owns its own resampler
    // to output_sample_rate. Updated on device hot-swap via set_output_rate().

    // Playback stream: 48kHz → output_device_rate (stereo)
    let mut playback_stream_resampler = if output_sample_rate == SAMPLE_RATE {
        None
    } else {
        eprintln!("[pipeline] Playback stream resampler: {}Hz → {}Hz (stereo)", SAMPLE_RATE, output_sample_rate);
        Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2))
    };
    let mut playback_stream_accum_l: Vec<f64> = Vec::new();
    let mut playback_stream_accum_r: Vec<f64> = Vec::new();

    // Separate stream output (None = disabled, stream mixed into main output)
    let mut stream_output: Option<cpal::Stream> = None;
    let mut separate_stream_enabled = false;
    let mut stream_output_device_name: Option<String> = None;
    // When separate stream output is enabled, stream audio may play on a device
    // with a different sample rate than the voice output. Track it separately.
    let mut stream_output_sample_rate: u32 = output_sample_rate;

    // ── Local state ───────────────────────────────────────────────────────────
    let mut muted = false;
    let mut deafened = false;
    let mut was_muted_before_deafen = false;
    let mut voice_threshold_db: f32 = -50.0; // dB threshold; below this, send silence
    let mut stream_volume: f32 = 1.0; // 0.0–1.0 viewer-side stream audio volume
    let mut user_volumes: HashMap<String, f32> = HashMap::new(); // username → linear gain

    // Noise gate state: hysteresis + hang time to stop near-threshold flutter.
    // When the level hovers around voice_threshold_db, raw per-frame thresholding
    // flip-flops between "send voice" and "send silence" every 20ms. Each flip
    // creates a waveform discontinuity the listener perceives as a quiet pop.
    // Gate opens at voice_threshold_db, stays open while rms >= (threshold - 6dB),
    // and keeps transmitting for GATE_HANG_FRAMES extra frames after falling
    // below the close threshold. No effect on captured audio — only on whether
    // the encoded frame is voice or silence.
    const GATE_HYSTERESIS_DB: f32 = 6.0;
    const GATE_HANG_FRAMES: u32 = 10; // 200ms @ 20ms/frame
    let mut gate_open: bool = false;
    let mut gate_hang_remaining: u32 = 0;

    let mut sequence: u16 = 0;
    let mut local_speaking = SpeakingDetector::new();
    let mut input_level_counter: u32 = 0; // throttle InputLevel events (~every 3 frames = 60ms)
    let mut remote_peers: HashMap<String, PeerAudio> = HashMap::new();

    // Rebuilds the ArcSwap peer-list snapshot handed to the output callback.
    // Call after any peer insert/remove.
    fn refresh_peer_list(peers: &PeerList, remote_peers: &HashMap<String, PeerAudio>) {
        let snapshot: Vec<PeerOutput> = remote_peers
            .iter()
            .map(|(name, p)| p.output_handle(name))
            .collect();
        peers.store(Arc::new(snapshot));
    }

    // Accumulator for resampled 48kHz capture PCM — persists across loop iterations
    let mut capture_48k_buf: Vec<i16> = Vec::with_capacity(FRAME_SIZE * 4);

    let mut last_ping_time = Instant::now();
    let ping_interval = Duration::from_secs(3);

    // Connection-stats emission (powers the user-panel telemetry popover).
    // Sampled every 2s so the graph has reasonable temporal resolution
    // without flooding the IPC channel.
    let mut last_stats_time = Instant::now();
    let stats_interval = Duration::from_secs(2);
    let mut last_plc_total: u64 = 0;
    let mut last_decoded_total: u64 = 0;
    let mut last_latency_ms: Option<u32> = None;

    // AEC render reference: summed across peers per tick so AEC sees what the
    // speaker actually plays. Reset each tick; fed to render_ref_prod at end.
    let mut aec_render_mix: Vec<f32> = Vec::with_capacity(FRAME_SIZE * 4);

    // ── Voice processing (AEC / NS / AGC) ──────────────────────────────────
    // VoipAec3 bundles all three processors. We rebuild it when toggles change.
    let mut aec_enabled = false;
    let mut ns_level: u8 = 0; // 0=off, 1=light, 2=moderate, 3=aggressive, 4=very aggressive
    let mut agc_enabled = false;
    let mut voice_processor: Option<aec3::voip::VoipAec3> = None;

    // RNNoise deep-learning noise suppressor — much better than WebRTC's spectral NS.
    // Created when ns_level > 0, processes 480-sample (10ms) frames at 48kHz.
    let mut rnnoise: Option<Box<nnnoiseless::DenoiseState<'static>>> = None;

    // Helper: (re)build the voice processor based on current toggle state.
    // WebRTC NS is disabled when RNNoise handles suppression (avoids double-filtering).
    fn build_voice_processor(aec: bool, ns_level: u8, agc: bool) -> Option<aec3::voip::VoipAec3> {
        // Only use WebRTC processor for AEC and/or AGC — RNNoise handles NS
        if !aec && !agc {
            return None;
        }
        // 48kHz mono, 10ms frames = 480 samples
        let builder = aec3::voip::VoipAec3::builder(48000, 1, 1)
            .enable_noise_suppression(false) // RNNoise handles this
            .enable_gain_controller2(agc);
        match builder.build() {
            Ok(processor) => {
                eprintln!("[pipeline] Voice processor built: aec={}, ns_level={}, agc={}", aec, ns_level, agc);
                Some(processor)
            }
            Err(e) => {
                eprintln!("[pipeline] Failed to build voice processor: {}", e);
                None
            }
        }
    }

    fn build_rnnoise(ns_level: u8) -> Option<Box<nnnoiseless::DenoiseState<'static>>> {
        if ns_level == 0 {
            return None;
        }
        eprintln!("[pipeline] RNNoise deep-learning noise suppressor enabled (level={})", ns_level);
        Some(nnnoiseless::DenoiseState::new())
    }

    // AEC render reference accumulator (10ms = 480 samples at 48kHz)
    const AEC_FRAME_SIZE: usize = 480; // 10ms at 48kHz
    let mut render_ref_accum: Vec<f32> = Vec::with_capacity(AEC_FRAME_SIZE * 2);

    // ── Dedicated voice UDP recv thread ─────────────────────────────────────
    // Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice socket
    // and forwards them to the audio processing thread. Video packets arrive
    // on a separate media socket handled by the video recv thread in mod.rs.
    let (audio_pkt_tx, audio_pkt_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1024);
    let recv_drops = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let recv_socket = Arc::clone(&socket);
    let recv_event_tx = event_tx.clone();
    let recv_drops_clone = Arc::clone(&recv_drops);
    std::thread::Builder::new()
        .name("decibell-voice-recv".to_string())
        .spawn(move || {
            voice_recv_thread(recv_socket, audio_pkt_tx, recv_event_tx, recv_drops_clone);
        })
        .expect("spawn voice recv thread");

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
                Ok(ControlMessage::SetStreamStereo(enabled)) => {
                    stream_stereo.store(enabled, std::sync::atomic::Ordering::Relaxed);
                }
                Ok(ControlMessage::SetUserVolume(username, gain)) => {
                    eprintln!("[pipeline] SetUserVolume: '{}' → gain={:.3}", username, gain);
                    user_volumes.insert(username, gain);
                }
                Ok(ControlMessage::SetInputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping input device to: {:?}", name);
                    input_stream_opt = None; // drop old stream
                    { let mut g = capture_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    capture_48k_buf.clear();
                    capture_accum.clear();
                    match build_input_stream(&host, name.as_deref(), Arc::clone(&capture_prod)) {
                        Some((stream, rate)) => {
                            input_stream_opt = Some(stream);
                            input_sample_rate = rate;
                            if rate == SAMPLE_RATE {
                                capture_resampler = None;
                                eprintln!("[pipeline] Input device {}Hz — passthrough", rate);
                            } else {
                                capture_resampler = Some(make_sinc_resampler(rate, SAMPLE_RATE, 480, 1));
                                eprintln!("[pipeline] Input device {}Hz — resampler to {}Hz", rate, SAMPLE_RATE);
                            }
                        }
                        None => {
                            input_stream_opt = None;
                            eprintln!("[pipeline] Warning: no input device after hot-swap");
                        }
                    }
                    // Reset voice processor state on device change
                    if voice_processor.is_some() {
                        voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                        render_ref_accum.clear();
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
                Ok(ControlMessage::SetOutputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping output device to: {:?}", name);
                    output_stream = None; // drop old stream
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    for peer in remote_peers.values_mut() { peer.drain_ring(); }
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
                    if separate_stream_enabled {
                        match build_voice_output_stream(&host, name.as_deref(), Arc::clone(&peers), Arc::clone(&render_ref_prod), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => eprintln!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    } else {
                        match build_output_stream(&host, name.as_deref(), Arc::clone(&peers), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => eprintln!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    }
                    // Update every peer's resampler for the new output rate.
                    for peer in remote_peers.values_mut() {
                        peer.set_output_rate(output_sample_rate);
                    }
                    // Stream resampler: only update if stream plays on the same device
                    if !separate_stream_enabled {
                        stream_output_sample_rate = output_sample_rate;
                        if output_sample_rate == SAMPLE_RATE {
                            playback_stream_resampler = None;
                        } else {
                            playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2));
                        }
                    }
                }
                Ok(ControlMessage::SetSeparateStreamOutput(enabled, device)) => {
                    eprintln!("[pipeline] Separate stream output: enabled={}, device={:?}", enabled, device);
                    separate_stream_enabled = enabled;
                    stream_output_device_name = device.clone();
                    // Rebuild main output and stream output
                    output_stream = None;
                    stream_output = None;
                    // Drain ring buffers to prevent stale audio causing delay
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    for peer in remote_peers.values_mut() { peer.drain_ring(); }
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
                    if enabled {
                        // Main output: voice-only
                        if let Some((stream, rate, _ch)) = build_voice_output_stream(&host, None, Arc::clone(&peers), Arc::clone(&render_ref_prod), &event_tx) {
                            output_sample_rate = rate;
                            output_stream = Some(stream);
                        }
                        // Stream output: stream-only on separate device (may have different rate)
                        if let Some((stream, rate, _ch)) = build_stream_output_stream(&host, device.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), &event_tx) {
                            stream_output_sample_rate = rate;
                            stream_output = Some(stream);
                        }
                    } else {
                        // Back to mixed mode — stream plays on same device as voice
                        if let Some((stream, rate, _ch)) = build_output_stream(&host, None, Arc::clone(&peers), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), &event_tx) {
                            output_sample_rate = rate;
                            stream_output_sample_rate = rate;
                            output_stream = Some(stream);
                        }
                    }
                    // Update every peer's resampler for the new voice output rate.
                    for peer in remote_peers.values_mut() {
                        peer.set_output_rate(output_sample_rate);
                    }
                    if stream_output_sample_rate == SAMPLE_RATE {
                        playback_stream_resampler = None;
                    } else {
                        playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, stream_output_sample_rate, 480, 2));
                    }
                }
                Ok(ControlMessage::SetStreamOutputDevice(name)) => {
                    if separate_stream_enabled {
                        eprintln!("[pipeline] Hot-swapping stream output device to: {:?}", name);
                        stream_output_device_name = name.clone();
                        stream_output = None;
                        // Drain stream ring buffer
                        { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                        playback_stream_accum_l.clear();
                        playback_stream_accum_r.clear();
                        if let Some((stream, rate, _ch)) = build_stream_output_stream(&host, name.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), &event_tx) {
                            stream_output_sample_rate = rate;
                            stream_output = Some(stream);
                            // Rebuild stream resampler for the new device rate
                            if rate == SAMPLE_RATE {
                                playback_stream_resampler = None;
                            } else {
                                playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, rate, 480, 2));
                            }
                        }
                    }
                }
                Ok(ControlMessage::SetAecEnabled(enabled)) => {
                    eprintln!("[pipeline] AEC enabled={}", enabled);
                    aec_enabled = enabled;
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Ok(ControlMessage::SetNoiseSuppressionLevel(level)) => {
                    eprintln!("[pipeline] NS level={}", level);
                    ns_level = level;
                    rnnoise = build_rnnoise(ns_level);
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Ok(ControlMessage::SetAgcEnabled(enabled)) => {
                    eprintln!("[pipeline] AGC enabled={}", enabled);
                    agc_enabled = enabled;
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // 1b. Feed AEC render reference from decoded voice audio ──────────────
        // Drain the render ref ring buffer and process 10ms chunks through AEC.
        if voice_processor.is_some() {
            if let Ok(mut rr_cons) = render_ref_cons.lock() {
                while let Some(s) = rr_cons.try_pop() {
                    render_ref_accum.push(s);
                }
            }
            // Feed complete 10ms render frames to AEC
            while render_ref_accum.len() >= AEC_FRAME_SIZE {
                let chunk: Vec<f32> = render_ref_accum.drain(..AEC_FRAME_SIZE).collect();
                if let Some(ref mut proc) = voice_processor {
                    let _ = proc.handle_render_frame(&chunk);
                }
            }
        }

        // 2. Capture & encode → send UDP ──────────────────────────────────────
        //
        // The capture ring buffer carries i16 at the input device's native rate.
        // We drain available samples, resample to 48kHz if needed, and accumulate
        // in capture_48k_buf. When we have a full 960-sample frame, encode + send.
        {
            // Drain all available samples from the capture ring buffer
            {
                let mut cons = capture_cons.lock().unwrap();
                let avail = cons.occupied_len();
                if avail > 0 {
                    if capture_resampler.is_none() {
                        // Input device is 48kHz — copy directly to the 48k buffer
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_48k_buf.push(s);
                            }
                        }
                    } else {
                        // Drain raw samples into the resampler accumulator
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_accum.push(s as f64 / 32768.0);
                            }
                        }
                    }
                }
            } // release capture_cons lock

            // Resample accumulated raw samples: input_rate → 48kHz
            if let Some(ref mut resampler) = capture_resampler {
                let mut needed = resampler.input_frames_next();
                while capture_accum.len() >= needed {
                    let chunk: Vec<f64> = capture_accum.drain(..needed).collect();
                    if let Ok(out) = resampler.process(&[&chunk], None) {
                        for &s in &out[0] {
                            capture_48k_buf.push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                        }
                    }
                    needed = resampler.input_frames_next();
                }
            }

            // Try to assemble a full Opus frame (960 samples at 48kHz = 20ms)
            let frame_opt: Option<[i16; FRAME_SIZE]> = if capture_48k_buf.len() >= FRAME_SIZE {
                let mut frame = [0i16; FRAME_SIZE];
                frame.copy_from_slice(&capture_48k_buf[..FRAME_SIZE]);
                capture_48k_buf.drain(..FRAME_SIZE);
                Some(frame)
            } else {
                None
            };

            if let Some(mut frame) = frame_opt {
                // ── Voice processing (AEC / NS / AGC) ────────────────────────
                // Process BEFORE threshold check so AGC-boosted levels are what
                // the threshold sees. Previous order caused AGC to boost quiet
                // audio after the gate had already decided to fade it out,
                // producing robotic popping artifacts.
                if !muted && (voice_processor.is_some() || rnnoise.is_some()) {
                    let mut f32_frame: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();

                    // Pass 1: WebRTC AEC + AGC (if enabled)
                    if let Some(ref mut proc) = voice_processor {
                        let mut output_buf = vec![0.0f32; AEC_FRAME_SIZE];
                        for chunk_idx in 0..2 {
                            let start = chunk_idx * AEC_FRAME_SIZE;
                            let end = start + AEC_FRAME_SIZE;
                            if let Ok(_) = proc.process_capture_frame(
                                &f32_frame[start..end],
                                false,
                                &mut output_buf,
                            ) {
                                f32_frame[start..end].copy_from_slice(&output_buf);
                            }
                        }
                    }

                    // Pass 2: RNNoise deep-learning noise suppression
                    if let Some(ref mut rnn) = rnnoise {
                        let mut rnn_in = [0.0f32; AEC_FRAME_SIZE];
                        let mut rnn_out = [0.0f32; AEC_FRAME_SIZE];
                        for chunk_idx in 0..2 {
                            let start = chunk_idx * AEC_FRAME_SIZE;
                            let end = start + AEC_FRAME_SIZE;
                            // RNNoise expects samples in [-32768, 32767] range
                            for (j, s) in f32_frame[start..end].iter().enumerate() {
                                rnn_in[j] = s * 32768.0;
                            }
                            let _vad = rnn.process_frame(&mut rnn_out, &rnn_in);
                            // Convert back to [-1, 1] range
                            for (j, s) in rnn_out.iter().enumerate() {
                                f32_frame[start + j] = s / 32768.0;
                            }
                        }
                    }

                    for (i, s) in f32_frame.iter().enumerate() {
                        frame[i] = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                    }
                }

                // Compute RMS in dB on the PROCESSED frame (post-AGC/NS)
                let rms = {
                    let sum_sq: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
                    (sum_sq / frame.len() as f64).sqrt() as f32
                };
                let rms_db = if rms > 0.0 {
                    20.0 * (rms / 32768.0).log10()
                } else {
                    -96.0
                };
                let open_threshold = voice_threshold_db;
                let close_threshold = voice_threshold_db - GATE_HYSTERESIS_DB;
                if muted {
                    gate_open = false;
                    gate_hang_remaining = 0;
                } else if rms_db >= open_threshold {
                    gate_open = true;
                    gate_hang_remaining = GATE_HANG_FRAMES;
                } else if gate_open {
                    if rms_db >= close_threshold {
                        // Soft zone — keep open and refresh hang.
                        gate_hang_remaining = GATE_HANG_FRAMES;
                    } else if gate_hang_remaining > 0 {
                        gate_hang_remaining -= 1;
                    } else {
                        gate_open = false;
                    }
                }
                let transmit_voice = gate_open;

                // Emit input level for the UI meter (~every 60ms)
                input_level_counter += 1;
                if input_level_counter >= 3 {
                    input_level_counter = 0;
                    let _ = event_tx.send(VoiceEvent::InputLevel(rms_db));
                }

                // Speaking detection based on threshold (no hysteresis — this is
                // just for the UI speaking ring, user wants it to react immediately)
                if let Some(state) = local_speaking.process_threshold(!muted && rms_db >= open_threshold) {
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), state));
                }

                // Encode: when gate is closed, send true silence so nothing leaks
                // to listeners. Opus DTX makes silence frames tiny.
                let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                let encode_result = if !transmit_voice {
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
        let mut pkt_count_this_iter = 0u32;
        let mut peers_changed = false;
        loop {
            match audio_pkt_rx.try_recv() {
                Ok(raw) => {
                    pkt_count_this_iter += 1;
                    if raw.len() >= AUDIO_HEADER_SIZE {
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
                                    last_latency_ms = Some(rtt_ms);
                                    let _ = event_tx.send(VoiceEvent::PingMeasured(rtt_ms));
                                }
                            } else if username == sender_id {
                                // Ignore our own reflected audio packets
                            } else if pkt.packet_type == PACKET_TYPE_AUDIO {
                                let now = Instant::now();
                                let is_new = !remote_peers.contains_key(&username);
                                if is_new {
                                    eprintln!("[pipeline] New remote peer '{}' detected (type={}, seq={}, payload={}B)",
                                        username, pkt.packet_type, pkt.sequence, pkt.payload_size);
                                }
                                let inserted = !remote_peers.contains_key(&username);
                                let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                    PeerAudio::new(output_sample_rate, now)
                                });
                                peer.last_packet_time = now;
                                if inserted { peers_changed = true; }

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
                                peer.voice_underrun_logged = false;
                            } else if pkt.packet_type == PACKET_TYPE_STREAM_AUDIO {
                                if username != sender_id {
                                    let now = Instant::now();
                                    let inserted = !remote_peers.contains_key(&username);
                                    let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                        PeerAudio::new(output_sample_rate, now)
                                    });
                                    peer.last_packet_time = now;
                                    if inserted { peers_changed = true; }

                                    if peer.stream_audio_decoder.is_none() {
                                        peer.stream_audio_decoder = super::codec::StereoOpusDecoder::new().ok();
                                    }

                                    peer.stream_jitter.push(pkt.sequence, pkt.payload_data().to_vec());
                                }
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }
        if peers_changed {
            refresh_peer_list(&peers, &remote_peers);
        }

        // 4b. Drain jitter buffers → decode → push to per-peer ring ─────────
        let drain_now = Instant::now();
        let frame_dur = Duration::from_millis(20);
        // Cap how far behind drain times can fall. Without this, a peer whose
        // jitter buffer empties (e.g. network stutter) accumulates a large
        // time debt, and when packets resume the loop fires dozens of PLC
        // frames in a single burst — audible as a glitch.
        let max_behind = Duration::from_millis(100);
        aec_render_mix.clear();
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
                    None => {
                        // Buffer not ready (initial fill or auto-recovery reset).
                        // Log once per underrun episode; the flag is cleared when
                        // packets resume in the recv handler.
                        if !peer.voice_underrun_logged {
                            eprintln!("[pipeline] Jitter buffer reset for peer '{}' — re-buffering", username);
                            peer.voice_underrun_logged = true;
                        }
                        peer.voice_drain_time = drain_now;
                        break;
                    }
                };
                let mut pcm = [0i16; FRAME_SIZE];
                let decode_ok = match &opus_opt {
                    Some(data) => peer.decoder.decode(data, &mut pcm).is_ok(),
                    None => {
                        // Packet lost — try FEC recovery using the next packet
                        if let Some(next_data) = peer.voice_jitter.peek_next() {
                            peer.decoder.decode_fec(next_data, &mut pcm).is_ok()
                        } else {
                            // No next packet available — fall back to basic PLC
                            peer.decoder.decode(&[], &mut pcm).is_ok()
                        }
                    }
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
                        let mut f32_frame = [0.0f32; FRAME_SIZE];
                        for (i, &s) in pcm.iter().enumerate() {
                            f32_frame[i] = (s as f32 / 32768.0) * gain;
                        }
                        // Sum into AEC render reference (mono, pre-resample).
                        if aec_enabled {
                            if aec_render_mix.len() < f32_frame.len() {
                                aec_render_mix.resize(f32_frame.len(), 0.0);
                            }
                            for (dst, &s) in aec_render_mix.iter_mut().zip(f32_frame.iter()) {
                                *dst += s;
                            }
                        }
                        peer.push_voice_frame(&f32_frame);
                    }
                }
            }

            // ── Stream audio jitter buffer ──
            if let Some(ref mut decoder) = peer.stream_audio_decoder {
                while drain_now.duration_since(peer.stream_drain_time) >= frame_dur {
                    peer.stream_drain_time += frame_dur;
                    let opus_opt = match peer.stream_jitter.drain() {
                        Some(v) => v,
                        None => {
                            peer.stream_drain_time = drain_now;
                            break;
                        }
                    };
                    let mut pcm = [0i16; STEREO_FRAME_SAMPLES];
                    let decode_ok = match &opus_opt {
                        Some(data) => decoder.decode(data, &mut pcm).is_ok(),
                        None => decoder.decode(&[], &mut pcm).is_ok(), // PLC
                    };
                    if decode_ok {
                        if playback_stream_resampler.is_none() {
                            // Output device is 48kHz — push directly
                            if let Ok(mut prod) = stream_prod.lock() {
                                for i in 0..STEREO_FRAME_SIZE {
                                    let l = pcm[i * 2] as i32;
                                    let r = pcm[i * 2 + 1] as i32;
                                    if stream_stereo.load(std::sync::atomic::Ordering::Relaxed) {
                                        let sl = ((l as f32) * stream_volume) as i32;
                                        let sr = ((r as f32) * stream_volume) as i32;
                                        let _ = prod.try_push(sl.clamp(-32768, 32767) as i16);
                                        let _ = prod.try_push(sr.clamp(-32768, 32767) as i16);
                                    } else {
                                        let mono = (l + r) / 2;
                                        let scaled = ((mono as f32) * stream_volume) as i32;
                                        let _ = prod.try_push(scaled.clamp(-32768, 32767) as i16);
                                    }
                                }
                            }
                        } else if let Some(ref mut resampler) = playback_stream_resampler {
                            // Resample stereo 48kHz → output device rate, then push
                            for i in 0..STEREO_FRAME_SIZE {
                                let l = pcm[i * 2] as f32 * stream_volume / 32768.0;
                                let r = pcm[i * 2 + 1] as f32 * stream_volume / 32768.0;
                                playback_stream_accum_l.push(l as f64);
                                playback_stream_accum_r.push(r as f64);
                            }
                            let mut needed = resampler.input_frames_next();
                            while playback_stream_accum_l.len() >= needed && playback_stream_accum_r.len() >= needed {
                                let cl: Vec<f64> = playback_stream_accum_l.drain(..needed).collect();
                                let cr: Vec<f64> = playback_stream_accum_r.drain(..needed).collect();
                                if let Ok(out) = resampler.process(&[&cl, &cr], None) {
                                    if let Ok(mut prod) = stream_prod.lock() {
                                        let is_stereo = stream_stereo.load(std::sync::atomic::Ordering::Relaxed);
                                        let len = out[0].len().min(out[1].len());
                                        for i in 0..len {
                                            if is_stereo {
                                                let _ = prod.try_push((out[0][i] * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                                let _ = prod.try_push((out[1][i] * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                            } else {
                                                let mono = (out[0][i] + out[1][i]) / 2.0;
                                                let _ = prod.try_push((mono * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                            }
                                        }
                                    }
                                }
                                needed = resampler.input_frames_next();
                            }
                        }
                        // Feed stream audio to AEC render reference (mono, with volume applied)
                        if aec_enabled {
                            if let Ok(mut rr_prod) = render_ref_prod.lock() {
                                for i in 0..STEREO_FRAME_SIZE {
                                    let l = pcm[i * 2] as f32;
                                    let r = pcm[i * 2 + 1] as f32;
                                    let mono = ((l + r) / 2.0) * stream_volume / 32768.0;
                                    let _ = rr_prod.try_push(mono);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 4c. Feed AEC render reference (mixed voice pre-playback). ────────
        if aec_enabled && !aec_render_mix.is_empty() {
            if let Ok(mut rr_prod) = render_ref_prod.lock() {
                for &s in &aec_render_mix {
                    let _ = rr_prod.try_push(s);
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
        let had_removals = !to_remove.is_empty();
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
        if had_removals {
            refresh_peer_list(&peers, &remote_peers);
        }

        // 6a. Connection-stats sample for the user-panel telemetry popover.
        // Aggregated across all remote peers — for the typical 1-2 peer
        // voice channel this is a faithful "how's my connection" reading.
        // packet_loss_pct is computed from the delta of plc/decoded counters
        // so it tracks the actual recent window, not session totals.
        if last_stats_time.elapsed() >= stats_interval {
            let mut plc_total: u64 = 0;
            let mut decoded_total: u64 = 0;
            for (_, p) in remote_peers.iter() {
                plc_total = plc_total.saturating_add(p.voice_jitter.plc_frames);
                decoded_total = decoded_total.saturating_add(p.voice_jitter.decoded_frames);
            }
            let plc_delta = plc_total.saturating_sub(last_plc_total);
            let decoded_delta = decoded_total.saturating_sub(last_decoded_total);
            let total_delta = plc_delta + decoded_delta;
            let packet_loss_pct = if total_delta == 0 {
                0.0
            } else {
                (plc_delta as f32 / total_delta as f32) * 100.0
            };
            let _ = event_tx.send(VoiceEvent::ConnectionStats {
                latency_ms: last_latency_ms,
                packet_loss_pct,
            });
            last_plc_total = plc_total;
            last_decoded_total = decoded_total;
            last_stats_time = Instant::now();
        }

        // 6. Periodic diagnostics (every 5s) ────────────────────────────────
        static DIAG_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let iter_num = DIAG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if iter_num % 1000 == 999 { // ~every 5s (5ms per iter)
            let stream_fill = if let Ok(c) = stream_cons.try_lock() { c.occupied_len() } else { 0 };
            let recv_drop_total = recv_drops.load(std::sync::atomic::Ordering::Relaxed);
            let peer_stats: Vec<String> = remote_peers.iter().map(|(n, p)| {
                format!("{}(j={:.1}ms tgt={} plc={} drop={})",
                    n, p.voice_jitter.jitter_ms(), p.voice_jitter.target(),
                    p.voice_jitter.plc_frames, p.voice_jitter.dropped_frames)
            }).collect();
            eprintln!("[pipeline] diag: peers=[{}], stream_buf={}/{}, recv_drops={}, pkts_this_iter={}",
                peer_stats.join(", "), stream_fill, BUF_CAP, recv_drop_total, pkt_count_this_iter);
        }

        // 7. Sleep if loop finished under 5ms ──────────────────────────────────
        let elapsed = loop_start.elapsed();
        let target = Duration::from_millis(5);
        if elapsed < target {
            std::thread::sleep(target - elapsed);
        }
    }

    // Streams are dropped here, which stops CPAL.
    // Dropping audio_pkt_rx disconnects the channel, causing the recv thread to exit.
    drop(audio_pkt_rx);
    drop(stream_output);    // Option<cpal::Stream> — separate stream output
    drop(output_stream);    // Option<cpal::Stream>
    drop(input_stream_opt); // Option<cpal::Stream>
}

// ── Dedicated voice UDP receive thread ──────────────────────────────────────
//
// Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice UDP socket
// and forwards them to the audio processing thread. Video packets arrive on
// a separate media socket handled by the video recv thread in mod.rs.

fn voice_recv_thread(
    socket: Arc<UdpSocket>,
    audio_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
    drops: Arc<std::sync::atomic::AtomicU64>,
) {
    const RECV_BUF_SIZE: usize = PACKET_TOTAL_SIZE;

    if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(1))) {
        let _ = event_tx.send(VoiceEvent::Error(format!("voice recv thread: set_read_timeout: {}", e)));
        return;
    }

    let mut buf = [0u8; RECV_BUF_SIZE];
    let mut recv_count: u64 = 0;
    let mut recv_log_time = Instant::now();

    loop {
        match socket.recv(&mut buf) {
            Ok(n) if n >= 1 => {
                recv_count += 1;
                if recv_log_time.elapsed() >= Duration::from_secs(5) {
                    eprintln!("[voice-recv] 5s stats: packets={}", recv_count);
                    recv_count = 0;
                    recv_log_time = Instant::now();
                }

                // Forward all packets on the voice socket to the audio thread.
                // No type classification needed — only voice packets arrive here.
                match audio_tx.try_send(buf[..n].to_vec()) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        // Audio thread is behind — drop this packet.
                        // The jitter buffer's PLC will smooth the gap.
                        drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        break;
                    }
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
                eprintln!("[voice-recv] Socket error: {}", e);
                break;
            }
        }
    }

    eprintln!("[voice-recv] Exiting");
}
