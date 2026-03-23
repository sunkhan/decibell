use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::capture::RawFrame;
use super::encoder::{EncoderConfig, H264Encoder};
use super::video_packet::{UdpVideoPacket, UDP_MAX_PAYLOAD};

pub enum VideoPipelineControl {
    ForceKeyframe,
    Shutdown,
}

pub enum VideoPipelineEvent {
    Started,
    Stopped,
    Error(String),
}

/// Run the video send pipeline on a dedicated thread.
/// Reads RawFrames from the channel, encodes them, packetizes, and sends via UDP.
///
/// IMPORTANT: `socket` must be the SAME UDP socket used by the voice audio pipeline.
/// The community server identifies senders by their UDP source address, which was
/// learned during voice connection. A different socket would have a different port
/// and the server would reject the packets.
pub fn run_video_send_pipeline(
    frame_rx: std::sync::mpsc::Receiver<RawFrame>,
    control_rx: std::sync::mpsc::Receiver<VideoPipelineControl>,
    event_tx: std::sync::mpsc::Sender<VideoPipelineEvent>,
    socket: Arc<UdpSocket>,
    sender_id: String,
    config: EncoderConfig,
    target_fps: u32,
) {
    // Initialize encoder
    let mut encoder = match H264Encoder::new(&config) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VideoPipelineEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(VideoPipelineEvent::Started);

    let mut frame_id: u32 = 0;
    let frame_interval = Duration::from_secs_f64(1.0 / target_fps as f64);
    let mut last_frame_time = Instant::now();

    loop {
        // Check control messages
        match control_rx.try_recv() {
            Ok(VideoPipelineControl::Shutdown) => break,
            Ok(VideoPipelineControl::ForceKeyframe) => {
                encoder.force_keyframe();
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        // Frame rate limiting: skip frames that arrive faster than target
        let now = Instant::now();
        if now.duration_since(last_frame_time) < frame_interval {
            match frame_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(_frame) => continue, // drop frame, too soon
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Receive frame
        let frame = match frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(f) => f,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        last_frame_time = Instant::now();

        // Encode
        match encoder.encode_frame(&frame.data, frame.width, frame.height) {
            Ok(Some(encoded)) => {
                // Packetize: split encoded data into UDP_MAX_PAYLOAD-sized chunks
                let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
                let total = chunks.len() as u16;

                for (i, chunk) in chunks.iter().enumerate() {
                    let pkt = UdpVideoPacket::new(
                        &sender_id,
                        frame_id,
                        i as u16,
                        total,
                        encoded.is_keyframe,
                        chunk,
                    );
                    let _ = socket.send(&pkt.to_bytes());
                }
                frame_id = frame_id.wrapping_add(1);
            }
            Ok(None) => {} // encoder buffering
            Err(e) => {
                let _ = event_tx.send(VideoPipelineEvent::Error(format!("Encode: {}", e)));
            }
        }
    }

    // Flush encoder
    for encoded in encoder.flush() {
        let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;
        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = UdpVideoPacket::new(&sender_id, frame_id, i as u16, total, encoded.is_keyframe, chunk);
            let _ = socket.send(&pkt.to_bytes());
        }
        frame_id = frame_id.wrapping_add(1);
    }

    let _ = event_tx.send(VideoPipelineEvent::Stopped);
}
