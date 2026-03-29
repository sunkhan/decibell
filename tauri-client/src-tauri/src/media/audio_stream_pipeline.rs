use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use super::capture::AudioFrame;
use super::codec::{StereoOpusEncoder, MAX_OPUS_FRAME_SIZE, STEREO_FRAME_SAMPLES};
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
/// Reads AudioFrames from the platform capture, Opus-encodes stereo,
/// packetizes, and sends via UDP.
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

        // Convert f32 interleaved stereo to i16 and accumulate
        for &sample in &frame.data {
            let clamped = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            pcm_buf.push(clamped);
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
