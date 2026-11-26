use std::sync::Arc;
use std::time::Duration;

use rubato::{SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction, Resampler};

use super::capture::AudioFrame;
use super::codec::{StereoOpusEncoder, MAX_OPUS_FRAME_SIZE, SAMPLE_RATE, STEREO_FRAME_SAMPLES};
use super::media_socket::MediaSocket;
use super::packet::UdpAudioPacket;

pub enum AudioStreamControl {
    Shutdown,
    /// Re-point the STREAM_AUDIO sends at a new voice socket and key ring —
    /// used when the stream follows the user into a new voice channel, whose
    /// MLS group seals with keys of its own.
    Retarget(Arc<MediaSocket>, Option<super::frame_crypto::SharedKeyRing>),
}

pub enum AudioStreamEvent {
    Started,
    Stopped,
    Error(String),
}

/// Run the stream audio send pipeline on a dedicated thread.
/// Reads AudioFrames from the platform capture, resamples to 48kHz,
/// Opus-encodes stereo, packetizes, and sends via UDP.
pub fn run_audio_stream_pipeline(
    frame_rx: std::sync::mpsc::Receiver<AudioFrame>,
    control_rx: std::sync::mpsc::Receiver<AudioStreamControl>,
    event_tx: std::sync::mpsc::Sender<AudioStreamEvent>,
    mut socket: Arc<MediaSocket>,
    sender_id: String,
    bitrate_kbps: u32,
    mut ring: Option<super::frame_crypto::SharedKeyRing>,
) {
    use super::frame_crypto::{AudioSealer, Kind, PACKET_TYPE_STREAM_AUDIO_SEALED};
    let sealer = AudioSealer::new(Kind::StreamAudio);
    let encoder = match StereoOpusEncoder::new((bitrate_kbps * 1000) as i32) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(AudioStreamEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(AudioStreamEvent::Started);
    log::info!(
        "[stream-audio] Pipeline started, stereo Opus @ {}kbps",
        bitrate_kbps
    );

    let mut sequence: u16 = 0;
    let mut frame_count: u64 = 0;
    // Accumulation buffer: platform capture may deliver arbitrary chunk sizes,
    // but Opus needs exactly STEREO_FRAME_SAMPLES (1920) interleaved i16 samples.
    let mut pcm_buf: Vec<i16> = Vec::with_capacity(STEREO_FRAME_SAMPLES * 4);

    // Lazy-initialized sinc resampler (stereo, 2 channels) — created on first
    // frame when we learn the device sample rate.
    let mut resampler: Option<SincFixedOut<f64>> = None;
    // Per-channel accumulation buffers for the sinc resampler (needs fixed chunks)
    let mut accum_l: Vec<f64> = Vec::new();
    let mut accum_r: Vec<f64> = Vec::new();
    let mut passthrough = false;

    'pipeline: loop {
        // Receive audio frame from platform capture
        let frame = match frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(f) => Some(f),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        // Control messages after the wait and before encoding: a retarget
        // already sent applies to the frame in hand, so no packet goes out
        // after a channel move sealed for, or sent to, the channel we left.
        loop {
            match control_rx.try_recv() {
                Ok(AudioStreamControl::Shutdown) => break 'pipeline,
                Ok(AudioStreamControl::Retarget(s, r)) => {
                    log::info!("[stream-audio] send route re-pointed (channel move)");
                    socket = s;
                    ring = r;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'pipeline,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        let Some(frame) = frame else { continue };

        // Initialize resampler on first frame (now we know the device sample rate)
        if resampler.is_none() && !passthrough {
            let from_rate = frame.sample_rate;
            if from_rate == SAMPLE_RATE {
                passthrough = true;
                log::info!("[stream-audio] Device rate {}Hz matches Opus, passthrough", from_rate);
            } else {
                log::info!(
                    "[stream-audio] Device rate {}Hz != {}Hz, enabling sinc resampler",
                    from_rate, SAMPLE_RATE
                );
                let params = SincInterpolationParameters {
                    sinc_len: 64,
                    f_cutoff: 0.95,
                    interpolation: SincInterpolationType::Cubic,
                    oversampling_factor: 128,
                    window: WindowFunction::Blackman2,
                };
                match SincFixedOut::<f64>::new(
                    SAMPLE_RATE as f64 / from_rate as f64,
                    1.1,
                    params,
                    480, // output chunk size (10ms at 48kHz)
                    2,   // stereo
                ) {
                    Ok(r) => resampler = Some(r),
                    Err(e) => {
                        let msg = format!("Failed to create stereo resampler ({}Hz → {}Hz): {}", from_rate, SAMPLE_RATE, e);
                        log::info!("[stream-audio] {}", msg);
                        let _ = event_tx.send(AudioStreamEvent::Error(msg));
                        break;
                    }
                }
            }
        }

        if passthrough {
            // No resampling needed — convert f32 to i16 directly
            for &sample in &frame.data {
                pcm_buf.push((sample * 32767.0).clamp(-32768.0, 32767.0) as i16);
            }
        } else if let Some(ref mut res) = resampler {
            // De-interleave stereo into per-channel accumulation buffers
            for chunk in frame.data.chunks_exact(2) {
                accum_l.push(chunk[0] as f64);
                accum_r.push(chunk[1] as f64);
            }

            // Process in resampler-sized chunks
            let mut needed = res.input_frames_next();
            while accum_l.len() >= needed && accum_r.len() >= needed {
                let chunk_l: Vec<f64> = accum_l.drain(..needed).collect();
                let chunk_r: Vec<f64> = accum_r.drain(..needed).collect();
                if let Ok(out) = res.process(&[&chunk_l, &chunk_r], None) {
                    // Re-interleave and convert to i16
                    let len = out[0].len().min(out[1].len());
                    for i in 0..len {
                        pcm_buf.push((out[0][i] * 32767.0).clamp(-32768.0, 32767.0) as i16);
                        pcm_buf.push((out[1][i] * 32767.0).clamp(-32768.0, 32767.0) as i16);
                    }
                }
                needed = res.input_frames_next();
            }
        }

        // Encode complete frames (1920 interleaved i16 samples = 960 per channel)
        while pcm_buf.len() >= STEREO_FRAME_SAMPLES {
            let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
            match encoder.encode(&pcm_buf[..STEREO_FRAME_SAMPLES], &mut opus_out) {
                Ok(len) => {
                    // Community channel: seal under the MLS epoch key (skipped
                    // while no epoch is ready); P2P: plain inside the sealed socket.
                    let packet = match &ring {
                        Some(r) => sealer.seal(&r.load(), sequence, &opus_out[..len]).and_then(|sealed| {
                            UdpAudioPacket::new_typed(PACKET_TYPE_STREAM_AUDIO_SEALED, &sender_id, sequence, &sealed)
                        }),
                        None => UdpAudioPacket::new_stream_audio(&sender_id, sequence, &opus_out[..len]),
                    };
                    match packet {
                        Some(packet) => {
                            let _ = socket.send(&packet.to_bytes());
                        }
                        None => {
                            if ring.is_none() {
                                log::warn!("[stream-audio] dropped oversize frame ({} B)", len);
                            }
                        }
                    }
                    sequence = sequence.wrapping_add(1);
                    frame_count += 1;

                    if frame_count == 1 || frame_count % 500 == 0 {
                        log::debug!(
                            "[stream-audio] Encoded frame {}, {} Opus bytes, seq={}",
                            frame_count, len, sequence
                        );
                    }
                }
                Err(e) => {
                    log::warn!("[stream-audio] Encode error: {}", e);
                    let _ = event_tx.send(AudioStreamEvent::Error(format!("Encode: {}", e)));
                }
            }
            pcm_buf.drain(..STEREO_FRAME_SAMPLES);
        }
    }

    log::info!("[stream-audio] Pipeline stopped after {} frames", frame_count);
    let _ = event_tx.send(AudioStreamEvent::Stopped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::frame_crypto::{open_audio, EpochKeys, KeyRing, Kind, SharedKeyRing, PACKET_TYPE_STREAM_AUDIO_SEALED};
    use std::net::UdpSocket;
    use std::sync::mpsc;

    fn ring(ex: u8) -> SharedKeyRing {
        let keys = EpochKeys::derive(1, &[ex; 32], &["7".to_string()]);
        Arc::new(arc_swap::ArcSwap::from_pointee(KeyRing::empty("7").rotated(keys)))
    }

    fn socket_to(rx: &UdpSocket) -> Arc<MediaSocket> {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(rx.local_addr().unwrap()).unwrap();
        Arc::new(MediaSocket::plain(sock, "7"))
    }

    /// Exactly one Opus frame of 48 kHz stereo (no resampler involved).
    fn frame() -> AudioFrame {
        AudioFrame { data: vec![0.1; STEREO_FRAME_SAMPLES], channels: 2, sample_rate: SAMPLE_RATE }
    }

    fn recv(rx: &UdpSocket) -> Option<UdpAudioPacket> {
        let mut buf = [0u8; 2048];
        let n = rx.recv(&mut buf).ok()?;
        UdpAudioPacket::from_bytes(&buf[..n])
    }

    /// Review B7: stream audio carried into another voice channel is sealed
    /// with the new channel's group and sent on its socket.
    #[test]
    fn retarget_moves_stream_audio_to_the_new_socket_and_keys() {
        let old_rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let new_rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        for rx in [&old_rx, &new_rx] {
            rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        }
        let (old_ring, new_ring) = (ring(1), ring(2));
        let (frame_tx, frame_rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, _event_rx) = mpsc::channel();
        let pipeline = {
            let (socket, ring) = (socket_to(&old_rx), old_ring.clone());
            std::thread::spawn(move || {
                run_audio_stream_pipeline(frame_rx, control_rx, event_tx, socket, "7".into(), 64, Some(ring))
            })
        };

        frame_tx.send(frame()).unwrap();
        let p = recv(&old_rx).expect("first packet on the old socket");
        assert_eq!(p.packet_type, PACKET_TYPE_STREAM_AUDIO_SEALED);
        assert!(open_audio(&old_ring.load(), Kind::StreamAudio, "7", p.sequence, p.payload_data()).is_some());

        // The move: the retarget goes out before the next captured frame.
        control_tx
            .send(AudioStreamControl::Retarget(socket_to(&new_rx), Some(new_ring.clone())))
            .unwrap();
        frame_tx.send(frame()).unwrap();
        let p = recv(&new_rx).expect("packet after the move on the new socket");
        assert!(open_audio(&new_ring.load(), Kind::StreamAudio, "7", p.sequence, p.payload_data()).is_some());
        assert!(
            open_audio(&old_ring.load(), Kind::StreamAudio, "7", p.sequence, p.payload_data()).is_none(),
            "the old channel's keys must not open audio sent to the new one"
        );
        old_rx.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        assert!(recv(&old_rx).is_none(), "nothing more goes to the old socket");

        control_tx.send(AudioStreamControl::Shutdown).unwrap();
        pipeline.join().unwrap();
    }
}
