use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::{HashMap, VecDeque};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::codec::{
    OpusDecoder, OpusEncoder, CHANNELS, FRAME_SIZE, MAX_OPUS_FRAME_SIZE, SAMPLE_RATE,
};
use super::packet::{
    UdpAudioPacket, PACKET_TOTAL_SIZE, PACKET_TYPE_AUDIO, PACKET_TYPE_PING,
};
use super::speaking::SpeakingDetector;

// ── Control / Event messages ──────────────────────────────────────────────────

pub enum ControlMessage {
    SetMute(bool),
    SetDeafen(bool),
    Shutdown,
}

pub enum VoiceEvent {
    SpeakingChanged(String, bool),
    PingMeasured(u32),
    Error(String),
}

// ── Per-remote-peer state ─────────────────────────────────────────────────────

struct RemotePeer {
    decoder: OpusDecoder,
    speaking: SpeakingDetector,
    last_seq: u16,
    last_packet_time: Instant,
}

// ── Main blocking pipeline entry-point ───────────────────────────────────────

/// Runs the audio pipeline on the calling thread (should be a dedicated OS thread).
pub fn run_audio_pipeline(
    server_addr: String,
    sender_id: String,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // ── UDP socket ────────────────────────────────────────────────────────────
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("UDP bind failed: {}", e)));
            return;
        }
    };
    if let Err(e) = socket.connect(&server_addr) {
        let _ = event_tx.send(VoiceEvent::Error(format!("UDP connect failed: {}", e)));
        return;
    }
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

    let stream_config = cpal::StreamConfig {
        channels: CHANNELS,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    // ── Shared buffers ────────────────────────────────────────────────────────
    let capture_buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let playback_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));

    const BUF_CAP: usize = FRAME_SIZE * 8;

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
            let cfg = stream_config.clone();
            match input_device.build_input_stream(
                &cfg,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut buf) = cap_buf_in.lock() {
                        let remaining = BUF_CAP.saturating_sub(buf.len());
                        let take = data.len().min(remaining);
                        buf.extend_from_slice(&data[..take]);
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

    let output_stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            if let Ok(mut buf) = play_buf_out.lock() {
                for sample in data.iter_mut() {
                    *sample = buf.pop_front().unwrap_or(0);
                }
            } else {
                for sample in data.iter_mut() {
                    *sample = 0;
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

    let mut sequence: u16 = 0;
    let mut local_speaking = SpeakingDetector::new();
    let mut remote_peers: HashMap<String, RemotePeer> = HashMap::new();

    let mut last_ping_time = Instant::now();
    let ping_interval = Duration::from_secs(3);

    let mut recv_buf = [0u8; PACKET_TOTAL_SIZE];

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
                // Speaking detection on local audio
                if let Some(state) = local_speaking.process(&frame) {
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), state));
                }

                let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                let encode_result = if muted {
                    encoder.encode_silence(&mut opus_out)
                } else {
                    encoder.encode(&frame, &mut opus_out)
                };

                match encode_result {
                    Ok(len) => {
                        let packet =
                            UdpAudioPacket::new_audio(&sender_id, sequence, &opus_out[..len]);
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
                    let packet =
                        UdpAudioPacket::new_audio(&sender_id, sequence, &opus_out[..len]);
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
        // The socket has a 5ms read timeout; this is non-blocking from the loop's perspective.
        match socket.recv(&mut recv_buf) {
            Ok(n) if n == PACKET_TOTAL_SIZE => {
                if let Some(pkt) = UdpAudioPacket::from_bytes(&recv_buf[..n]) {
                    let username = pkt.sender_username();

                    // Ignore our own reflected packets
                    if username == sender_id {
                        // fall through — ignore
                    } else if pkt.packet_type == PACKET_TYPE_PING {
                        // Measure RTT: payload[0..8] is the original send timestamp_ns
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
                    } else if pkt.packet_type == PACKET_TYPE_AUDIO {
                        // Sequence duplicate/out-of-order check
                        let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                            // First packet from this user
                            RemotePeer {
                                decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                    // We can't recover gracefully from decoder init failure here
                                    // without making the control flow much more complex.
                                    // Log and create a default anyway — decode will return errors.
                                    OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                }),
                                speaking: SpeakingDetector::new(),
                                last_seq: pkt.sequence.wrapping_sub(1),
                                last_packet_time: Instant::now(),
                            }
                        });

                        let diff = pkt.sequence.wrapping_sub(peer.last_seq);
                        // diff == 0 → duplicate; diff > 32768 → out-of-order (wrapped)
                        if diff == 0 || diff > 32768 {
                            // skip stale/duplicate packet
                        } else {
                            peer.last_seq = pkt.sequence;
                            peer.last_packet_time = Instant::now();

                            let opus_data = pkt.payload_data();
                            let mut pcm = [0i16; FRAME_SIZE];
                            match peer.decoder.decode(opus_data, &mut pcm) {
                                Ok(_) => {
                                    // Speaking detection on remote audio
                                    if let Some(state) = peer.speaking.process(&pcm) {
                                        let _ = event_tx.send(VoiceEvent::SpeakingChanged(
                                            username.clone(),
                                            state,
                                        ));
                                    }

                                    // Mix into playback buffer (only if not deafened)
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
                    }
                }
            }
            Ok(_) => {
                // Short read — not a valid packet, ignore
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Expected: 5ms timeout elapsed with no data
            }
            Err(e) => {
                let _ = event_tx.send(VoiceEvent::Error(format!("UDP recv error: {}", e)));
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
    drop(output_stream);
    drop(input_stream_opt);
}
