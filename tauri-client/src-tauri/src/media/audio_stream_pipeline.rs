use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use rubato::{SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction, Resampler};

use super::capture::AudioFrame;
use super::codec::{StereoOpusEncoder, MAX_OPUS_FRAME_SIZE, SAMPLE_RATE, STEREO_FRAME_SAMPLES};
use super::packet::UdpAudioPacket;

pub enum AudioStreamControl {
    Shutdown,
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
    socket: Arc<UdpSocket>,
    sender_id: String,
    bitrate_kbps: u32,
) {
    let encoder = match StereoOpusEncoder::new((bitrate_kbps * 1000) as i32) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(AudioStreamEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(AudioStreamEvent::Started);
    eprintln!(
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

    loop {
        // Check control messages
        match control_rx.try_recv() {
            Ok(AudioStreamControl::Shutdown) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        // Receive audio frame from platform capture
        let frame = match frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(f) => f,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        // Initialize resampler on first frame (now we know the device sample rate)
        if resampler.is_none() && !passthrough {
            let from_rate = frame.sample_rate;
            if from_rate == SAMPLE_RATE {
                passthrough = true;
                eprintln!("[stream-audio] Device rate {}Hz matches Opus, passthrough", from_rate);
            } else {
                eprintln!(
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
                resampler = Some(SincFixedOut::<f64>::new(
                    SAMPLE_RATE as f64 / from_rate as f64,
                    1.1,
                    params,
                    480, // output chunk size (10ms at 48kHz)
                    2,   // stereo
                ).expect("failed to create stereo sinc resampler"));
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
                    let packet =
                        UdpAudioPacket::new_stream_audio(&sender_id, sequence, &opus_out[..len]);
                    let _ = socket.send(&packet.to_bytes());
                    sequence = sequence.wrapping_add(1);
                    frame_count += 1;

                    if frame_count == 1 || frame_count % 500 == 0 {
                        eprintln!(
                            "[stream-audio] Encoded frame {}, {} Opus bytes, seq={}",
                            frame_count, len, sequence
                        );
                    }
                }
                Err(e) => {
                    eprintln!("[stream-audio] Encode error: {}", e);
                    let _ = event_tx.send(AudioStreamEvent::Error(format!("Encode: {}", e)));
                }
            }
            pcm_buf.drain(..STEREO_FRAME_SAMPLES);
        }
    }

    eprintln!("[stream-audio] Pipeline stopped after {} frames", frame_count);
    let _ = event_tx.send(AudioStreamEvent::Stopped);
}
