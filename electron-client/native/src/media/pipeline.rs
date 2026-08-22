use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ringbuf::{HeapRb, traits::{Consumer, Producer, Split, Observer}};
use rubato::Resampler;

use super::audio_device::{
    make_sinc_resampler, build_input_stream, build_output_stream,
    build_voice_output_stream, build_stream_output_stream, OutputPullStats, PeerList,
};
use super::jitter::Frame;
use super::debug_dump::AudioDump;
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
    /// Live Opus bitrate change in bps (admin edited the channel's
    /// bitrate mid-call). Applies from the next encoded frame.
    SetVoiceBitrate(i32),
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
/// This frame carries no voice: a gate-closed tail frame or a keepalive.
/// Receivers use it to tell "the sender paused" from "a packet is late", and
/// to keep keepalive cadence out of their jitter estimate. Older clients
/// ignore the bit (they only test the two above) — wire-compatible.
pub const FLAG_SILENCE: u8 = 0x04;

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
    initial_input_device: Option<String>,
    initial_output_device: Option<String>,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // Socket timeout is set by the dedicated recv thread — not needed here.
    // The audio loop uses channel-based recv (non-blocking try_recv).

    // ── Opus encoder ──────────────────────────────────────────────────────────
    let mut encoder = match OpusEncoder::new(voice_bitrate_bps) {
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

    // Per-output-stream pull telemetry (see OutputPullStats). `main_pull`
    // serves the voice (or voice+stream) output, `stream_pull` the separate
    // stream-audio output when that mode is on.
    // Opt-in raw audio capture (DECIBELL_AUDIO_DUMP=dir). The output tap is
    // an SPSC ring the callback pushes the device mix into; drained to the
    // WAV file from this thread each tick.
    let mut audio_dump = AudioDump::from_env();
    let (mut tap_cons, tap_prod) = if audio_dump.is_some() {
        let rb = HeapRb::<i16>::new(BUF_CAP * 4);
        let (p, c) = rb.split();
        (Some(c), Some(Arc::new(std::sync::Mutex::new(p))))
    } else {
        (None, None)
    };
    let main_pull = Arc::new(OutputPullStats::new(tap_prod));
    let stream_pull = Arc::new(OutputPullStats::new(None));

    let (mut output_stream, mut output_sample_rate) = match build_output_stream(
        &host,
        initial_output_device.as_deref(),
        Arc::clone(&peers),
        Arc::clone(&stream_cons),
        Arc::clone(&stream_stereo),
        Arc::clone(&render_ref_prod),
        Arc::clone(&main_pull),
        &event_tx,
    ) {
        Some((stream, rate, _ch)) => (Some(stream), rate),
        None => {
            // Stay alive in a degraded "no output" state instead of
            // killing the pipeline thread. The user might still be
            // able to recover by picking a device from Settings →
            // Audio, which sends a SetOutputDevice ControlMessage —
            // but only if we're still here to receive it. Without
            // this, the engine becomes a zombie holding a control_tx
            // whose receiver was dropped, and the device picker has
            // no effect (silently swallowed SendError). Output stays
            // None; control loop runs; hot-swap to a real device on
            // user action is identical to any other SetOutputDevice
            // flow.
            let _ = event_tx.send(VoiceEvent::Error(
                "No audio output device — pick one in Settings → Audio".to_string(),
            ));
            (None, SAMPLE_RATE)
        }
    };

    // ── Build input (capture) stream ──────────────────────────────────────────
    let (mut input_stream_opt, mut input_sample_rate): (Option<cpal::Stream>, u32) = match build_input_stream(
        &host,
        initial_input_device.as_deref(),
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
        log::info!("[pipeline] Capture resampler: {}Hz → {}Hz", input_sample_rate, SAMPLE_RATE);
        Some(make_sinc_resampler(input_sample_rate, SAMPLE_RATE, 480, 1))
    };
    let mut capture_accum: Vec<f64> = Vec::new();

    // Voice resampling is per-peer now — each PeerAudio owns its own resampler
    // to output_sample_rate. Updated on device hot-swap via set_output_rate().

    // Playback stream: 48kHz → output_device_rate (stereo)
    let mut playback_stream_resampler = if output_sample_rate == SAMPLE_RATE {
        None
    } else {
        log::info!("[pipeline] Playback stream resampler: {}Hz → {}Hz (stereo)", SAMPLE_RATE, output_sample_rate);
        Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2))
    };
    let mut playback_stream_accum_l: Vec<f64> = Vec::new();
    // Reusable resampler input chunks (see peer.rs resamp_scratch).
    let mut playback_stream_scratch_l: Vec<f64> = Vec::new();
    let mut playback_stream_scratch_r: Vec<f64> = Vec::new();
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
    // When the gate closes we don't stop dead: we send a few frames of
    // *encoded* silence at normal cadence first, so the receiver's decoder
    // renders the voice→silence transition itself (MDCT overlap / LPC decay)
    // and its playback ring runs out on true zeros. Stopping on the last
    // voice frame left the receiver to cut the waveform wherever it happened
    // to be — an audible tick at the end of every talkspurt, louder for
    // peers the listener has boosted.
    const GATE_TAIL_FRAMES: u32 = 3; // 60ms
    let mut gate_tail_remaining: u32 = 0;
    // The most recent processed frame that was NOT transmitted; sent as
    // pre-roll when the gate opens so a word's attack isn't chopped.
    let mut gate_pre_roll: Option<[i16; FRAME_SIZE]> = None;

    let mut sequence: u16 = 0;
    // Silence-transmission keepalive. When we are not actively transmitting
    // voice (muted, VAD gate closed, or no microphone) we drop from 50 pps to
    // one packet per KEEPALIVE_INTERVAL — just enough to keep the server's UDP
    // endpoint mapping for us alive and the NAT binding open, without flooding
    // the relay with silent packets every listener discards. See the send block.
    let mut last_voice_send = Instant::now();
    const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);
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

    // ── Playback-ring pacing ────────────────────────────────────────────
    // Frames are decoded into each peer's ring when the ring drops below a
    // low-water mark, not on a free-running 20ms clock. The old clock-paced
    // drain kept at most one frame in the ring while the device callback
    // pulls its whole period at once (26ms on a stock PipeWire desktop), so
    // whether a callback found the ring short was down to phase luck — and
    // silence gating re-rolls that phase at every talkspurt. Pacing on ring
    // level also absorbs device-vs-wall-clock drift in the jitter buffer,
    // where it's handled gracefully, instead of in the ring, where it isn't.
    // The low-water mark is the device's observed per-callback pull plus one
    // loop tick of margin; until a pull has been observed assume 20ms.
    let mut main_pull_frames: usize = 0;
    let mut stream_pull_frames: usize = 0;
    let mut last_pull_sample = Instant::now();
    let pull_sample_interval = Duration::from_secs(2);
    fn low_water_samples(pull_frames: usize, rate: u32, channels: usize) -> usize {
        let pull = if pull_frames > 0 { pull_frames } else { (rate / 50) as usize };
        (pull + (rate / 100) as usize) * channels
    }
    // Frames a cold start must hold beyond the jitter target so that, once
    // the ring is primed, the jitter buffer still holds `target`. The ring
    // sits anywhere in [low_water - pull, low_water + frame) between ticks,
    // so budget its top of range: low_water plus one frame.
    fn prime_frames(low_water: usize, rate: u32, channels: usize) -> usize {
        let frame = ((FRAME_SIZE as u64 * rate as u64 / SAMPLE_RATE as u64) as usize * channels).max(1);
        (low_water + frame - 1) / frame + 1
    }
    // Bound on frames decoded per peer per tick (a cold-start prime or a
    // post-stall catch-up); keeps one tick short even with many peers.
    const MAX_FRAMES_PER_TICK: usize = 8;

    // AEC render reference accumulator: ONE 20ms playback frame (mono, 48kHz),
    // summed across all peers + stream audio and flushed to `render_ref_prod`
    // at a fixed 20ms cadence (`render_flush_interval`, section 4c). Fixing the
    // flush rate is the whole point: the old code pushed a frame per peer PER
    // LOOP ITERATION, so with 2+ peers decoding on different ~5ms iterations the
    // render reference was fed 2–3× faster than the mic capture, wrecking AEC3's
    // render/capture delay alignment — echo cancellation broke in 3+ person
    // calls. Now it is exactly one 960-sample frame per 20ms, whatever the peer
    // count.
    let mut render_sum = [0.0f32; FRAME_SIZE];
    let mut last_render_flush = Instant::now();
    let render_flush_interval = Duration::from_millis(20);
    // Decoded stream-audio frames (mono 48kHz, volume applied) awaiting the
    // render flush — same role as PeerAudio::render_fifo for voice.
    let mut stream_render_fifo: std::collections::VecDeque<[f32; FRAME_SIZE]> = std::collections::VecDeque::new();
    const RENDER_FIFO_MAX: usize = 32;

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
                log::info!("[pipeline] Voice processor built: aec={}, ns_level={}, agc={}", aec, ns_level, agc);
                Some(processor)
            }
            Err(e) => {
                log::warn!("[pipeline] Failed to build voice processor: {}", e);
                None
            }
        }
    }

    fn build_rnnoise(ns_level: u8) -> Option<Box<nnnoiseless::DenoiseState<'static>>> {
        if ns_level == 0 {
            return None;
        }
        log::info!("[pipeline] RNNoise deep-learning noise suppressor enabled (level={})", ns_level);
        Some(nnnoiseless::DenoiseState::new())
    }

    // AEC render reference accumulator (10ms = 480 samples at 48kHz)
    const AEC_FRAME_SIZE: usize = 480; // 10ms at 48kHz
    let mut render_ref_accum: Vec<f32> = Vec::with_capacity(AEC_FRAME_SIZE * 2);

    // ── Dedicated voice UDP recv thread ─────────────────────────────────────
    // Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice socket
    // and forwards them to the audio processing thread. Video packets arrive
    // on a separate media socket handled by the video recv thread in mod.rs.
    //
    // The thread is joined on `'main loop` exit (Shutdown) below — we
    // signal it via voice_recv_stop, drop our channel-receiver end so
    // its next try_send sees Disconnected, and then join. Without the
    // stop flag the thread can sit indefinitely in socket.recv() when
    // no packets are flowing (silent channel, no peers).
    let (audio_pkt_tx, audio_pkt_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1024);
    let recv_drops = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let voice_recv_stop = Arc::new(AtomicBool::new(false));
    let recv_socket = Arc::clone(&socket);
    let recv_event_tx = event_tx.clone();
    let recv_drops_clone = Arc::clone(&recv_drops);
    let voice_recv_stop_thread = Arc::clone(&voice_recv_stop);
    let voice_recv_handle = std::thread::Builder::new()
        .name("decibell-voice-recv".to_string())
        .spawn(move || {
            voice_recv_thread(
                recv_socket,
                audio_pkt_tx,
                recv_event_tx,
                recv_drops_clone,
                voice_recv_stop_thread,
            );
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
                Ok(ControlMessage::SetVoiceBitrate(bps)) => {
                    log::info!("[pipeline] Live voice bitrate change: {} bps", bps);
                    encoder.set_bitrate(bps);
                }
                Ok(ControlMessage::SetStreamVolume(v)) => {
                    stream_volume = v.clamp(0.0, 1.0);
                }
                Ok(ControlMessage::SetStreamStereo(enabled)) => {
                    stream_stereo.store(enabled, std::sync::atomic::Ordering::Relaxed);
                }
                Ok(ControlMessage::SetUserVolume(username, gain)) => {
                    log::info!("[pipeline] SetUserVolume: '{}' → gain={:.3}", username, gain);
                    user_volumes.insert(username, gain);
                }
                Ok(ControlMessage::SetInputDevice(name)) => {
                    log::info!("[pipeline] Hot-swapping input device to: {:?}", name);
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
                                log::info!("[pipeline] Input device {}Hz — passthrough", rate);
                            } else {
                                capture_resampler = Some(make_sinc_resampler(rate, SAMPLE_RATE, 480, 1));
                                log::info!("[pipeline] Input device {}Hz — resampler to {}Hz", rate, SAMPLE_RATE);
                            }
                        }
                        None => {
                            input_stream_opt = None;
                            log::info!("[pipeline] Warning: no input device after hot-swap");
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
                    log::info!("[pipeline] Hot-swapping output device to: {:?}", name);
                    output_stream = None; // drop old stream
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    for peer in remote_peers.values_mut() { peer.drain_ring(); }
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
                    if separate_stream_enabled {
                        match build_voice_output_stream(&host, name.as_deref(), Arc::clone(&peers), Arc::clone(&render_ref_prod), Arc::clone(&main_pull), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => log::info!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    } else {
                        match build_output_stream(&host, name.as_deref(), Arc::clone(&peers), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), Arc::clone(&main_pull), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => log::info!("[pipeline] Warning: no output device after hot-swap"),
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
                    log::info!("[pipeline] Separate stream output: enabled={}, device={:?}", enabled, device);
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
                        if let Some((stream, rate, _ch)) = build_voice_output_stream(&host, None, Arc::clone(&peers), Arc::clone(&render_ref_prod), Arc::clone(&main_pull), &event_tx) {
                            output_sample_rate = rate;
                            output_stream = Some(stream);
                        }
                        // Stream output: stream-only on separate device (may have different rate)
                        if let Some((stream, rate, _ch)) = build_stream_output_stream(&host, device.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&stream_pull), &event_tx) {
                            stream_output_sample_rate = rate;
                            stream_output = Some(stream);
                        }
                    } else {
                        // Back to mixed mode — stream plays on same device as voice
                        if let Some((stream, rate, _ch)) = build_output_stream(&host, None, Arc::clone(&peers), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), Arc::clone(&main_pull), &event_tx) {
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
                        log::info!("[pipeline] Hot-swapping stream output device to: {:?}", name);
                        stream_output_device_name = name.clone();
                        stream_output = None;
                        // Drain stream ring buffer
                        { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                        playback_stream_accum_l.clear();
                        playback_stream_accum_r.clear();
                        if let Some((stream, rate, _ch)) = build_stream_output_stream(&host, name.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&stream_pull), &event_tx) {
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
                    log::info!("[pipeline] AEC enabled={}", enabled);
                    aec_enabled = enabled;
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Ok(ControlMessage::SetNoiseSuppressionLevel(level)) => {
                    log::info!("[pipeline] NS level={}", level);
                    ns_level = level;
                    rnnoise = build_rnnoise(ns_level);
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Ok(ControlMessage::SetAgcEnabled(enabled)) => {
                    log::info!("[pipeline] AGC enabled={}", enabled);
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
                let gate_was_open = gate_open;
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
                let gate_opened = !gate_was_open && gate_open;
                let gate_closed = gate_was_open && !gate_open;
                if gate_closed {
                    gate_tail_remaining = GATE_TAIL_FRAMES;
                }

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

                // ── Gate edges ───────────────────────────────────────────
                // A 20ms-grid gate chops the waveform at its edges, and the
                // codec only smooths ~2.5ms of that:
                //  - Opening: the frame that crossed the threshold is already
                //    loud (a word's attack rises 30dB inside one frame), so the
                //    listener gets silence → loud with the attack cut off — a
                //    pop at every word start. Fix: pre-roll. Send the previous,
                //    sub-threshold frame first (one frame of lookahead paid
                //    only at talkspurt starts, never in steady state) with a
                //    10ms fade-in on it, so the natural attack is transmitted.
                //  - Closing: after the hang the signal is the room's noise
                //    floor, often lifted by AGC; a zero frame after it is a
                //    step. Fix: the three tail frames are the real signal
                //    faded out over 60ms, not encoded silence.
                const GATE_FADE_IN: usize = 480; // 10ms
                let mut pre_roll_frame: Option<[i16; FRAME_SIZE]> = None;
                if gate_opened {
                    match gate_pre_roll.take() {
                        Some(mut pr) => {
                            for (i, s) in pr.iter_mut().take(GATE_FADE_IN).enumerate() {
                                *s = (*s as f32 * (i as f32 / GATE_FADE_IN as f32)) as i16;
                            }
                            pre_roll_frame = Some(pr);
                        }
                        None => {
                            // Nothing held back (previous frame was sent as a
                            // tail frame, or first frame ever): ramp this one.
                            for (i, s) in frame.iter_mut().take(GATE_FADE_IN).enumerate() {
                                *s = (*s as f32 * (i as f32 / GATE_FADE_IN as f32)) as i16;
                            }
                        }
                    }
                }
                let tail_index = if !transmit_voice && gate_tail_remaining > 0 {
                    Some(GATE_TAIL_FRAMES - gate_tail_remaining) // 0, 1, 2
                } else {
                    None
                };
                if let Some(t) = tail_index {
                    let n = frame.len() as f32;
                    let g0 = 1.0 - t as f32 / GATE_TAIL_FRAMES as f32;
                    let g1 = 1.0 - (t + 1) as f32 / GATE_TAIL_FRAMES as f32;
                    for (i, s) in frame.iter_mut().enumerate() {
                        let g = g0 + (g1 - g0) * (i as f32 / n);
                        *s = (*s as f32 * g) as i16;
                    }
                }

                // ── Silence-transmission gating ──────────────────────────
                // When actively transmitting voice, send every 20ms frame.
                // Otherwise (muted, VAD gate closed, or the speech tail ended)
                // fall back to one keepalive per KEEPALIVE_INTERVAL so we stop
                // spraying the relay with silent packets. `transmit_voice`
                // already accounts for mute (the gate is forced closed while
                // muted) and the 200ms gate-hang, so brief inter-word pauses
                // keep transmitting and only sustained silence throttles.
                let should_send = if transmit_voice {
                    last_voice_send = loop_start;
                    true
                } else if tail_index.is_some() {
                    gate_tail_remaining -= 1;
                    last_voice_send = loop_start;
                    true
                } else if last_voice_send.elapsed() >= KEEPALIVE_INTERVAL {
                    last_voice_send = loop_start;
                    true
                } else {
                    false
                };
                // Hold back an unsent frame as next talkspurt's pre-roll.
                gate_pre_roll = if transmit_voice || tail_index.is_some() { None } else { Some(frame) };

                if should_send {
                    if let Some(d) = audio_dump.as_mut() {
                        if gate_opened { d.event(&format!("TX gate open rms={:.0}dB pre_roll={}", rms_db, pre_roll_frame.is_some())); }
                        if tail_index == Some(0) { d.event(&format!("TX gate close (60ms fade) rms={:.0}dB", rms_db)); }
                    }
                    // Flags byte: muted | deafened | silence (tail / keepalive)
                    let flags = if muted { FLAG_MUTED } else { 0 }
                        | if deafened { FLAG_DEAFENED } else { 0 }
                        | if transmit_voice { 0 } else { FLAG_SILENCE };
                    // Pre-roll goes out first, then this frame; both as voice.
                    let frames_to_send: [Option<(&[i16; FRAME_SIZE], bool)>; 2] = [
                        pre_roll_frame.as_ref().map(|f| (f, true)),
                        Some((&frame, transmit_voice || tail_index.is_some())),
                    ];
                    for (f, carries_audio) in frames_to_send.into_iter().flatten() {
                        // Encode real audio for voice and tail frames; true
                        // silence for keepalives so nothing leaks and Opus
                        // DTX keeps them tiny.
                        let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                        let encode_result = if carries_audio {
                            encoder.encode(f, &mut opus_out)
                        } else {
                            encoder.encode_silence(&mut opus_out)
                        };
                        match encode_result {
                            Ok(len) => {
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
                    }
                }
            } else if input_stream_opt.is_none() {
                // No mic — send a keepalive at KEEPALIVE_INTERVAL (NOT every
                // ~5ms loop iteration) so the server keeps our endpoint mapping
                // and we can still hear other participants.
                if last_voice_send.elapsed() >= KEEPALIVE_INTERVAL {
                    last_voice_send = loop_start;
                    let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                    if let Ok(len) = encoder.encode_silence(&mut opus_out) {
                        let flags = FLAG_MUTED | FLAG_SILENCE; // no mic = effectively muted
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
                                    log::info!("[pipeline] New remote peer '{}' detected (type={}, seq={}, payload={}B)",
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
                                // Emit mute/deafen only on an actual change —
                                // these flags ride every audio packet (~50/s per
                                // talking peer) but almost never change, so
                                // forwarding each to the JS event bridge floods
                                // the IPC channel for nothing.
                                let peer_muted = flags & FLAG_MUTED != 0;
                                let peer_deafened = flags & FLAG_DEAFENED != 0;
                                if peer.last_reported_state != Some((peer_muted, peer_deafened)) {
                                    peer.last_reported_state = Some((peer_muted, peer_deafened));
                                    let _ = event_tx.send(VoiceEvent::UserStateChanged(
                                        username.clone(), peer_muted, peer_deafened,
                                    ));
                                }

                                let is_silence = flags & FLAG_SILENCE != 0;
                                peer.voice_jitter.push(pkt.sequence, opus_data.to_vec(), is_silence);
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
                                        // Stream audio is continuous and rarely
                                        // quiet, so the quiet-frame-first trim
                                        // may never find a frame to drop; cap
                                        // the backlog tightly (splice past
                                        // +60ms over target) so it stays in
                                        // step with the immediately-painted
                                        // video. Voice keeps the default.
                                        peer.stream_jitter.set_hard_excess(3);
                                    }

                                    // Straight into the jitter buffer on every
                                    // platform. The Linux-only 1s A/V-sync hold
                                    // queue that used to sit here compensated
                                    // for the Tauri-era MSE video buffer; with
                                    // WebCodecs video painting immediately it
                                    // had become a pure audio-behind-video lag
                                    // (see peer.rs).
                                    peer.stream_jitter.push(pkt.sequence, pkt.payload_data().to_vec(), false);
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
        // Clock-paced fallback only: cap how far behind drain times can fall
        // so a stall doesn't fire dozens of frames in one burst.
        let max_behind = Duration::from_millis(100);

        // Refresh the device pull estimate every couple of seconds.
        if drain_now.duration_since(last_pull_sample) >= pull_sample_interval {
            last_pull_sample = drain_now;
            let m = main_pull.take_recent_max();
            if m > 0 && m != main_pull_frames {
                log::info!("[pipeline] Output pull size: {} frames @ {}Hz", m, output_sample_rate);
                main_pull_frames = m;
            }
            // A callback gap of 3+ pulls means the device stalled under us
            // (xrun) — that's a pop no amount of ring priming prevents.
            let gap_us = main_pull.take_max_gap_us();
            if main_pull_frames > 0 && gap_us > 3 * (main_pull_frames as u64 * 1_000_000 / output_sample_rate as u64) {
                log::warn!("[pipeline] Output callback gap {:.1}ms (pull is {:.1}ms) — device xrun?",
                    gap_us as f64 / 1000.0, main_pull_frames as f64 * 1000.0 / output_sample_rate as f64);
                if let Some(d) = audio_dump.as_mut() { d.event(&format!("OUTPUT CALLBACK GAP {:.1}ms", gap_us as f64 / 1000.0)); }
            }
            if let Some(d) = audio_dump.as_mut() { d.flush(); }
            let m = stream_pull.take_recent_max();
            if m > 0 && m != stream_pull_frames {
                log::info!("[pipeline] Stream output pull size: {} frames @ {}Hz", m, stream_output_sample_rate);
                stream_pull_frames = m;
            }
        }

        // Voice rings are paced by their consumer when there is one and we're
        // actually feeding them. With no output stream, or while deafened
        // (decode for the speaking indicator, never push), the ring would
        // never move — fall back to the 20ms clock.
        let voice_ring_paced = output_stream.is_some() && !deafened;
        let voice_low_water = low_water_samples(main_pull_frames, output_sample_rate, 1);
        let voice_prime = prime_frames(voice_low_water, output_sample_rate, 1);

        let stream_is_stereo = stream_stereo.load(std::sync::atomic::Ordering::Relaxed);
        let stream_channels = if stream_is_stereo { 2 } else { 1 };
        let stream_ring_paced = if separate_stream_enabled { stream_output.is_some() } else { output_stream.is_some() };
        let stream_low_water = low_water_samples(
            if separate_stream_enabled { stream_pull_frames } else { main_pull_frames },
            stream_output_sample_rate, stream_channels,
        );
        let stream_prime = prime_frames(stream_low_water, stream_output_sample_rate, stream_channels);

        for (username, peer) in remote_peers.iter_mut() {
            if drain_now.duration_since(peer.voice_drain_time) > max_behind {
                peer.voice_drain_time = drain_now - max_behind;
            }
            if drain_now.duration_since(peer.stream_drain_time) > max_behind {
                peer.stream_drain_time = drain_now - max_behind;
            }
            peer.voice_jitter.set_prime_frames(if voice_ring_paced { voice_prime } else { 0 });

            // ── Voice jitter buffer ──
            let mut produced = 0usize;
            loop {
                let due = if voice_ring_paced {
                    peer.ring_len() < voice_low_water
                } else {
                    drain_now.duration_since(peer.voice_drain_time) >= frame_dur
                };
                if !due || produced >= MAX_FRAMES_PER_TICK { break; }
                if !voice_ring_paced { peer.voice_drain_time += frame_dur; }

                let frame = peer.voice_jitter.drain();
                if let Some(d) = audio_dump.as_mut() {
                    match &frame {
                        Frame::Expand => d.event(&format!("{} EXPAND (plc) ring={} tgt={}", username, peer.ring_len(), peer.voice_jitter.target())),
                        Frame::Lost => d.event(&format!("{} LOST (fec/plc) jb={}", username, peer.voice_jitter.len())),
                        Frame::Idle if !peer.onset_pending => d.event(&format!("{} idle ring={} under={}", username, peer.ring_len(), peer.voice_jitter.underruns)),
                        _ => {}
                    }
                }
                if frame == Frame::Idle {
                    if !peer.onset_pending {
                        // First idle tick after audio: play out the
                        // resampler's remainder so the tail ends cleanly.
                        peer.flush_tail();
                    }
                    peer.onset_pending = true;
                    // Nothing to play: cold, the sender paused (gate tail /
                    // keepalive), or an underrun's PLC expansion ran out.
                    if !peer.voice_underrun_logged && peer.voice_jitter.underruns > 0 {
                        log::debug!("[pipeline] Voice idle for peer '{}' (underruns={}, expand={}, tgt={})",
                            username, peer.voice_jitter.underruns, peer.voice_jitter.expand_frames, peer.voice_jitter.target());
                        peer.voice_underrun_logged = true;
                    }
                    // Feed the speaking detector a silent tick at frame
                    // cadence so the ring clears instead of sticking lit —
                    // a silence-gated sender no longer streams low-RMS
                    // frames to clear it for us.
                    if drain_now.duration_since(peer.last_silent_tick) >= frame_dur {
                        peer.last_silent_tick = drain_now;
                        if let Some(state) = peer.speaking.process_threshold(false) {
                            let _ = event_tx.send(VoiceEvent::SpeakingChanged(username.clone(), state));
                        }
                    }
                    if !voice_ring_paced { peer.voice_drain_time = drain_now; }
                    break;
                }
                produced += 1;
                let mut pcm = [0i16; FRAME_SIZE];
                let decode_ok = match &frame {
                    Frame::Packet(data) => peer.decoder.decode(data, &mut pcm).is_ok(),
                    Frame::Lost => {
                        // Packet lost with later frames buffered — try FEC
                        // from the next packet, else plain PLC.
                        if let Some(next_data) = peer.voice_jitter.peek_next() {
                            peer.decoder.decode_fec(next_data, &mut pcm).is_ok()
                        } else {
                            peer.decoder.decode(&[], &mut pcm).is_ok()
                        }
                    }
                    // Ran dry mid-talkspurt: conceal while the late packet
                    // lands (bounded by the jitter buffer).
                    Frame::Expand => peer.decoder.decode(&[], &mut pcm).is_ok(),
                    Frame::Idle => unreachable!(),
                };
                if !decode_ok { continue; }
                // Last voice frame before the sender's gate closed (the next
                // buffered packet is a flagged tail frame): fade it out over
                // the frame. Senders from 0.7.5 fade their own tail, but a
                // 0.7.4 sender follows its noise floor with a hard zero frame
                // — audible as a tick after the person stops, especially on a
                // boosted peer — and this smooths it on the listening side.
                if matches!(frame, Frame::Packet(_))
                    && !peer.voice_jitter.last_was_silence()
                    && peer.voice_jitter.next_is_silence() == Some(true)
                {
                    let n = pcm.len() as f32;
                    for (i, s) in pcm.iter_mut().enumerate() {
                        *s = (*s as f32 * (1.0 - i as f32 / n)) as i16;
                    }
                    if let Some(d) = audio_dump.as_mut() {
                        d.event(&format!("{} last voice frame (fade-out)", username));
                    }
                }
                // Fade PLC expansion to silence across the episode: Opus PLC
                // alone keeps a voiced tail pitch-repeating at useful level for
                // 100ms+, which — when a pre-0.7.4 sender simply stops — is
                // heard as a short buzz/tick after the person stops talking.
                // A late packet that resumes playback comes back at full
                // level; Opus smooths that join itself.
                if frame == Frame::Expand {
                    let k = peer.voice_jitter.expand_index() as f32;
                    let g = (1.0 - k / (super::jitter::MAX_EXPAND_FRAMES as f32 + 1.0)).powi(2);
                    for s in pcm.iter_mut() { *s = (*s as f32 * g) as i16; }
                }

                let rms = {
                    let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
                    (sum_sq / pcm.len() as f64).sqrt() as f32
                };
                let rms_db = if rms > 0.0 { 20.0 * (rms / 32768.0).log10() } else { -96.0 };
                if let Some(state) = peer.speaking.process_threshold(rms_db >= -50.0) {
                    let _ = event_tx.send(VoiceEvent::SpeakingChanged(username.clone(), state));
                }
                // Latency trim: over target, drop quiet frames (decoded, so
                // the decoder state stays continuous) instead of playing them.
                if peer.voice_jitter.should_drop_excess(rms_db < -45.0) {
                    if let Some(d) = audio_dump.as_mut() {
                        d.event(&format!("{} DROP excess={} rms={:.0}dB", username, peer.voice_jitter.excess(), rms_db));
                    }
                    if !voice_ring_paced { peer.voice_drain_time -= frame_dur; }
                    continue;
                }
                if !deafened {
                    let gain = user_volumes.get(username.as_str()).copied().unwrap_or(1.0);
                    let mut f32_frame = [0.0f32; FRAME_SIZE];
                    for (i, &s) in pcm.iter().enumerate() {
                        f32_frame[i] = (s as f32 / 32768.0) * gain;
                    }
                    // Talkspurt onset into an empty ring: the sender's VAD
                    // gate opened on a 20ms frame boundary, so the waveform
                    // can start anywhere — and a 0.7.4 sender's first frame
                    // is already mid-attack. Ramp the whole first frame so
                    // the listener never gets a step (or an 8ms click-length
                    // ramp) out of digital silence. Senders from 0.7.5 send a
                    // pre-roll frame first, so this only shapes quiet audio.
                    if peer.onset_pending && peer.ring_len() == 0 {
                        const FADE: usize = 960; // 20ms @ 48kHz — the whole first frame
                        for (i, s) in f32_frame.iter_mut().take(FADE).enumerate() {
                            *s *= i as f32 / FADE as f32;
                        }
                        if let Some(d) = audio_dump.as_mut() {
                            d.event(&format!("{} onset (fade-in) tgt={} jb={}", username, peer.voice_jitter.target(), peer.voice_jitter.len()));
                        }
                    }
                    peer.onset_pending = false;
                    if let Some(d) = audio_dump.as_mut() {
                        d.peer_frame(username, &f32_frame);
                    }
                    // Queue for the AEC render reference (mono, pre-resample);
                    // section 4c sums one frame per peer per 20ms.
                    if aec_enabled {
                        if peer.render_fifo.len() >= RENDER_FIFO_MAX { peer.render_fifo.pop_front(); }
                        peer.render_fifo.push_back(f32_frame);
                    }
                    peer.push_voice_frame(&f32_frame);
                }
            }

            // ── Stream audio jitter buffer ──
            if let Some(ref mut decoder) = peer.stream_audio_decoder {
                peer.stream_jitter.set_prime_frames(if stream_ring_paced { stream_prime } else { 0 });
                let mut produced = 0usize;
                loop {
                    let due = if stream_ring_paced {
                        let len = stream_prod.lock().map(|p| p.occupied_len()).unwrap_or(usize::MAX);
                        len < stream_low_water
                    } else {
                        drain_now.duration_since(peer.stream_drain_time) >= frame_dur
                    };
                    if !due || produced >= MAX_FRAMES_PER_TICK { break; }
                    if !stream_ring_paced { peer.stream_drain_time += frame_dur; }

                    let frame = peer.stream_jitter.drain();
                    if frame == Frame::Idle {
                        if !stream_ring_paced { peer.stream_drain_time = drain_now; }
                        break;
                    }
                    produced += 1;
                    let mut pcm = [0i16; STEREO_FRAME_SAMPLES];
                    let decode_ok = match &frame {
                        Frame::Packet(data) => decoder.decode(data, &mut pcm).is_ok(),
                        Frame::Lost | Frame::Expand => decoder.decode(&[], &mut pcm).is_ok(), // PLC
                        Frame::Idle => unreachable!(),
                    };
                    if !decode_ok { continue; }
                    let quiet = {
                        let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
                        let rms = (sum_sq / pcm.len() as f64).sqrt() as f32;
                        rms < 32768.0 * 0.0056 // ≈ -45 dBFS
                    };
                    if peer.stream_jitter.should_drop_excess(quiet) {
                        if !stream_ring_paced { peer.stream_drain_time -= frame_dur; }
                        continue;
                    }
                    if playback_stream_resampler.is_none() {
                        // Output device is 48kHz — push directly
                        if let Ok(mut prod) = stream_prod.lock() {
                            for i in 0..STEREO_FRAME_SIZE {
                                let l = pcm[i * 2] as i32;
                                let r = pcm[i * 2 + 1] as i32;
                                if stream_is_stereo {
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
                            playback_stream_scratch_l.clear();
                            playback_stream_scratch_l.extend(playback_stream_accum_l.drain(..needed));
                            playback_stream_scratch_r.clear();
                            playback_stream_scratch_r.extend(playback_stream_accum_r.drain(..needed));
                            if let Ok(out) = resampler.process(&[&playback_stream_scratch_l, &playback_stream_scratch_r], None) {
                                if let Ok(mut prod) = stream_prod.lock() {
                                    let len = out[0].len().min(out[1].len());
                                    for i in 0..len {
                                        if stream_is_stereo {
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
                    // Queue stream audio for the AEC render reference (mono,
                    // volume applied); summed with voice in 4c.
                    if aec_enabled {
                        let mut mono = [0.0f32; FRAME_SIZE];
                        for i in 0..STEREO_FRAME_SIZE {
                            let l = pcm[i * 2] as f32;
                            let r = pcm[i * 2 + 1] as f32;
                            mono[i] = ((l + r) / 2.0) * stream_volume / 32768.0;
                        }
                        if stream_render_fifo.len() >= RENDER_FIFO_MAX { stream_render_fifo.pop_front(); }
                        stream_render_fifo.push_back(mono);
                    }
                }
            }
        }

        // 4b'. Debug dump: device output tap + ring-dry detection ─────────
        if let Some(d) = audio_dump.as_mut() {
            if let Some(c) = tap_cons.as_mut() {
                let mut buf: Vec<i16> = Vec::with_capacity(c.occupied_len());
                while let Some(s) = c.try_pop() { buf.push(s); }
                if !buf.is_empty() { d.output_samples(&buf, output_sample_rate); }
            }
            for (username, peer) in remote_peers.iter() {
                if voice_ring_paced && peer.ring_len() == 0
                    && (peer.voice_jitter.is_ready() || peer.voice_jitter.is_expanding())
                {
                    d.event(&format!("{} RING DRY while audio expected (jb={})", username, peer.voice_jitter.len()));
                }
            }
        }

        // 4c. Flush the AEC render reference at the playback frame rate. ────
        // Exactly one 960-sample (20ms) mono frame per render_flush_interval:
        // the sum of the oldest queued frame of every peer plus stream audio.
        // Decoding is paced by the playback ring and bursts at talkspurt
        // starts, so frames are queued (render_fifo) and released here at
        // frame cadence rather than summed as they're decoded. The while loop
        // catches up if the loop fell behind; the 100ms guard caps a burst
        // after a long stall. Keeps render:capture at 1:1 whatever the peer
        // count — the fix for AEC breaking with 3+ people in the channel.
        if aec_enabled {
            if last_render_flush.elapsed() > Duration::from_millis(100) {
                last_render_flush = drain_now;
            }
            while last_render_flush.elapsed() >= render_flush_interval {
                last_render_flush += render_flush_interval;
                render_sum.fill(0.0);
                for peer in remote_peers.values_mut() {
                    if let Some(f) = peer.render_fifo.pop_front() {
                        for (dst, &s) in render_sum.iter_mut().zip(f.iter()) { *dst += s; }
                    }
                }
                if let Some(f) = stream_render_fifo.pop_front() {
                    for (dst, &s) in render_sum.iter_mut().zip(f.iter()) { *dst += s; }
                }
                if let Ok(mut rr_prod) = render_ref_prod.lock() {
                    for &s in render_sum.iter() {
                        let _ = rr_prod.try_push(s);
                    }
                }
            }
        } else if !stream_render_fifo.is_empty() {
            stream_render_fifo.clear();
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
                format!("{}(j={:.1}ms tgt={} buf={} ring={} plc={} exp={} under={} drop={})",
                    n, p.voice_jitter.jitter_ms(), p.voice_jitter.target(), p.voice_jitter.len(), p.ring_len(),
                    p.voice_jitter.plc_frames, p.voice_jitter.expand_frames, p.voice_jitter.underruns,
                    p.voice_jitter.dropped_frames)
            }).collect();
            log::info!("[pipeline] diag: peers=[{}], pull={}fr low_water={} short_pulls={} stream_buf={}/{}, recv_drops={}, pkts_this_iter={}",
                peer_stats.join(", "), main_pull_frames, voice_low_water, main_pull.short_pulls(), stream_fill, BUF_CAP, recv_drop_total, pkt_count_this_iter);
        }

        // 7. Sleep if loop finished under 5ms ──────────────────────────────────
        let elapsed = loop_start.elapsed();
        let target = Duration::from_millis(5);
        if elapsed < target {
            std::thread::sleep(target - elapsed);
        }
    }

    // Streams are dropped here, which stops CPAL.
    // Signal the voice recv thread to exit and join it before letting
    // the audio thread return. Without an explicit stop flag the recv
    // loop could sit in socket.recv() for many seconds waiting for the
    // mpsc try_send Disconnected to surface (only happens on the NEXT
    // packet — silent channels never get one), and `.join()` upstream
    // would block the whole shutdown. Now bounded by the 1ms socket
    // read timeout — exits within one poll iteration.
    voice_recv_stop.store(true, Ordering::Relaxed);
    drop(audio_pkt_rx);
    let _ = voice_recv_handle.join();
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
    stop: Arc<AtomicBool>,
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
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match socket.recv(&mut buf) {
            Ok(n) if n >= 1 => {
                recv_count += 1;
                if recv_log_time.elapsed() >= Duration::from_secs(5) {
                    log::debug!("[voice-recv] 5s stats: packets={}", recv_count);
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
                log::warn!("[voice-recv] Socket error: {}", e);
                break;
            }
        }
    }

    log::info!("[voice-recv] Exiting");
}
