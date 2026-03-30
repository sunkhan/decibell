use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

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

/// Stateful linear-interpolation resampler for a single channel.
struct LinearResampler {
    ratio: f64,
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

    // Lazy-initialized resamplers (one per channel) — created on first frame
    // when we learn the device sample rate.
    let mut resampler_l: Option<LinearResampler> = None;
    let mut resampler_r: Option<LinearResampler> = None;

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

        // Initialize resamplers on first frame (now we know the device sample rate)
        if resampler_l.is_none() {
            let from_rate = frame.sample_rate;
            if from_rate != SAMPLE_RATE {
                eprintln!(
                    "[stream-audio] Device rate {}Hz != {}Hz, enabling resampler",
                    from_rate, SAMPLE_RATE
                );
            }
            resampler_l = Some(LinearResampler::new(from_rate, SAMPLE_RATE));
            resampler_r = Some(LinearResampler::new(from_rate, SAMPLE_RATE));
        }

        let res_l = resampler_l.as_mut().unwrap();
        let res_r = resampler_r.as_mut().unwrap();

        if res_l.passthrough {
            // No resampling needed — convert f32 to i16 directly
            for &sample in &frame.data {
                let clamped = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
                pcm_buf.push(clamped);
            }
        } else {
            // De-interleave stereo, resample each channel, re-interleave
            let num_frames = frame.data.len() / 2;
            let mut left_in = Vec::with_capacity(num_frames);
            let mut right_in = Vec::with_capacity(num_frames);
            for chunk in frame.data.chunks_exact(2) {
                left_in.push(chunk[0] as f64);
                right_in.push(chunk[1] as f64);
            }

            let mut left_out = Vec::with_capacity(
                (num_frames as f64 / res_l.ratio) as usize + 2
            );
            let mut right_out = Vec::with_capacity(
                (num_frames as f64 / res_r.ratio) as usize + 2
            );
            res_l.process(&left_in, &mut left_out);
            res_r.process(&right_in, &mut right_out);

            // Re-interleave and convert to i16
            let out_len = left_out.len().min(right_out.len());
            for i in 0..out_len {
                let l = (left_out[i] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                let r = (right_out[i] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                pcm_buf.push(l);
                pcm_buf.push(r);
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
