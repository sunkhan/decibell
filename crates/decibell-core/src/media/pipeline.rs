use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ringbuf::{HeapRb, traits::{Consumer, Producer, Split, Observer}};
use rubato::Resampler;

use super::audio_device::{
    make_sinc_resampler, build_input_stream, build_output_stream,
    build_stream_output_stream, OutputHandle, OutputPullStats, PeerList, DEVICE_BUFFER_MS,
};
use super::cue;
use super::jitter::Frame;
use super::media_socket::MediaSocket;
use super::debug_dump::AudioDump;
use super::codec::{
    OpusEncoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE,
    SAMPLE_RATE, STEREO_FRAME_SAMPLES, STEREO_FRAME_SIZE,
};
use super::peer::{PeerAudio, PeerOutput};
use arc_swap::ArcSwap;
use super::packet::{
    UdpAudioPacket, AUDIO_HEADER_SIZE, MAX_PAYLOAD_SIZE, PACKET_TYPE_AUDIO, PACKET_TYPE_PING,
    PACKET_TYPE_STREAM_AUDIO, SENDER_ID_SIZE,
};
#[cfg(not(unix))]
use super::udp_recv::{self, Ready};
#[cfg(not(target_os = "linux"))]
use super::udp_recv::READ_GUARD;
use super::udp_recv::{is_oversized, RecvErrors, Waker, RECV_BUF_SIZE};
use super::speaking::SpeakingDetector;
use super::voice_gate::Gate as VoiceGate;
use super::capture_chain::{self, CaptureChain, TxGate};

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
    /// The OS default capture / render device became a different device
    /// (default_device_watch.rs). Followed only by what's on "Default".
    /// Never sent on Linux, where PipeWire / PulseAudio follow by themselves.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    DefaultInputChanged,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    DefaultOutputChanged,
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
    KeyframeRequested,
    Error(String),
}

/// Bound on frames decoded per peer per tick (a cold-start prime or a
/// post-stall catch-up); keeps one tick short even with many peers.
const MAX_FRAMES_PER_TICK: usize = 8;
/// Decoded frames queued per peer for the AEC render reference.
const RENDER_FIFO_MAX: usize = 32;

// Flags byte prepended to audio payload
const FLAG_MUTED: u8 = 0x01;
const FLAG_DEAFENED: u8 = 0x02;
/// This frame carries no voice: a gate-closed tail frame or a keepalive.
/// Receivers use it to tell "the sender paused" from "a packet is late", and
/// to keep keepalive cadence out of their jitter estimate. Older clients
/// ignore the bit (they only test the two above) — wire-compatible.
pub const FLAG_SILENCE: u8 = 0x04;

/// The main output was replaced (or first built, or lost): interface sounds
/// go to the new one's mix. `old` is the replaced output's bus; with no new
/// output they play on a stream of their own again.
fn main_output_changed(old: Option<Arc<cue::CueBus>>, new: &Option<OutputHandle>) {
    match new.as_ref().and_then(|o| o.cues.as_ref()) {
        Some(bus) => cue::register_live(bus),
        None => {
            if let Some(old) = &old {
                cue::retire_live(old);
            }
        }
    }
}

/// An output stream on its way out (the user picked another device).
pub(crate) struct Retiring {
    out: OutputHandle,
    since: Instant,
}

/// Take `out` out of use without a click: its callback fades to silence
/// over 5 ms (this waits for that: one or two callbacks, so the caller can
/// then empty the rings the callback was reading; 60 ms at most, for a device
/// whose callback has stopped), and the stream stays open, playing silence,
/// until `reap_retired` closes it. Closing a stream drops what the device
/// still had buffered, which used to cut the wave wherever it was.
pub(crate) fn retire_output(out: OutputHandle, retiring: &mut Vec<Retiring>) {
    out.ctl.fade_out_and_wait(0, Duration::from_millis(60));
    retiring.push(Retiring { out, since: Instant::now() });
}

/// Close the retired streams whose device has been handed its buffer's worth
/// of silence since the fade (so the fade itself was played), or that have
/// had half a second (a dead device). Called every tick; nothing waits.
pub(crate) fn reap_retired(retiring: &mut Vec<Retiring>) {
    if retiring.is_empty() {
        return;
    }
    retiring.retain(|r| r.out.ctl.silent_ms() < DEVICE_BUFFER_MS && r.since.elapsed() < Duration::from_millis(500));
}

// Per-peer state lives in `media::peer::PeerAudio`. Voice mixing happens in
// the output audio callback which pulls from each peer's ring buffer.

// ── Main blocking pipeline entry-point ───────────────────────────────────────

/// Runs the audio pipeline on the calling thread (should be a dedicated OS thread).
/// The socket must already be connected to the server (bind + connect done by caller).
/// Video packets are forwarded to `video_packet_tx` for processing on a separate thread,
/// keeping the audio loop fast and preventing video reassembly from causing audio choppiness.
pub fn run_audio_pipeline(
    socket: Arc<MediaSocket>,
    sender_id: String,
    voice_bitrate_bps: i32,
    initial_input_device: Option<String>,
    initial_output_device: Option<String>,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
    ring: Option<crate::media::frame_crypto::SharedKeyRing>,
) {
    use crate::media::frame_crypto::{self, Kind, PACKET_TYPE_AUDIO_SEALED};
    // Community channel (ring present): every voice frame is sealed under
    // the MLS epoch keys and plaintext audio from others is ignored. Until
    // the group's first epoch is ready nothing is sent at all — the sealer
    // returns None and the frame is skipped.
    let voice_sealer = frame_crypto::AudioSealer::new(Kind::Voice);
    let make_audio_packet = |sequence: u16, flagged: &[u8]| -> Option<UdpAudioPacket> {
        match &ring {
            Some(r) => {
                let sealed = voice_sealer.seal(&r.load(), sequence, flagged)?;
                UdpAudioPacket::new_typed(PACKET_TYPE_AUDIO_SEALED, &sender_id, sequence, &sealed)
            }
            None => UdpAudioPacket::new_audio(&sender_id, sequence, flagged),
        }
    };
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

    // The user's device selections (None = the system default), kept so a
    // default-device change only moves what follows the default, and so
    // rebuilding the voice output keeps the chosen device.
    let mut input_device_name = initial_input_device.clone();
    let mut output_device_name = initial_output_device.clone();

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
        Some((Arc::clone(&stream_cons), Arc::clone(&stream_stereo))),
        Arc::clone(&main_pull),
        &event_tx,
    ) {
        Some(out) => {
            let rate = out.rate;
            (Some(out), rate)
        }
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
    // Interface sounds play in this output's mix while it is live (cue.rs).
    main_output_changed(None, &output_stream);

    // ── Build input (capture) stream ──────────────────────────────────────────
    let (mut input_stream_opt, input_sample_rate): (Option<cpal::Stream>, u32) = match build_input_stream(
        &host,
        initial_input_device.as_deref(),
        Arc::clone(&capture_prod),
        None,
    ) {
        Some((s, rate)) => (Some(s), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No microphone found — running in listen-only mode".to_string(),
            ));
            (None, SAMPLE_RATE)
        }
    };

    // ── Capture chain (all DSP off the audio callback threads) ────────────────
    // input_device_rate → 48kHz, 20 ms frames, AEC / AGC / noise suppression.
    let mut chain = CaptureChain::new(input_sample_rate);

    // Voice resampling is per-peer now — each PeerAudio owns its own resampler
    // to output_sample_rate. Updated on device hot-swap via set_output_rate().

    // Playback stream: 48kHz → output_device_rate (stereo)
    let mut stream_out = StreamOut::new(output_sample_rate);

    // Separate stream output (None = disabled, stream mixed into main output)
    let mut stream_output: Option<OutputHandle> = None;
    // Streams switched away from, fading out before they are closed.
    let mut retiring: Vec<Retiring> = Vec::new();
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
    let mut user_volumes: HashMap<String, f32> = HashMap::new(); // media id (uid) → linear gain

    // The transmit gate (hysteresis, hang, fade tail, pre-roll): capture_chain.rs.
    let mut tx_gate = TxGate::new();

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
    // Keyed by the 32-byte sender id off the wire (no String per packet);
    // each PeerAudio carries its id as text (`name`) for events and volumes.
    let mut remote_peers: HashMap<PeerKey, PeerAudio> = HashMap::new();
    let self_key = peer_key(&sender_id);

    // Rebuilds the ArcSwap peer-list snapshot handed to the output callback.
    // Call after any peer insert/remove.
    fn refresh_peer_list(peers: &PeerList, remote_peers: &HashMap<PeerKey, PeerAudio>) {
        let snapshot: Vec<PeerOutput> = remote_peers.values().map(|p| p.output_handle()).collect();
        peers.store(Arc::new(snapshot));
    }


    // The first PING goes out at once: on the community relay it is what
    // binds our voice endpoint (the relay answers an unbound source with a
    // bind challenge that MediaSocket echoes — review S11), so a listener
    // starts hearing the channel one round trip after joining. Until the
    // first echo comes back (proof we're bound) it repeats every 500 ms, so
    // a lost challenge costs half a second rather than a full interval.
    let mut last_ping_time: Option<Instant> = None;
    let ping_interval = Duration::from_secs(3);
    const PING_INTERVAL_UNANSWERED: Duration = Duration::from_millis(500);

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


    // ── Dedicated voice UDP recv thread ─────────────────────────────────────
    // Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice socket
    // and forwards them to the audio processing thread. Video packets arrive
    // on a separate media socket handled by the video recv thread in mod.rs.
    //
    // The thread is joined on `'main loop` exit (Shutdown) below — we
    // set voice_recv_stop and wake its blocked read (`StopOnDrop`), drop
    // our channel-receiver end so its next try_send sees Disconnected, and
    // then join.
    // Datagrams reach this thread through an SPSC byte ring, each as a
    // record [len u16 LE][datagram] pushed in one piece: no allocation per
    // packet (the channel of Vecs it replaced allocated one each).
    let (audio_pkt_tx, mut audio_pkt_rx) = HeapRb::<u8>::new(VOICE_RX_RING_BYTES).split();
    let recv_drops = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let voice_recv_stop = Arc::new(AtomicBool::new(false));
    let voice_recv_waker = match Waker::new() {
        Ok(w) => Arc::new(w),
        Err(e) => {
            log::error!("[voice-recv] waker: {}", e);
            let _ = event_tx.send(VoiceEvent::Error(format!(
                "Voice receive couldn't start ({}) — reconnect to hear others",
                e
            )));
            return;
        }
    };
    let recv_socket = Arc::clone(&socket);
    let recv_event_tx = event_tx.clone();
    let recv_drops_clone = Arc::clone(&recv_drops);
    let voice_recv_stop_thread = Arc::clone(&voice_recv_stop);
    let voice_recv_waker_thread = Arc::clone(&voice_recv_waker);
    let voice_recv_handle = std::thread::Builder::new()
        .name("decibell-voice-recv".to_string())
        .spawn(move || {
            // Guarded like the audio and video threads: a panic here would
            // end incoming voice with nothing on screen.
            let panic_tx = recv_event_tx.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                voice_recv_thread(
                    recv_socket,
                    audio_pkt_tx,
                    recv_event_tx,
                    recv_drops_clone,
                    voice_recv_stop_thread,
                    voice_recv_waker_thread,
                );
            }));
            if let Err(e) = outcome {
                let msg = e
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown error".to_string());
                log::error!("[voice-recv] thread panicked: {}", msg);
                let _ = panic_tx.send(VoiceEvent::Error(format!(
                    "Voice receive crashed ({}) — reconnect to hear others",
                    msg
                )));
            }
        })
        .expect("spawn voice recv thread");
    // Stops the recv thread however this function ends. The normal exit
    // below sets the flag, wakes it and joins; a panic unwinding the
    // pipeline would otherwise leave the thread waiting on the socket forever.
    let stop_recv = StopOnDrop(Arc::clone(&voice_recv_stop), Arc::clone(&voice_recv_waker), Arc::clone(&socket));

    // One datagram at a time, copied out of the byte ring.
    let mut rx_dgram = [0u8; RECV_BUF_SIZE];

    // ── Main loop ─────────────────────────────────────────────────────────────
    'main: loop {
        let loop_start = Instant::now();

        // 1. Drain control messages ────────────────────────────────────────────
        // A default-device change expands into the same hot-swaps a Settings
        // pick would send, queued here and handled first.
        let mut follow_default: std::collections::VecDeque<ControlMessage> = std::collections::VecDeque::new();
        loop {
            let next = match follow_default.pop_front() {
                Some(msg) => Ok(msg),
                None => control_rx.try_recv(),
            };
            match next {
                Ok(ControlMessage::DefaultInputChanged) => {
                    if input_device_name.is_none() {
                        log::info!("[pipeline] System default input changed — following it");
                        follow_default.push_back(ControlMessage::SetInputDevice(None));
                    }
                }
                Ok(ControlMessage::DefaultOutputChanged) => {
                    if output_device_name.is_none() {
                        log::info!("[pipeline] System default output changed — following it");
                        follow_default.push_back(ControlMessage::SetOutputDevice(None));
                    }
                    if separate_stream_enabled && stream_output_device_name.is_none() {
                        follow_default.push_back(ControlMessage::SetStreamOutputDevice(None));
                    }
                }
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
                    // Mid-word: the talkspurt ends on the wire before the old
                    // device closes. The flagged frame makes the listener fade
                    // out the last voice frame it holds; the new device's
                    // first frame then opens a new talkspurt (faded in).
                    if tx_gate.cut() {
                        let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                        if let Ok(len) = encoder.encode_silence(&mut opus_out) {
                            let flags = if muted { FLAG_MUTED } else { 0 }
                                | if deafened { FLAG_DEAFENED } else { 0 }
                                | FLAG_SILENCE;
                            let mut flagged = [0u8; MAX_OPUS_FRAME_SIZE + 1];
                            flagged[0] = flags;
                            flagged[1..1 + len].copy_from_slice(&opus_out[..len]);
                            if let Some(packet) = make_audio_packet(sequence, &flagged[..1 + len]) {
                                let _ = socket.send(&packet.to_bytes());
                            }
                            sequence = sequence.wrapping_add(1);
                            last_voice_send = loop_start;
                        }
                    }
                    input_device_name = name.clone();
                    input_stream_opt = None; // drop old stream
                    { let mut g = capture_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    chain.clear_capture();
                    match build_input_stream(&host, name.as_deref(), Arc::clone(&capture_prod), None) {
                        Some((stream, rate)) => {
                            input_stream_opt = Some(stream);
                            chain.set_input_rate(rate);
                        }
                        None => {
                            input_stream_opt = None;
                            log::info!("[pipeline] Warning: no input device after hot-swap");
                        }
                    }
                    // Reset voice processor state on device change
                    if chain.reset_processor() {
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
                Ok(ControlMessage::SetOutputDevice(name)) => {
                    log::info!("[pipeline] Hot-swapping output device to: {:?}", name);
                    output_device_name = name.clone();
                    // The old stream fades out before the rings it reads are
                    // emptied, and is closed once that has been played.
                    let old = output_stream.take();
                    let old_cues = old.as_ref().and_then(|o| o.cues.clone());
                    if let Some(old) = old {
                        retire_output(old, &mut retiring);
                    }
                    // Stream audio on its own device plays on untouched.
                    if !separate_stream_enabled {
                        { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                        stream_out.clear_accum();
                    }
                    for peer in remote_peers.values_mut() { peer.drain_ring(); }
                    let stream_here = (!separate_stream_enabled).then(|| (Arc::clone(&stream_cons), Arc::clone(&stream_stereo)));
                    match build_output_stream(&host, name.as_deref(), Arc::clone(&peers), stream_here, Arc::clone(&main_pull), &event_tx) {
                        Some(out) => {
                            output_sample_rate = out.rate;
                            output_stream = Some(out);
                        }
                        None => log::info!("[pipeline] Warning: no output device after hot-swap"),
                    }
                    main_output_changed(old_cues, &output_stream);
                    // Update every peer's resampler for the new output rate.
                    for peer in remote_peers.values_mut() {
                        peer.set_output_rate(output_sample_rate);
                    }
                    // Stream resampler: only update if stream plays on the same device
                    if !separate_stream_enabled {
                        stream_output_sample_rate = output_sample_rate;
                        stream_out.set_rate(output_sample_rate);
                    }
                }
                Ok(ControlMessage::SetSeparateStreamOutput(enabled, device)) => {
                    log::info!("[pipeline] Separate stream output: enabled={}, device={:?}", enabled, device);
                    separate_stream_enabled = enabled;
                    stream_output_device_name = device.clone();
                    // Rebuild main output and stream output
                    let old = output_stream.take();
                    let old_cues = old.as_ref().and_then(|o| o.cues.clone());
                    // Both fade together, then each is waited for.
                    if let Some(s) = &stream_output {
                        s.ctl.fade_out();
                    }
                    if let Some(old) = old {
                        retire_output(old, &mut retiring);
                    }
                    if let Some(old) = stream_output.take() {
                        retire_output(old, &mut retiring);
                    }
                    // Drain ring buffers to prevent stale audio causing delay
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    for peer in remote_peers.values_mut() { peer.drain_ring(); }
                    stream_out.clear_accum();
                    if enabled {
                        // Main output: voice-only, on the device the user chose
                        if let Some(out) = build_output_stream(&host, output_device_name.as_deref(), Arc::clone(&peers), None, Arc::clone(&main_pull), &event_tx) {
                            output_sample_rate = out.rate;
                            output_stream = Some(out);
                        }
                        // Stream output: stream-only on separate device (may have different rate)
                        if let Some(out) = build_stream_output_stream(&host, device.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&stream_pull), &event_tx) {
                            stream_output_sample_rate = out.rate;
                            stream_output = Some(out);
                        }
                    } else {
                        // Back to mixed mode — stream plays on same device as voice
                        if let Some(out) = build_output_stream(&host, output_device_name.as_deref(), Arc::clone(&peers), Some((Arc::clone(&stream_cons), Arc::clone(&stream_stereo))), Arc::clone(&main_pull), &event_tx) {
                            output_sample_rate = out.rate;
                            stream_output_sample_rate = out.rate;
                            output_stream = Some(out);
                        }
                    }
                    main_output_changed(old_cues, &output_stream);
                    // Update every peer's resampler for the new voice output rate.
                    for peer in remote_peers.values_mut() {
                        peer.set_output_rate(output_sample_rate);
                    }
                    stream_out.set_rate(stream_output_sample_rate);
                }
                Ok(ControlMessage::SetStreamOutputDevice(name)) => {
                    if separate_stream_enabled {
                        log::info!("[pipeline] Hot-swapping stream output device to: {:?}", name);
                        stream_output_device_name = name.clone();
                        if let Some(old) = stream_output.take() {
                            retire_output(old, &mut retiring);
                        }
                        // Drain stream ring buffer
                        { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                        stream_out.clear_accum();
                        if let Some(out) = build_stream_output_stream(&host, name.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&stream_pull), &event_tx) {
                            stream_output_sample_rate = out.rate;
                            // Rebuild stream resampler for the new device rate
                            stream_out.set_rate(out.rate);
                            stream_output = Some(out);
                        }
                    }
                }
                Ok(ControlMessage::SetAecEnabled(enabled)) => {
                    log::info!("[pipeline] AEC enabled={}", enabled);
                    if chain.set_aec(enabled) {
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
                Ok(ControlMessage::SetNoiseSuppressionLevel(level)) => {
                    log::info!("[pipeline] NS level={}", level);
                    if chain.set_ns_level(level) {
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
                Ok(ControlMessage::SetAgcEnabled(enabled)) => {
                    log::info!("[pipeline] AGC enabled={}", enabled);
                    if chain.set_agc(enabled) {
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // Deafened: the output callback ramps the voices down (and back up)
        // instead of the rings just running out mid-wave.
        if let Some(out) = &output_stream {
            out.ctl.set_voice(!deafened);
        }

        // 1a. Close output streams that were switched away from, once their
        // fade-out has been played.
        reap_retired(&mut retiring);

        // 1b. Feed AEC render reference from decoded voice audio ──────────────
        // Drain the render ref ring buffer and process 10ms chunks through AEC.
        if chain.has_processor() {
            if let Ok(mut rr_cons) = render_ref_cons.lock() {
                chain.feed_render(&mut rr_cons);
            }
        }

        // 2. Capture & encode → send UDP ──────────────────────────────────────
        //
        // The capture ring buffer carries i16 at the input device's native rate.
        // We drain available samples, resample to 48kHz if needed, and accumulate
        // in capture_48k_buf. When we have a full 960-sample frame, encode + send.
        {
            // Drain all available samples from the capture ring buffer and
            // resample them to 48kHz (capture_chain.rs).
            {
                let mut cons = capture_cons.lock().unwrap();
                chain.pull(&mut cons);
            } // release capture_cons lock

            // A full Opus frame (960 samples at 48kHz = 20ms), when captured.
            if let Some(mut frame) = chain.next_frame() {
                // ── Voice processing (AEC / NS / AGC) ────────────────────────
                // Process BEFORE threshold check so AGC-boosted levels are what
                // the threshold sees. Previous order caused AGC to boost quiet
                // audio after the gate had already decided to fade it out,
                // producing robotic popping artifacts.
                if tx_gate.wants_processing(muted) {
                    chain.process(&mut frame);
                }

                // RMS in dB on the PROCESSED frame (post-AGC/NS)
                let rms_db = capture_chain::rms_db(&frame);
                let open_threshold = voice_threshold_db;
                // Push-to-talk / push-to-mute (global hotkeys) force the gate
                // either way (capture_chain.rs `TxGate`, with the edge shaping).
                let hotkey_gate = crate::media::voice_gate::gate();
                let tx = tx_gate.step(&mut frame, rms_db, voice_threshold_db, muted, hotkey_gate);
                let transmit_voice = tx.transmit_voice;
                let tail_index = tx.tail_index;
                let gate_opened = tx.opened;
                let pre_roll_frame = tx.pre_roll;

                // Emit input level for the UI meter (~every 60ms) — only
                // while Settings → Audio's meter has reporting switched on
                // (set_input_level_reporting); nothing else listens.
                input_level_counter += 1;
                if input_level_counter >= 3 {
                    input_level_counter = 0;
                    if crate::events::input_level_reporting_enabled() {
                        let _ = event_tx.send(VoiceEvent::InputLevel(rms_db));
                    }
                }

                // Speaking detection based on threshold (no hysteresis — this is
                // just for the UI speaking ring, user wants it to react immediately)
                // Under push-to-talk the threshold slider is out of play; use
                // the same -50 dB the remote side judges our audio by.
                let local_voice = match hotkey_gate {
                    VoiceGate::Vad => rms_db >= open_threshold,
                    VoiceGate::Open => rms_db >= -50.0,
                    VoiceGate::Closed => false,
                };
                if let Some(state) = local_speaking.process_threshold(!muted && local_voice) {
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), state));
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
                    last_voice_send = loop_start;
                    true
                } else if last_voice_send.elapsed() >= KEEPALIVE_INTERVAL {
                    last_voice_send = loop_start;
                    true
                } else {
                    false
                };

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
                                match make_audio_packet(sequence, &flagged[..1 + len]) {
                                    Some(packet) => {
                                        let _ = socket.send(&packet.to_bytes());
                                    }
                                    // Plain path: unreachable with MAX_OPUS_FRAME_SIZE = cap - 1
                                    // (kept as the explicit no-truncation guard). Sealed path:
                                    // no epoch keys yet / quarantined — skip the frame.
                                    None => {
                                        if ring.is_none() {
                                            log::warn!("[pipeline] dropped oversize voice frame ({} B)", 1 + len);
                                        }
                                    }
                                }
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
                        if let Some(packet) = make_audio_packet(sequence, &flagged[..1 + len]) {
                            let _ = socket.send(&packet.to_bytes());
                        }
                        sequence = sequence.wrapping_add(1);
                    }
                }
            }
        }

        // 3. Send ping every 3s ────────────────────────────────────────────────
        let ping_due = match last_ping_time {
            None => true,
            Some(t) => {
                let every = if last_latency_ms.is_some() { ping_interval } else { PING_INTERVAL_UNANSWERED };
                t.elapsed() >= every
            }
        };
        if ping_due {
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let ping_pkt = UdpAudioPacket::new_ping(&sender_id, ts_ns);
            let _ = socket.send(&ping_pkt.to_bytes());
            last_ping_time = Some(Instant::now());
        }

        // 4. Drain audio packets from recv thread ─────────────────────────────
        // The dedicated recv thread reads the UDP socket and hands every
        // datagram over through the byte ring. We only see audio/ping packets
        // here — video packets go to the video thread, never touching this loop.
        let mut pkt_count_this_iter = 0u32;
        let mut peers_changed = false;
        if !audio_pkt_rx.write_is_held() && audio_pkt_rx.is_empty() {
            // The recv thread is gone (it reported why) and left nothing.
            break 'main;
        }
        {
            let rx = VoiceRx {
                ring: ring.as_ref(),
                self_key: &self_key,
                output_rate: output_sample_rate,
                event_tx: &event_tx,
            };
            while let Some(n) = pop_datagram(&mut audio_pkt_rx, &mut rx_dgram) {
                pkt_count_this_iter += 1;
                match on_voice_datagram(&mut rx_dgram[..n], &rx, &mut remote_peers, &Instant::now) {
                    Datagram::Ping(rtt_ms) => last_latency_ms = Some(rtt_ms),
                    Datagram::NewPeer => peers_changed = true,
                    Datagram::Other => {}
                }
            }
        }
        if peers_changed {
            refresh_peer_list(&peers, &remote_peers);
        }

        // 4b. Drain jitter buffers → decode → push to per-peer ring ─────────
        let drain_now = Instant::now();
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

        for peer in remote_peers.values_mut() {
            if drain_now.duration_since(peer.voice_drain_time) > max_behind {
                peer.voice_drain_time = drain_now - max_behind;
            }
            if drain_now.duration_since(peer.stream_drain_time) > max_behind {
                peer.stream_drain_time = drain_now - max_behind;
            }

            // ── Voice jitter buffer ──
            drain_peer_voice(peer, &mut VoiceDrain {
                now: drain_now,
                ring_paced: voice_ring_paced,
                low_water: voice_low_water,
                prime: voice_prime,
                deafened,
                aec_enabled: chain.aec_enabled(),
                user_volumes: &user_volumes,
                event_tx: &event_tx,
                dump: audio_dump.as_mut(),
            });

            // ── Stream audio jitter buffer ──
            drain_peer_stream(peer, &mut StreamDrain {
                now: drain_now,
                ring_paced: stream_ring_paced,
                low_water: stream_low_water,
                prime: stream_prime,
                stereo: stream_is_stereo,
                volume: stream_volume,
                aec_enabled: chain.aec_enabled(),
                prod: &stream_prod,
                out: &mut stream_out,
                render_fifo: &mut stream_render_fifo,
            });
        }

        // 4b'. Debug dump: device output tap + ring-dry detection ─────────
        if let Some(d) = audio_dump.as_mut() {
            if let Some(c) = tap_cons.as_mut() {
                let mut buf: Vec<i16> = Vec::with_capacity(c.occupied_len());
                while let Some(s) = c.try_pop() { buf.push(s); }
                if !buf.is_empty() { d.output_samples(&buf, output_sample_rate); }
            }
            for peer in remote_peers.values() {
                if voice_ring_paced && peer.ring_len() == 0
                    && (peer.voice_jitter.is_ready() || peer.voice_jitter.is_expanding())
                {
                    d.event(&format!("{} RING DRY while audio expected (jb={})", peer.name, peer.voice_jitter.len()));
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
        if chain.aec_enabled() {
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
        let mut to_remove: Vec<PeerKey> = Vec::new();
        for (key, peer) in &remote_peers {
            if peer.last_packet_time.elapsed() > stale_timeout {
                to_remove.push(*key);
            }
        }
        let had_removals = !to_remove.is_empty();
        for key in to_remove {
            if let Some(mut peer) = remote_peers.remove(&key) {
                // Emit speaking-stopped if they were still marked speaking
                if peer.speaking.is_speaking() {
                    peer.speaking.reset();
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged(peer.name, false));
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
            // Read on the producer's side: the consumer's lock is the output
            // callback's, and taking it here could make a callback miss it.
            let stream_fill = stream_prod.lock().map(|p| p.occupied_len()).unwrap_or(0);
            let recv_drop_total = recv_drops.load(std::sync::atomic::Ordering::Relaxed);
            let peer_stats: Vec<String> = remote_peers.values().map(|p| { let n = &p.name;
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

    // The microphone closes first (the OS's indicator goes out at once).
    drop(input_stream_opt);
    // Voice and stream audio fade out (5 ms, in the callbacks) instead of
    // being cut when the streams close.
    if let Some(out) = &output_stream {
        out.ctl.set_program(false);
    }
    if let Some(out) = &stream_output {
        out.ctl.fade_out();
    }
    // Signal the voice recv thread to exit and join it before letting
    // the audio thread return. Without the stop flag and the wake the recv
    // loop would sit in its wait until the next packet (silent channels
    // never get one), and `.join()` upstream would block the whole
    // shutdown; the wake ends its wait at once.
    drop(stop_recv); // sets the flag and wakes the read
    drop(audio_pkt_rx);
    let _ = voice_recv_handle.join();
    if let Some(out) = &output_stream {
        // Interface sounds playing in this output's mix (the "disconnect"
        // sound of this very leave) finish on it: later ones play elsewhere,
        // the ring moves. The callback wakes this thread as each ends.
        if let Some(bus) = &out.cues {
            cue::retire_live(bus);
            let deadline = Instant::now() + Duration::from_secs(3);
            while bus.busy() > 0 {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                std::thread::park_timeout(deadline - now);
            }
        }
        // Then the stream itself: faded (already silent, normally) and the
        // device handed its buffer's worth of silence before it closes.
        out.ctl.fade_out_and_wait(DEVICE_BUFFER_MS, Duration::from_millis(150));
    }
    drop(retiring);
    drop(stream_output);
    drop(output_stream);
}

// ── Voice receive, per datagram and per peer ────────────────────────────────

/// A sender's 32-byte id as it is on the wire (NUL-padded): the key peers
/// are held under, so a packet needs no String to find its peer.
pub(crate) type PeerKey = [u8; SENDER_ID_SIZE];

/// `id` as it appears on the wire (NUL-padded, cut at 32 bytes).
pub(crate) fn peer_key(id: &str) -> PeerKey {
    let mut k = [0u8; SENDER_ID_SIZE];
    let b = id.as_bytes();
    let n = b.len().min(SENDER_ID_SIZE);
    k[..n].copy_from_slice(&b[..n]);
    k
}

/// The id's text up to its first NUL, when it is UTF-8 (the relay stamps a
/// decimal uid). `None` can't name a member of the key ring or a watch, as
/// its lossy text (what a String key held before) couldn't.
fn key_str(key: &PeerKey) -> Option<&str> {
    let end = key.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
    std::str::from_utf8(&key[..end]).ok()
}

/// The id's text for a new peer (lossy, as `UdpAudioPacket::sender_id_str`).
fn key_name(key: &PeerKey) -> String {
    let end = key.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
    String::from_utf8_lossy(&key[..end]).into_owned()
}

/// Bytes of the recv thread → audio thread datagram ring: ~1,000 voice
/// packets, a second of five talking peers, far past any stall the audio
/// thread has (a device hot-swap). The channel it replaced held 1,024.
pub(crate) const VOICE_RX_RING_BYTES: usize = 256 * 1024;

/// Push one datagram as a record. `rec` is the datagram with two spare bytes
/// in front of it (`rec[2..]`), which take its length. False when the ring has
/// no room (the packet is dropped; the jitter buffer conceals it).
pub(crate) fn push_datagram(prod: &mut ringbuf::HeapProd<u8>, rec: &mut [u8]) -> bool {
    let len = rec.len() - 2;
    if len > u16::MAX as usize || prod.vacant_len() < rec.len() {
        return false;
    }
    rec[..2].copy_from_slice(&(len as u16).to_le_bytes());
    prod.push_slice(rec) == rec.len()
}

/// Pop the next whole record into `out`; its length, or None when none is
/// there.
pub(crate) fn pop_datagram(cons: &mut ringbuf::HeapCons<u8>, out: &mut [u8]) -> Option<usize> {
    let mut hdr = [0u8; 2];
    if cons.peek_slice(&mut hdr) < 2 {
        return None;
    }
    let len = u16::from_le_bytes(hdr) as usize;
    if cons.occupied_len() < 2 + len {
        return None; // never: a record is pushed in one piece
    }
    cons.skip(2);
    if len > out.len() {
        cons.skip(len);
        return Some(0);
    }
    cons.pop_slice(&mut out[..len]);
    Some(len)
}

/// What `on_voice_datagram` needs from the pipeline.
pub(crate) struct VoiceRx<'a> {
    /// The channel's MLS key ring; None on the plain (P2P) path.
    pub ring: Option<&'a crate::media::frame_crypto::SharedKeyRing>,
    /// Our own id: our packets reflected back are ignored.
    pub self_key: &'a PeerKey,
    pub output_rate: u32,
    pub event_tx: &'a std::sync::mpsc::Sender<VoiceEvent>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Datagram {
    Other,
    /// Our PING came back: the round trip in ms (PingMeasured sent).
    Ping(u32),
    /// A peer heard for the first time: the output's peer list must be rebuilt.
    NewPeer,
}

/// Section 4 of the loop for one datagram: a PING echo, or a peer's audio
/// or stream audio opened (in place, when sealed) into its jitter buffer.
/// Strict: in an encrypted channel only sealed audio that opens under the
/// sender's epoch key is heard; on the plain (P2P) path only plain. Allocates
/// nothing for a known peer. `clock` is read where the loop read
/// `Instant::now()` before (once the packet is opened, and again as it goes
/// into the jitter buffer: its arrival for the jitter estimate), so the
/// buffer sees each packet at the same point as before.
pub(crate) fn on_voice_datagram(
    dgram: &mut [u8],
    rx: &VoiceRx<'_>,
    remote_peers: &mut HashMap<PeerKey, PeerAudio>,
    clock: &dyn Fn() -> Instant,
) -> Datagram {
    use crate::media::frame_crypto::{self, Kind, PACKET_TYPE_AUDIO_SEALED, PACKET_TYPE_STREAM_AUDIO_SEALED};
    // Header as `UdpAudioPacket::from_bytes` reads it, payload left in place.
    if dgram.len() < AUDIO_HEADER_SIZE {
        return Datagram::Other;
    }
    let packet_type = dgram[0];
    let mut key: PeerKey = [0u8; SENDER_ID_SIZE];
    key.copy_from_slice(&dgram[1..1 + SENDER_ID_SIZE]);
    let sequence = u16::from_le_bytes([dgram[33], dgram[34]]);
    let payload_size = u16::from_le_bytes([dgram[35], dgram[36]]) as usize;
    if payload_size > MAX_PAYLOAD_SIZE || dgram.len() < AUDIO_HEADER_SIZE + payload_size {
        return Datagram::Other;
    }
    let payload = &mut dgram[AUDIO_HEADER_SIZE..AUDIO_HEADER_SIZE + payload_size];

    if packet_type == PACKET_TYPE_PING {
        if payload.len() >= 8 {
            let sent_ns = u64::from_le_bytes(payload[..8].try_into().unwrap_or([0; 8]));
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let rtt_ms = (now_ns.saturating_sub(sent_ns) / 1_000_000) as u32;
            let _ = rx.event_tx.send(VoiceEvent::PingMeasured(rtt_ms));
            return Datagram::Ping(rtt_ms);
        }
        return Datagram::Other;
    }
    if key == *rx.self_key {
        // Ignore our own reflected audio packets
        return Datagram::Other;
    }
    let is_voice = packet_type == PACKET_TYPE_AUDIO || packet_type == PACKET_TYPE_AUDIO_SEALED;
    let is_stream = packet_type == PACKET_TYPE_STREAM_AUDIO || packet_type == PACKET_TYPE_STREAM_AUDIO_SEALED;
    if !is_voice && !is_stream {
        return Datagram::Other;
    }
    let kind = if is_voice { Kind::Voice } else { Kind::StreamAudio };
    let sealed_type = if is_voice { PACKET_TYPE_AUDIO_SEALED } else { PACKET_TYPE_STREAM_AUDIO_SEALED };
    let opened: Option<std::ops::Range<usize>> = match (rx.ring, packet_type == sealed_type) {
        (Some(r), true) => key_str(&key)
            .and_then(|name| frame_crypto::open_audio_in_place(&r.load(), kind, name, sequence, payload)),
        (None, false) => Some(0..payload.len()),
        _ => None,
    };
    let Some(opened) = opened else { return Datagram::Other };
    let opened = &payload[opened];

    if is_stream {
        // Only for streams we're watching. The community relay fans
        // STREAM_AUDIO out to watchers only, so stop-watching silenced it
        // at the source; a P2P peer sends it unconditionally, so gate here —
        // otherwise the share keeps playing after you stop watching (and
        // again when they restart it).
        if !key_str(&key).is_some_and(super::is_watched) {
            return Datagram::Other;
        }
    }

    let now = clock();
    let inserted = !remote_peers.contains_key(&key);
    if inserted {
        log::info!(
            "[pipeline] New remote peer '{}' detected (type={}, seq={}, payload={}B)",
            key_name(&key), packet_type, sequence, payload_size
        );
    }
    let peer = remote_peers
        .entry(key)
        .or_insert_with(|| PeerAudio::new(key_name(&key), rx.output_rate, now));
    peer.last_packet_time = now;

    if is_voice {
        let (flags, opus_data) = if opened.len() > 1 {
            (opened[0], &opened[1..])
        } else {
            (0u8, opened)
        };
        // Emit mute/deafen only on an actual change — these flags ride every
        // audio packet (~50/s per talking peer) but almost never change, so
        // forwarding each to the JS event bridge floods the IPC channel for
        // nothing.
        let peer_muted = flags & FLAG_MUTED != 0;
        let peer_deafened = flags & FLAG_DEAFENED != 0;
        if peer.last_reported_state != Some((peer_muted, peer_deafened)) {
            peer.last_reported_state = Some((peer_muted, peer_deafened));
            let _ = rx.event_tx.send(VoiceEvent::UserStateChanged(peer.name.clone(), peer_muted, peer_deafened));
        }
        let is_silence = flags & FLAG_SILENCE != 0;
        peer.voice_jitter.push_slice_at(sequence, opus_data, clock(), is_silence);
        peer.voice_underrun_logged = false;
    } else {
        if peer.stream_audio_decoder.is_none() {
            peer.stream_audio_decoder = super::codec::StereoOpusDecoder::new().ok();
            // Stream audio is continuous and rarely quiet, so the
            // quiet-frame-first trim may never find a frame to drop; cap the
            // backlog tightly (splice past +60ms over target) so it stays in
            // step with the immediately-painted video. Voice keeps the
            // default.
            peer.stream_jitter.set_hard_excess(3);
        }
        // Straight into the jitter buffer on every platform. The Linux-only
        // 1s A/V-sync hold queue that used to sit here compensated for the
        // Tauri-era MSE video buffer; with WebCodecs video painting
        // immediately it had become a pure audio-behind-video lag (see
        // peer.rs).
        peer.stream_jitter.push_slice_at(sequence, opened, clock(), false);
    }
    if inserted { Datagram::NewPeer } else { Datagram::Other }
}

/// What `drain_peer_voice` needs from the pipeline (section 4b).
pub(crate) struct VoiceDrain<'a> {
    pub now: Instant,
    /// Decode when the peer's ring is below `low_water` (the device pulls
    /// it); else on the 20 ms clock (no output, or deafened).
    pub ring_paced: bool,
    pub low_water: usize,
    pub prime: usize,
    pub deafened: bool,
    pub aec_enabled: bool,
    pub user_volumes: &'a HashMap<String, f32>,
    pub event_tx: &'a std::sync::mpsc::Sender<VoiceEvent>,
    pub dump: Option<&'a mut AudioDump>,
}

/// Decode a peer's due voice frames into its ring (section 4b). Allocates
/// nothing in steady state: payload buffers go back to the jitter buffer.
pub(crate) fn drain_peer_voice(peer: &mut PeerAudio, cx: &mut VoiceDrain<'_>) {
    let drain_now = cx.now;
    let frame_dur = Duration::from_millis(20);
    let voice_ring_paced = cx.ring_paced;
    let voice_low_water = cx.low_water;
    peer.voice_jitter.set_prime_frames(if voice_ring_paced { cx.prime } else { 0 });
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
        if let Some(d) = cx.dump.as_deref_mut() {
            match &frame {
                Frame::Expand => d.event(&format!("{} EXPAND (plc) ring={} tgt={}", peer.name, peer.ring_len(), peer.voice_jitter.target())),
                Frame::Lost => d.event(&format!("{} LOST (fec/plc) jb={}", peer.name, peer.voice_jitter.len())),
                Frame::Idle if !peer.onset_pending => d.event(&format!("{} idle ring={} under={}", peer.name, peer.ring_len(), peer.voice_jitter.underruns)),
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
            peer.concealed_level = 1.0; // what comes next fades in from silence
            // Nothing to play: cold, the sender paused (gate tail /
            // keepalive), or an underrun's PLC expansion ran out.
            if !peer.voice_underrun_logged && peer.voice_jitter.underruns > 0 {
                log::debug!("[pipeline] Voice idle for peer '{}' (underruns={}, expand={}, tgt={})",
                    peer.name, peer.voice_jitter.underruns, peer.voice_jitter.expand_frames, peer.voice_jitter.target());
                peer.voice_underrun_logged = true;
            }
            // Feed the speaking detector a silent tick at frame
            // cadence so the ring clears instead of sticking lit —
            // a silence-gated sender no longer streams low-RMS
            // frames to clear it for us.
            if drain_now.duration_since(peer.last_silent_tick) >= frame_dur {
                peer.last_silent_tick = drain_now;
                if let Some(state) = peer.speaking.process_threshold(false) {
                    let _ = cx.event_tx.send(VoiceEvent::SpeakingChanged(peer.name.clone(), state));
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
        let was_packet = matches!(frame, Frame::Packet(_));
        let was_expand = frame == Frame::Expand;
        if let Frame::Packet(data) = frame {
            peer.voice_jitter.recycle(data);
        }
        if !decode_ok { continue; }
        // Last voice frame before the sender's gate closed (the next
        // buffered packet is a flagged tail frame): fade it out over
        // the frame. Senders from 0.7.5 fade their own tail, but a
        // 0.7.4 sender follows its noise floor with a hard zero frame
        // — audible as a tick after the person stops, especially on a
        // boosted peer — and this smooths it on the listening side.
        if was_packet
            && !peer.voice_jitter.last_was_silence()
            && peer.voice_jitter.next_is_silence() == Some(true)
        {
            let n = pcm.len() as f32;
            for (i, s) in pcm.iter_mut().enumerate() {
                *s = (*s as f32 * (1.0 - i as f32 / n)) as i16;
            }
            peer.talkspurt_faded = true;
            if let Some(d) = cx.dump.as_deref_mut() {
                d.event(&format!("{} last voice frame (fade-out)", peer.name));
            }
        } else if was_packet && peer.voice_jitter.last_was_silence() {
            // A flagged frame (the sender's fade tail, or a keepalive) after
            // a last voice frame we faded to silence ourselves: the talkspurt
            // has ended here, so it plays as silence. Played as sent, the
            // tail (the sender's signal fading from full level) jumped back
            // up out of our silence: a click when the gate closed mid-sound
            // (a push-to-talk release, a mute).
            if peer.talkspurt_faded {
                pcm = [0i16; FRAME_SIZE];
            }
        } else if was_packet {
            peer.talkspurt_faded = false;
        }
        // Fade PLC expansion to silence across the episode: Opus PLC
        // alone keeps a voiced tail pitch-repeating at useful level for
        // 100ms+, which — when a pre-0.7.4 sender simply stops — is
        // heard as a short buzz/tick after the person stops talking.
        // A late packet that resumes playback comes back at full
        // level; Opus smooths that join itself.
        // Each frame of the episode ramps from the level the one before it
        // ended at down to its own: one gain per frame would step at every
        // frame boundary (30 % of the wave's value at the first).
        if was_expand {
            let level = |k: f32| (1.0 - k / (super::jitter::MAX_EXPAND_FRAMES as f32 + 1.0)).powi(2);
            let k = peer.voice_jitter.expand_index() as f32;
            let (from, to) = (level(k - 1.0), level(k));
            let n = pcm.len() as f32;
            for (i, s) in pcm.iter_mut().enumerate() {
                let g = from + (to - from) * ((i + 1) as f32 / n);
                *s = (*s as f32 * g) as i16;
            }
            peer.concealed_level = to;
        } else if peer.concealed_level < 1.0 {
            // The late packet came: playback resumes from the level the
            // concealment had faded to, back up across this frame. At full
            // level straight after a faded frame it stepped by the
            // difference.
            let from = peer.concealed_level;
            peer.concealed_level = 1.0;
            let n = pcm.len() as f32;
            for (i, s) in pcm.iter_mut().enumerate() {
                let g = from + (1.0 - from) * ((i + 1) as f32 / n);
                *s = (*s as f32 * g) as i16;
            }
        }

        let rms = {
            let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
            (sum_sq / pcm.len() as f64).sqrt() as f32
        };
        let rms_db = if rms > 0.0 { 20.0 * (rms / 32768.0).log10() } else { -96.0 };
        if let Some(state) = peer.speaking.process_threshold(rms_db >= -50.0) {
            let _ = cx.event_tx.send(VoiceEvent::SpeakingChanged(peer.name.clone(), state));
        }
        // Latency trim: over target, drop quiet frames (decoded, so
        // the decoder state stays continuous) instead of playing them.
        if peer.voice_jitter.should_drop_excess(rms_db < -45.0) {
            if let Some(d) = cx.dump.as_deref_mut() {
                d.event(&format!("{} DROP excess={} rms={:.0}dB", peer.name, peer.voice_jitter.excess(), rms_db));
            }
            // Kept (the first of a run) for the crossfade into the next
            // played frame: see `crossfade_in`.
            if !peer.voice_cut_held {
                peer.voice_cut = pcm;
                peer.voice_cut_held = true;
            }
            if !voice_ring_paced { peer.voice_drain_time -= frame_dur; }
            continue;
        }
        if !cx.deafened {
            // Frames were cut before this one to catch up: it starts as a
            // crossfade out of the first of them (unless it starts playback
            // anyway and fades in below).
            if peer.voice_cut_held && !(peer.onset_pending && peer.ring_len() == 0) {
                crossfade_in(&peer.voice_cut, &mut pcm, 1);
            }
            peer.voice_cut_held = false;
            let gain = cx.user_volumes.get(peer.name.as_str()).copied().unwrap_or(1.0);
            let mut f32_frame = [0.0f32; FRAME_SIZE];
            // A changed volume ramps across this frame from the gain the
            // last one was played at: a step at the frame boundary is a click
            // per change ("zipper" noise while the slider is dragged).
            let from = peer.played_gain.unwrap_or(gain);
            peer.played_gain = Some(gain);
            if from == gain {
                for (i, &s) in pcm.iter().enumerate() {
                    f32_frame[i] = (s as f32 / 32768.0) * gain;
                }
            } else {
                let n = FRAME_SIZE as f32;
                for (i, &s) in pcm.iter().enumerate() {
                    let g = from + (gain - from) * ((i + 1) as f32 / n);
                    f32_frame[i] = (s as f32 / 32768.0) * g;
                }
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
                if let Some(d) = cx.dump.as_deref_mut() {
                    d.event(&format!("{} onset (fade-in) tgt={} jb={}", peer.name, peer.voice_jitter.target(), peer.voice_jitter.len()));
                }
            }
            peer.onset_pending = false;
            if let Some(d) = cx.dump.as_deref_mut() {
                d.peer_frame(&peer.name, &f32_frame);
            }
            // Queue for the AEC render reference (mono, pre-resample);
            // section 4c sums one frame per peer per 20ms.
            if cx.aec_enabled {
                if peer.render_fifo.len() >= RENDER_FIFO_MAX { peer.render_fifo.pop_front(); }
                peer.render_fifo.push_back(f32_frame);
            }
            peer.push_voice_frame(&f32_frame);
        } else {
            // Nothing is played while deafened: the first frame after it
            // starts playback again and fades in like a talkspurt's first
            // (the peer may be mid-word).
            peer.onset_pending = true;
            peer.voice_cut_held = false;
        }
    }
}

/// Start `pcm` (interleaved) as a 10 ms crossfade out of `cut`, the frame
/// that was dropped before it: `cut` continues the last played frame, so the
/// wave stays whole across the dropped audio.
pub(crate) fn crossfade_in(cut: &[i16], pcm: &mut [i16], channels: usize) {
    const XFADE: usize = 480; // 10 ms at 48 kHz
    for i in 0..XFADE {
        let w = (i + 1) as f32 / XFADE as f32;
        for c in 0..channels {
            let k = i * channels + c;
            pcm[k] = (cut[k] as f32 * (1.0 - w) + pcm[k] as f32 * w) as i16;
        }
    }
}

/// The stream-audio playback resampler (48 kHz stereo → the device that
/// plays stream audio) with its buffers.
pub(crate) struct StreamOut {
    /// None when that device runs at 48 kHz.
    resampler: Option<rubato::SincFixedOut<f64>>,
    accum_l: Vec<f64>,
    accum_r: Vec<f64>,
    // Reusable resampler input chunks (see peer.rs resamp_scratch).
    scratch_l: Vec<f64>,
    scratch_r: Vec<f64>,
    /// The stream ring was emptied (a device switch): the next frame played
    /// starts playback again and fades in.
    restart: bool,
}

impl StreamOut {
    pub fn new(rate: u32) -> Self {
        let mut s = StreamOut { resampler: None, accum_l: Vec::new(), accum_r: Vec::new(), scratch_l: Vec::new(), scratch_r: Vec::new(), restart: false };
        if rate != SAMPLE_RATE {
            log::info!("[pipeline] Playback stream resampler: {}Hz → {}Hz (stereo)", SAMPLE_RATE, rate);
        }
        s.set_rate(rate);
        s
    }

    /// The device's rate (a new device): a fresh resampler.
    pub fn set_rate(&mut self, rate: u32) {
        self.resampler = if rate == SAMPLE_RATE { None } else { Some(make_sinc_resampler(SAMPLE_RATE, rate, 480, 2)) };
    }

    /// The stream ring was emptied with what is buffered here: playback
    /// starts over.
    pub fn clear_accum(&mut self) {
        self.accum_l.clear();
        self.accum_r.clear();
        self.restart = true;
    }
}

/// What `drain_peer_stream` needs from the pipeline (section 4b).
pub(crate) struct StreamDrain<'a> {
    pub now: Instant,
    /// Decode when the stream ring is below `low_water`; else on the 20 ms clock.
    pub ring_paced: bool,
    pub low_water: usize,
    pub prime: usize,
    pub stereo: bool,
    /// 0.0–1.0, the viewer's stream volume.
    pub volume: f32,
    pub aec_enabled: bool,
    pub prod: &'a std::sync::Mutex<ringbuf::HeapProd<i16>>,
    pub out: &'a mut StreamOut,
    pub render_fifo: &'a mut std::collections::VecDeque<[f32; FRAME_SIZE]>,
}

/// Decode a peer's due stream-audio frames into the stream ring (section 4b).
pub(crate) fn drain_peer_stream(peer: &mut PeerAudio, cx: &mut StreamDrain<'_>) {
    let drain_now = cx.now;
    let frame_dur = Duration::from_millis(20);
    let stream_ring_paced = cx.ring_paced;
    let stream_is_stereo = cx.stereo;
    let stream_volume = cx.volume;
    let stream_prod = cx.prod;
    if let Some(ref mut decoder) = peer.stream_audio_decoder {
        peer.stream_jitter.set_prime_frames(if stream_ring_paced { cx.prime } else { 0 });
        let mut produced = 0usize;
        loop {
            let due = if stream_ring_paced {
                let len = stream_prod.lock().map(|p| p.occupied_len()).unwrap_or(usize::MAX);
                len < cx.low_water
            } else {
                drain_now.duration_since(peer.stream_drain_time) >= frame_dur
            };
            if !due || produced >= MAX_FRAMES_PER_TICK { break; }
            if !stream_ring_paced { peer.stream_drain_time += frame_dur; }

            let frame = peer.stream_jitter.drain();
            if frame == Frame::Idle {
                // Nothing to play: what comes next starts from silence.
                peer.stream_onset_pending = true;
                peer.stream_concealed_level = 1.0;
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
            let was_expand = frame == Frame::Expand;
            if let Frame::Packet(data) = frame {
                peer.stream_jitter.recycle(data);
            }
            if !decode_ok { continue; }
            // The stream's packets stopped (a stall, or the stream ended
            // without a word): the concealment fades to silence across the
            // episode, as voice's does, instead of repeating at level until
            // it is cut off.
            if was_expand {
                let level = |k: f32| (1.0 - k / (super::jitter::MAX_EXPAND_FRAMES as f32 + 1.0)).powi(2);
                let k = peer.stream_jitter.expand_index() as f32;
                let (from, to) = (level(k - 1.0), level(k));
                let n = STEREO_FRAME_SIZE as f32;
                for (i, lr) in pcm.chunks_exact_mut(2).enumerate() {
                    let g = from + (to - from) * ((i + 1) as f32 / n);
                    lr[0] = (lr[0] as f32 * g) as i16;
                    lr[1] = (lr[1] as f32 * g) as i16;
                }
                peer.stream_concealed_level = to;
            } else if peer.stream_concealed_level < 1.0 {
                // Resumed: back up from the concealment's level (see voice).
                let from = peer.stream_concealed_level;
                peer.stream_concealed_level = 1.0;
                let n = STEREO_FRAME_SIZE as f32;
                for (i, lr) in pcm.chunks_exact_mut(2).enumerate() {
                    let g = from + (1.0 - from) * ((i + 1) as f32 / n);
                    lr[0] = (lr[0] as f32 * g) as i16;
                    lr[1] = (lr[1] as f32 * g) as i16;
                }
            }
            let quiet = {
                let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
                let rms = (sum_sq / pcm.len() as f64).sqrt() as f32;
                rms < 32768.0 * 0.0056 // ≈ -45 dBFS
            };
            if peer.stream_jitter.should_drop_excess(quiet) {
                // Cut to catch up. Kept (the first of a run: it is what the
                // last played frame runs into) so the next played frame can
                // start as a crossfade out of it; butting that frame against
                // the last played one is a step in the wave.
                if !peer.stream_cut_held {
                    peer.stream_cut = pcm;
                    peer.stream_cut_held = true;
                }
                if !stream_ring_paced { peer.stream_drain_time -= frame_dur; }
                continue;
            }
            let ring_empty = stream_prod.lock().map(|p| p.occupied_len() == 0).unwrap_or(false);
            if (peer.stream_onset_pending && ring_empty) || cx.out.restart {
                // Playback starts (the stream's first audio, after a pause,
                // on a new device): the whole first frame fades in, as a
                // talkspurt's does. A stream's audio starts wherever its
                // wave happens to be.
                let n = STEREO_FRAME_SIZE as f32;
                for (i, lr) in pcm.chunks_exact_mut(2).enumerate() {
                    let g = i as f32 / n;
                    lr[0] = (lr[0] as f32 * g) as i16;
                    lr[1] = (lr[1] as f32 * g) as i16;
                }
            } else if peer.stream_cut_held {
                crossfade_in(&peer.stream_cut, &mut pcm, 2);
            }
            peer.stream_cut_held = false;
            peer.stream_onset_pending = false;
            cx.out.restart = false;
            if cx.out.resampler.is_none() {
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
            } else if let Some(ref mut resampler) = cx.out.resampler {
                // Resample stereo 48kHz → output device rate, then push
                for i in 0..STEREO_FRAME_SIZE {
                    let l = pcm[i * 2] as f32 * stream_volume / 32768.0;
                    let r = pcm[i * 2 + 1] as f32 * stream_volume / 32768.0;
                    cx.out.accum_l.push(l as f64);
                    cx.out.accum_r.push(r as f64);
                }
                let mut needed = resampler.input_frames_next();
                while cx.out.accum_l.len() >= needed && cx.out.accum_r.len() >= needed {
                    cx.out.scratch_l.clear();
                    cx.out.scratch_l.extend(cx.out.accum_l.drain(..needed));
                    cx.out.scratch_r.clear();
                    cx.out.scratch_r.extend(cx.out.accum_r.drain(..needed));
                    if let Ok(out) = resampler.process(&[&cx.out.scratch_l, &cx.out.scratch_r], None) {
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
            if cx.aec_enabled {
                let mut mono = [0.0f32; FRAME_SIZE];
                for i in 0..STEREO_FRAME_SIZE {
                    let l = pcm[i * 2] as f32;
                    let r = pcm[i * 2 + 1] as f32;
                    mono[i] = ((l + r) / 2.0) * stream_volume / 32768.0;
                }
                if cx.render_fifo.len() >= RENDER_FIFO_MAX { cx.render_fifo.pop_front(); }
                cx.render_fifo.push_back(mono);
            }
        }
    }
}

// ── Dedicated voice UDP receive thread ──────────────────────────────────────
//
// Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice UDP socket
// and forwards them to the audio processing thread. Video packets arrive on
// a separate media socket handled by the video recv thread in mod.rs.
//
// Every receive error is transient (review A2, udp_recv.rs): an oversized
// datagram on Windows or an ICMP unreachable after a relay restart used to
// end this loop and, with it, incoming voice, while TCP and the UI stayed
// connected. It ends on the stop flag, or when the audio thread has dropped
// its receiver (that thread's own shutdown or crash, which it reports).

/// Stops the receive loop when dropped, including while a panic unwinds:
/// sets its flag and wakes it (its read on Unix, its wait on Windows).
struct StopOnDrop(Arc<AtomicBool>, Arc<Waker>, Arc<MediaSocket>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
        self.1.wake();
        #[cfg(unix)]
        self.2.wake_reader();
    }
}

fn voice_recv_thread(
    socket: Arc<MediaSocket>,
    mut audio_tx: ringbuf::HeapProd<u8>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
    drops: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
    waker: Arc<Waker>,
) {
    // How the loop sleeps until a datagram. Unix: in the read itself, one
    // syscall per datagram like the 1 ms read-timeout loop it replaced, so
    // each packet is handed over at least as promptly (a wait + read pair
    // measured ~10 µs later at the median); the stop shuts the read side
    // down (`MediaSocket::wake_reader`). Linux needs no read timeout; other
    // Unixes keep READ_GUARD as a safety net for the wake. Windows: in
    // `udp_recv::wait` (WSAPoll with the waker; a blocked read there can't be
    // woken without closing the socket), READ_GUARD guarding the read after it.
    #[cfg(target_os = "linux")]
    let read_timeout = None;
    #[cfg(not(target_os = "linux"))]
    let read_timeout = Some(READ_GUARD);
    #[cfg(unix)]
    let _ = &waker;
    if let Err(e) = socket.set_read_timeout(read_timeout) {
        log::error!("[voice-recv] set_read_timeout: {}", e);
        let _ = event_tx.send(VoiceEvent::Error(format!(
            "Voice receive couldn't start ({}) — reconnect to hear others",
            e
        )));
        return;
    }

    // Two bytes in front of the datagram take its length, so a record goes
    // into the ring in one piece (`push_datagram`).
    let mut buf = [0u8; 2 + RECV_BUF_SIZE];
    let mut recv_errors = RecvErrors::new("voice-recv");
    let mut recv_count: u64 = 0;
    let mut recv_log_time = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Windows: sleep until a datagram (or the stop's wake); nothing else
        // is timed here. Unix sleeps in the read below.
        #[cfg(not(unix))]
        match udp_recv::wait(&socket, &waker, None) {
            Ok(Ready::Socket) => {}
            Ok(Ready::Woken | Ready::Timeout) => continue,
            Err(e) => {
                recv_errors.handle(&e);
                continue;
            }
        }
        // The relay's bind challenges (review S11) are answered inside
        // MediaSocket, which hands this loop an empty read for them; a
        // sealed socket's skipped datagrams too (it returns to this loop
        // rather than reading past what is queued: `recv_ready`).
        match socket.recv_ready(&mut buf[2..]) {
            // Cut short to fit the buffer: bigger than anything we send.
            Ok(inner) if is_oversized(inner.end) => recv_errors.ok(),
            Ok(inner) if !inner.is_empty() => {
                recv_errors.ok();
                recv_count += 1;
                if recv_log_time.elapsed() >= Duration::from_secs(5) {
                    log::debug!("[voice-recv] 5s stats: packets={}", recv_count);
                    recv_count = 0;
                    recv_log_time = Instant::now();
                }

                // Forward all packets on the voice socket to the audio thread.
                // No type classification needed — only voice packets arrive here.
                if !audio_tx.read_is_held() {
                    break; // the audio thread is gone
                }
                // `inner` is relative to buf[2..]: the record is the two
                // bytes before the datagram, then the datagram.
                let rec = inner.start..inner.end + 2;
                if !push_datagram(&mut audio_tx, &mut buf[rec]) {
                    // Audio thread is behind — drop this packet.
                    // The jitter buffer's PLC will smooth the gap.
                    drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Ok(_) => recv_errors.ok(),
            Err(e) => recv_errors.handle(&e),
        }
    }

    log::info!("[voice-recv] Exiting");
}

#[cfg(test)]
mod recv_tests {
    use super::*;
    use std::net::UdpSocket;
    use std::sync::mpsc::channel;

    /// The recv thread → audio thread ring, as the pipeline makes it.
    fn ring() -> (ringbuf::HeapProd<u8>, ringbuf::HeapCons<u8>) {
        HeapRb::<u8>::new(VOICE_RX_RING_BYTES).split()
    }

    /// The next datagram the recv thread handed over, waiting up to 2 s.
    fn next_datagram(rx: &mut ringbuf::HeapCons<u8>) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buf = [0u8; RECV_BUF_SIZE];
        loop {
            if let Some(n) = pop_datagram(rx, &mut buf) {
                return buf[..n].to_vec();
            }
            assert!(Instant::now() < deadline, "no datagram handed over");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A relay restart, as the voice socket sees it: our datagrams to the
    /// closed port come back as ICMP unreachable, so the connected socket
    /// reads ECONNREFUSED (on Linux). The loop used to end there; it must
    /// keep running and deliver the relay's next datagram.
    #[cfg(target_os = "linux")]
    #[test]
    fn voice_recv_survives_connection_refused() {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(relay_addr).unwrap();
        let client_addr = client.local_addr().unwrap();
        drop(relay); // the relay goes away

        let socket = Arc::new(MediaSocket::plain(client, "7"));
        let (audio_tx, mut audio_rx) = ring();
        let (event_tx, event_rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(Waker::new().unwrap());
        let handle = {
            let (socket, stop, waker) = (socket.clone(), stop.clone(), waker.clone());
            std::thread::spawn(move || {
                voice_recv_thread(socket, audio_tx, event_tx, Arc::new(Default::default()), stop, waker)
            })
        };

        // Our keepalives to the dead port: each ICMP reply is one refused
        // receive (or a refused send, whichever syscall comes first).
        for _ in 0..20 {
            let _ = socket.send(b"keepalive");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!handle.is_finished(), "voice receive ended on ECONNREFUSED");

        // The relay is back on the same port and sends audio.
        let relay = UdpSocket::bind(relay_addr).unwrap();
        relay.send_to(b"\x00audio from the relay", client_addr).unwrap();
        let got = next_datagram(&mut audio_rx);
        assert_eq!(got, b"\x00audio from the relay");

        // An oversized datagram (cut to the buffer) is dropped; the loop
        // goes on to the next one.
        relay.send_to(&[0u8; RECV_BUF_SIZE + 100], client_addr).unwrap();
        relay.send_to(b"\x00next", client_addr).unwrap();
        let got = next_datagram(&mut audio_rx);
        assert_eq!(got, b"\x00next");

        drop(StopOnDrop(stop, waker, socket.clone()));
        handle.join().unwrap();
        assert!(event_rx.try_recv().is_err(), "no error event for transient errors");
    }

    #[test]
    fn voice_recv_stops_on_flag_and_when_the_audio_thread_is_gone() {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(peer.local_addr().unwrap()).unwrap();
        let socket = Arc::new(MediaSocket::plain(client, "7"));

        // The stop flag and its wake, on a silent socket: the loop sleeps in
        // its read (no read timeout polls the socket) and ends at once.
        let (audio_tx, _audio_rx) = ring();
        let (event_tx, _event_rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(Waker::new().unwrap());
        let handle = {
            let (socket, stop, waker) = (socket.clone(), stop.clone(), waker.clone());
            std::thread::spawn(move || {
                voice_recv_thread(socket, audio_tx, event_tx, Arc::new(Default::default()), stop, waker)
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        let guard = StopOnDrop(stop, waker, socket.clone());
        let t0 = Instant::now();
        drop(guard); // what an unwinding audio pipeline does
        handle.join().unwrap();
        assert!(t0.elapsed() < Duration::from_millis(250), "stop took {:?}", t0.elapsed());

        // The audio thread's receiver dropped: the next datagram ends it.
        // (A fresh socket: the stop above shut that one's read side.)
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(peer.local_addr().unwrap()).unwrap();
        let client_addr = client.local_addr().unwrap();
        let socket = Arc::new(MediaSocket::plain(client, "7"));
        let (audio_tx, audio_rx) = ring();
        let (event_tx, _event_rx) = channel();
        drop(audio_rx);
        let handle = {
            let socket = socket.clone();
            std::thread::spawn(move || {
                voice_recv_thread(
                    socket,
                    audio_tx,
                    event_tx,
                    Arc::new(Default::default()),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Waker::new().unwrap()),
                )
            })
        };
        peer.send_to(b"\x00audio", client_addr).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(handle.is_finished());
    }

    /// A sealed (P2P) socket: a datagram the loop doesn't surface (a peer's
    /// PING, reflected inside MediaSocket) hands the loop back to its stop
    /// check before it sleeps again, and a stop ends it at once (the read
    /// side shut down, plus a loopback byte for an unconnected socket). The
    /// next real datagram still arrives.
    #[test]
    fn sealed_loop_stops_at_once_after_a_reflected_ping() {
        use crate::media::media_socket::SocketKeys;
        use crate::media::packet::UdpAudioPacket;
        let ka = SocketKeys { tx_key: [1; 32], rx_key: [2; 32], tx_salt: [1; 4], rx_salt: [2; 4] };
        let kb = SocketKeys { tx_key: [2; 32], rx_key: [1; 32], tx_salt: [2; 4], rx_salt: [1; 4] };
        let a = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), ka, "1");
        let b = Arc::new(MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), kb, "2"));
        a.set_peer(b.local_addr().unwrap());
        b.set_peer(a.local_addr().unwrap());

        let (audio_tx, mut audio_rx) = ring();
        let (event_tx, _event_rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(Waker::new().unwrap());
        let handle = {
            let (b, stop, waker) = (b.clone(), stop.clone(), waker.clone());
            std::thread::spawn(move || {
                voice_recv_thread(b, audio_tx, event_tx, Arc::new(Default::default()), stop, waker)
            })
        };
        a.send(&UdpAudioPacket::new_ping("1", 5).to_bytes()).unwrap();
        let audio = UdpAudioPacket::new_audio("1", 1, b"after the ping").unwrap().to_bytes();
        std::thread::sleep(Duration::from_millis(30));
        a.send(&audio).unwrap();
        assert_eq!(next_datagram(&mut audio_rx), audio);
        // The ping came back to a, reflected.
        a.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut buf = [0u8; RECV_BUF_SIZE];
        assert!(a.recv(&mut buf).unwrap() > 0);

        a.send(&UdpAudioPacket::new_ping("1", 6).to_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let t0 = Instant::now();
        drop(StopOnDrop(stop, waker, b.clone()));
        handle.join().unwrap();
        assert!(t0.elapsed() < Duration::from_millis(200), "stop took {:?}", t0.elapsed());
    }

    /// The loop this one replaced: `recv` with a 1 ms read timeout, the
    /// datagram copied into a Vec for the audio thread.
    fn old_voice_recv_loop(socket: Arc<MediaSocket>, audio_tx: std::sync::mpsc::SyncSender<Vec<u8>>, stop: Arc<AtomicBool>) {
        socket.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        let mut buf = [0u8; RECV_BUF_SIZE];
        while !stop.load(Ordering::Relaxed) {
            if let Ok(n) = socket.recv(&mut buf) {
                if n >= 1 {
                    let _ = audio_tx.try_send(buf[..n].to_vec());
                }
            }
        }
    }

    /// Hand-off latency, datagram sent → visible to the audio thread, of this
    /// loop (blocking read, records into the byte ring) against the loop it
    /// replaced (1 ms read timeout, a Vec per datagram into a channel): no
    /// worse, so the jitter buffer sees every packet at least as early as
    /// before. Both loops run at once on their own socket pairs and take
    /// turns (so CPU frequency and load hit both alike): 2 × 300 datagrams
    /// 0.2–4 ms apart (inside and across the old loop's 1 ms slices); the
    /// consumer spins on each queue (`try_recv` / the ring) as the audio
    /// thread's tick would find it, and stamps each arrival.
    #[test]
    fn handoff_is_as_prompt_as_the_1ms_poll() {
        let pair = || {
            let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
            let client = UdpSocket::bind("127.0.0.1:0").unwrap();
            client.connect(peer.local_addr().unwrap()).unwrap();
            peer.connect(client.local_addr().unwrap()).unwrap();
            (peer, Arc::new(MediaSocket::plain(client, "7")))
        };
        let (old_peer, old_sock) = pair();
        let (new_peer, new_sock) = pair();
        let (ring_tx, mut ring_rx) = ring();
        let (vec_tx, vec_rx) = std::sync::mpsc::sync_channel(64);
        let (event_tx, _event_rx) = channel();
        let old_stop = Arc::new(AtomicBool::new(false));
        let new_stop = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(Waker::new().unwrap());
        let old_handle = {
            let (s, stop) = (old_sock.clone(), old_stop.clone());
            std::thread::spawn(move || old_voice_recv_loop(s, vec_tx, stop))
        };
        let new_handle = {
            let (s, stop, waker) = (new_sock.clone(), new_stop.clone(), waker.clone());
            std::thread::spawn(move || voice_recv_thread(s, ring_tx, event_tx, Arc::new(Default::default()), stop, waker))
        };
        let mut lcg = 0x2545_f491_4f6c_dd1du64;
        let (mut old, mut new) = (Vec::new(), Vec::new());
        let mut buf = [0u8; RECV_BUF_SIZE];
        for i in 0..600 {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            std::thread::sleep(Duration::from_micros(200 + (lcg >> 33) % 3800));
            let use_new = i % 2 == 1;
            let sent = Instant::now();
            if use_new { new_peer.send(b"\x00voice").unwrap(); } else { old_peer.send(b"\x00voice").unwrap(); }
            loop {
                let got = if use_new { pop_datagram(&mut ring_rx, &mut buf).is_some() } else { vec_rx.try_recv().is_ok() };
                if got {
                    break;
                }
                assert!(sent.elapsed() < Duration::from_secs(2), "never handed over");
                std::hint::spin_loop();
            }
            if use_new { new.push(sent.elapsed()) } else { old.push(sent.elapsed()) }
        }
        old_stop.store(true, Ordering::Relaxed);
        old_handle.join().unwrap();
        drop(StopOnDrop(new_stop, waker, new_sock));
        new_handle.join().unwrap();
        old.sort();
        new.sort();
        let pct = |v: &[Duration], p: usize| v[(v.len() * p / 100).min(v.len() - 1)];
        println!(
            "hand-off µs p10 / p50 / p90 / p99: 1 ms poll {} / {} / {} / {}; blocking read {} / {} / {} / {}",
            pct(&old, 10).as_micros(), pct(&old, 50).as_micros(), pct(&old, 90).as_micros(), pct(&old, 99).as_micros(),
            pct(&new, 10).as_micros(), pct(&new, 50).as_micros(), pct(&new, 90).as_micros(), pct(&new, 99).as_micros(),
        );
        // Both are one kernel wakeup and one read; allow scheduler noise from
        // tests running alongside, never a systematic delay.
        assert!(pct(&new, 50) <= pct(&old, 50) + pct(&old, 50) / 2 + Duration::from_micros(20));
        assert!(pct(&new, 90) <= pct(&old, 90) * 2 + Duration::from_micros(100));
    }
}

#[cfg(test)]
mod rx_tests {
    //! The voice receive path (sections 4 and 4b) driven without devices or
    //! sockets: datagrams through the recv thread's byte ring into
    //! `on_voice_datagram`, then `drain_peer_voice` into each peer's ring, a
    //! simulated device pulling 20 ms per peer per step.
    use super::*;
    use crate::media::frame_crypto::{AudioSealer, EpochKeys, KeyRing, SharedKeyRing};
    use crate::media::packet::UdpAudioPacket;
    use crate::media::peer::PeerCons;
    use crate::test_alloc::allocations_on_this_thread;
    use ringbuf::traits::Consumer;

    const PEERS: [&str; 3] = ["101", "102", "103"];
    const ME: &str = "9";
    const STEPS: usize = 400;
    const WARM_UP: usize = 120;

    fn rings() -> (Vec<KeyRing>, SharedKeyRing) {
        let members: Vec<String> = PEERS.iter().chain([&ME]).map(|s| s.to_string()).collect();
        let keys = EpochKeys::derive(1, &[7u8; 32], &members);
        let senders = PEERS.iter().map(|p| KeyRing::empty(p).rotated(keys.clone())).collect();
        (senders, Arc::new(ArcSwap::from_pointee(KeyRing::empty(ME).rotated(keys))))
    }

    /// (step, datagram) for each peer's 20 ms frames: a tone per peer, sealed
    /// or plain. `rough`: every 29th packet lost, pairs swapped every 17, and
    /// steps 200–206 held back and delivered at once on step 207.
    fn datagrams(sealed: bool, rough: bool) -> Vec<(usize, Vec<u8>)> {
        let (senders, _) = rings();
        let mut out = Vec::new();
        for (pi, peer) in PEERS.iter().enumerate() {
            let mut enc = super::super::codec::OpusEncoder::new(64_000).unwrap();
            let sealer = AudioSealer::new(Kind::Voice);
            let mut opus = [0u8; MAX_OPUS_FRAME_SIZE];
            for step in 0..STEPS {
                let mut frame = [0i16; FRAME_SIZE];
                for (i, s) in frame.iter_mut().enumerate() {
                    let t = (step * FRAME_SIZE + i) as f64 / 48_000.0;
                    *s = ((2.0 * std::f64::consts::PI * (180.0 + 70.0 * pi as f64) * t).sin() * 7_000.0) as i16;
                }
                let n = enc.encode(&frame, &mut opus).unwrap();
                let mut flagged = vec![0u8];
                flagged.extend_from_slice(&opus[..n]);
                let seq = step as u16;
                let pkt = if sealed {
                    let s = sealer.seal(&senders[pi], seq, &flagged).unwrap();
                    UdpAudioPacket::new_typed(crate::media::frame_crypto::PACKET_TYPE_AUDIO_SEALED, peer, seq, &s)
                } else {
                    UdpAudioPacket::new_audio(peer, seq, &flagged)
                };
                let mut at = step;
                if rough {
                    if step % 29 == 28 {
                        continue;
                    }
                    if (200..207).contains(&step) {
                        at = 207;
                    }
                }
                out.push((at, pkt.unwrap().to_bytes()));
            }
        }
        if rough {
            for i in (0..out.len() - 1).step_by(17) {
                if out[i].0 == out[i + 1].0 {
                    out.swap(i, i + 1);
                }
            }
        }
        out.sort_by_key(|(at, _)| *at);
        out
    }

    use crate::media::frame_crypto::Kind;

    struct Out {
        /// Each peer's ring as the device pulled it.
        pcm: Vec<Vec<i16>>,
        steady_allocations: u64,
    }

    /// Run the receive path. `old_path`: decode the datagrams as the loop did
    /// before (`UdpAudioPacket::from_bytes`, an allocating open, a Vec per
    /// packet into the jitter buffer); else the new path.
    fn run(sealed: bool, rough: bool, output_rate: u32, old_path: bool) -> Out {
        let dgrams = datagrams(sealed, rough);
        let (_, ring) = rings();
        let ring = sealed.then_some(ring);
        let (event_tx, _event_rx) = std::sync::mpsc::channel();
        let self_key = peer_key(ME);
        let user_volumes = HashMap::new();
        let mut peers: HashMap<PeerKey, PeerAudio> = HashMap::with_capacity(8);
        let (mut prod, mut cons) = HeapRb::<u8>::new(VOICE_RX_RING_BYTES).split();
        let mut rec = [0u8; 2 + RECV_BUF_SIZE];
        let mut dgram = [0u8; RECV_BUF_SIZE];
        let pull = (output_rate / 50) as usize;
        let low_water = pull + (output_rate / 100) as usize;
        let frame_out = FRAME_SIZE * output_rate as usize / 48_000;
        let prime = low_water.div_ceil(frame_out) + 1;
        let mut handles: Vec<(PeerKey, PeerCons)> = Vec::new();
        let mut pcm: Vec<Vec<i16>> = PEERS.iter().map(|_| Vec::with_capacity((STEPS + 20) * pull)).collect();
        let mut pulled = [0i16; 2048];
        let t0 = Instant::now();
        let mut next = 0;
        let mut warm = 0;
        // The window is the talking: from WARM_UP to the last packet's step
        // (the 20 steps after it are the talkspurt's end: speaking-stopped
        // events, which allocate in the channel, once per talkspurt).
        let mut talking_end = 0;
        for step in 0..STEPS + 20 {
            if step == WARM_UP {
                warm = allocations_on_this_thread();
            }
            if step == STEPS {
                talking_end = allocations_on_this_thread();
            }
            let now = t0 + Duration::from_millis(step as u64 * 20);
            // The recv thread's side: each datagram into the ring as a record.
            while next < dgrams.len() && dgrams[next].0 <= step {
                let d = &dgrams[next].1;
                rec[2..2 + d.len()].copy_from_slice(d);
                assert!(push_datagram(&mut prod, &mut rec[..2 + d.len()]));
                next += 1;
            }
            // The audio thread's: section 4, then 4b per peer.
            let rx = VoiceRx { ring: ring.as_ref(), self_key: &self_key, output_rate, event_tx: &event_tx };
            while let Some(n) = pop_datagram(&mut cons, &mut dgram) {
                if old_path {
                    let pkt = UdpAudioPacket::from_bytes(&dgram[..n]).unwrap();
                    let name = pkt.sender_id_str();
                    let opened = match &ring {
                        Some(r) => crate::media::frame_crypto::open_audio(&r.load(), Kind::Voice, &name, pkt.sequence, pkt.payload_data()).unwrap(),
                        None => pkt.payload_data().to_vec(),
                    };
                    let peer = peers.entry(peer_key(&name)).or_insert_with(|| PeerAudio::new(name.clone(), output_rate, now));
                    peer.last_packet_time = now;
                    peer.voice_jitter.push_at(pkt.sequence, opened[1..].to_vec(), now, false);
                } else {
                    on_voice_datagram_at(&mut dgram[..n], &rx, &mut peers, now);
                }
            }
            if handles.len() < peers.len() {
                handles = peers.iter().map(|(k, p)| (*k, p.output_handle().cons)).collect();
                handles.sort_by_key(|(k, _)| *k);
            }
            for peer in peers.values_mut() {
                drain_peer_voice(peer, &mut VoiceDrain {
                    now,
                    ring_paced: true,
                    low_water,
                    prime,
                    deafened: false,
                    aec_enabled: false,
                    user_volumes: &user_volumes,
                    event_tx: &event_tx,
                    dump: None,
                });
            }
            // The device: one 20 ms pull per peer.
            for (i, (_, c)) in handles.iter().enumerate() {
                let mut c = c.lock().unwrap();
                let got = c.pop_slice(&mut pulled[..pull]);
                pcm[i].extend_from_slice(&pulled[..got]);
                pcm[i].extend(std::iter::repeat(0).take(pull - got));
            }
        }
        let steady_allocations = talking_end - warm;
        Out { pcm, steady_allocations }
    }

    /// A talking peer costs no heap allocation per packet once warm: three
    /// peers, plain and sealed, at the device's rate and through the
    /// playback resampler.
    #[test]
    fn three_talking_peers_allocate_nothing_in_steady_state() {
        for sealed in [false, true] {
            for rate in [48_000, 44_100] {
                let out = run(sealed, false, rate, false);
                assert_eq!(out.steady_allocations, 0, "sealed={sealed} rate={rate}");
                for p in &out.pcm {
                    // Played throughout: digital silence only before the
                    // first frame and after the last, a real level between.
                    let zeros = p.iter().filter(|s| **s == 0).count();
                    let peak = p.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                    assert!(zeros < p.len() / 10 && peak > 3000, "sealed={sealed} rate={rate}: {zeros} zeros, peak {peak}");
                }
            }
        }
    }

    /// The same datagrams, with loss, reordering and a late burst, give the
    /// same PCM through the new path (in-place open, borrowed payloads,
    /// recycled buffers) as through the old one (copy, allocating open, a Vec
    /// per packet), sample for sample.
    #[test]
    fn new_receive_path_gives_the_old_pcm() {
        for sealed in [false, true] {
            for rate in [48_000, 44_100] {
                let old = run(sealed, true, rate, true);
                let new = run(sealed, true, rate, false);
                assert_eq!(old.pcm.len(), new.pcm.len());
                for (o, n) in old.pcm.iter().zip(&new.pcm) {
                    assert!(o.iter().any(|s| s.abs() > 1000));
                    assert!(o == n, "sealed={sealed} rate={rate}: PCM differs");
                }
            }
        }
    }

    /// Strict, and our own packets ignored: plaintext audio is dropped in an
    /// encrypted channel, sealed audio on the plain path, a reflected packet of
    /// ours, a truncated or over-long header; stream audio only for a watched
    /// streamer.
    #[test]
    fn what_reaches_a_jitter_buffer() {
        let (senders, ring) = rings();
        let (event_tx, _rx) = std::sync::mpsc::channel();
        let self_key = peer_key(ME);
        let now = Instant::now();
        let plain = UdpAudioPacket::new_audio("101", 1, b"\x00abc").unwrap().to_bytes();
        let s = AudioSealer::new(Kind::Voice).seal(&senders[0], 1, b"\x00abc").unwrap();
        let sealed = UdpAudioPacket::new_typed(crate::media::frame_crypto::PACKET_TYPE_AUDIO_SEALED, "101", 1, &s).unwrap().to_bytes();
        let ours = UdpAudioPacket::new_audio(ME, 1, b"\x00abc").unwrap().to_bytes();
        for (with_ring, dgram, heard) in [
            (true, &plain, false),
            (true, &sealed, true),
            (false, &sealed, false),
            (false, &plain, true),
            (false, &ours, false),
        ] {
            let mut peers = HashMap::new();
            let rx = VoiceRx { ring: with_ring.then_some(&ring), self_key: &self_key, output_rate: 48_000, event_tx: &event_tx };
            let got = on_voice_datagram_at(&mut dgram.clone(), &rx, &mut peers, now);
            assert_eq!(got == Datagram::NewPeer, heard, "ring={with_ring}");
            assert_eq!(peers.len(), heard as usize);
            if heard {
                assert_eq!(peers[&peer_key("101")].voice_jitter.len(), 1);
                assert_eq!(peers[&peer_key("101")].name, "101");
            }
        }
        let mut peers = HashMap::new();
        let rx = VoiceRx { ring: None, self_key: &self_key, output_rate: 48_000, event_tx: &event_tx };
        assert_eq!(on_voice_datagram_at(&mut plain[..plain.len() - 1].to_vec(), &rx, &mut peers, now), Datagram::Other);
        assert_eq!(on_voice_datagram_at(&mut plain[..20].to_vec(), &rx, &mut peers, now), Datagram::Other);
        let stream = UdpAudioPacket::new_stream_audio("4711", 1, b"opus").unwrap().to_bytes();
        assert_eq!(on_voice_datagram_at(&mut stream.clone(), &rx, &mut peers, now), Datagram::Other);
        assert!(peers.is_empty(), "unwatched stream audio is dropped");
        crate::media::watch_stream_add("4711");
        assert_eq!(on_voice_datagram_at(&mut stream.clone(), &rx, &mut peers, now), Datagram::NewPeer);
        assert_eq!(peers[&peer_key("4711")].stream_jitter.len(), 1);
        crate::media::watch_stream_remove("4711");
        // A PING echo: the round trip, and the event.
        let ping = UdpAudioPacket::new_ping(ME, 0).to_bytes();
        assert!(matches!(on_voice_datagram_at(&mut ping.clone(), &rx, &mut peers, now), Datagram::Ping(_)));
    }

    fn on_voice_datagram_at(d: &mut [u8], rx: &VoiceRx<'_>, peers: &mut HashMap<PeerKey, PeerAudio>, now: Instant) -> Datagram {
        on_voice_datagram(d, rx, peers, &move || now)
    }
}
