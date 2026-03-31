use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::video_packet::UdpVideoPacket;

/// A reassembled video frame ready for decoding.
#[derive(Debug, Clone)]
pub struct ReassembledFrame {
    pub frame_id: u32,
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub streamer_username: String,
}

/// Tracks in-progress frame assembly.
struct FrameAssembly {
    total_packets: u16,
    received: HashMap<u16, Vec<u8>>, // packet_index -> payload
    is_keyframe: bool,
    created_at: Instant,
}

/// Jitter buffer and frame reassembly for incoming video packets.
pub struct VideoReceiver {
    frames_in_progress: HashMap<u32, FrameAssembly>, // frame_id -> assembly
    last_complete_frame_id: Option<u32>,
    nack_timeout: Duration,
    max_nack_retries: u32,
    nack_tracking: HashMap<(u32, u16), (Instant, u32)>, // (frame_id, pkt_idx) -> (last_nack_time, retry_count)
    buffer_depth: Duration,
}

impl VideoReceiver {
    pub fn new() -> Self {
        VideoReceiver {
            frames_in_progress: HashMap::new(),
            last_complete_frame_id: None,
            nack_timeout: Duration::from_millis(50),
            max_nack_retries: 3,
            nack_tracking: HashMap::new(),
            buffer_depth: Duration::from_millis(50),
        }
    }

    /// Process an incoming video packet. Returns a complete frame if one is ready.
    pub fn process_packet(&mut self, pkt: &UdpVideoPacket) -> Option<ReassembledFrame> {
        // Copy fields from packed struct to avoid unaligned access UB
        let frame_id = { pkt.frame_id };
        let packet_index = { pkt.packet_index };
        let total_packets = { pkt.total_packets };
        let is_keyframe = { pkt.is_keyframe };

        let frame = self.frames_in_progress.entry(frame_id).or_insert_with(|| {
            FrameAssembly {
                total_packets,
                received: HashMap::new(),
                is_keyframe,
                created_at: Instant::now(),
            }
        });

        frame.received.insert(packet_index, pkt.payload_data().to_vec());

        // Check if frame is complete
        if frame.received.len() == frame.total_packets as usize {
            let assembly = self.frames_in_progress.remove(&frame_id).unwrap();
            self.last_complete_frame_id = Some(frame_id);

            // Clear NACK tracking for this frame
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);

            // Reassemble in order
            let mut data = Vec::new();
            for i in 0..assembly.total_packets {
                if let Some(chunk) = assembly.received.get(&i) {
                    data.extend_from_slice(chunk);
                }
            }

            return Some(ReassembledFrame {
                frame_id,
                data,
                is_keyframe: assembly.is_keyframe,
                streamer_username: pkt.sender_username(),
            });
        }

        None
    }

    /// Check for missing packets and return NACK requests to send.
    /// Also returns true if PLI should be sent (too many failures).
    pub fn check_missing(&mut self) -> (Vec<(u32, Vec<u16>)>, bool) {
        let now = Instant::now();
        let mut nacks: Vec<(u32, Vec<u16>)> = Vec::new();
        let mut need_pli = false;

        let mut stale_frames = Vec::new();

        for (&frame_id, assembly) in &self.frames_in_progress {
            // Skip frames older than buffer depth
            if assembly.created_at.elapsed() > self.buffer_depth * 3 {
                stale_frames.push(frame_id);
                continue;
            }

            // Find missing packet indices
            if assembly.created_at.elapsed() > self.nack_timeout {
                let mut missing = Vec::new();
                for i in 0..assembly.total_packets {
                    if !assembly.received.contains_key(&i) {
                        let key = (frame_id, i);
                        // Initial entry uses a past timestamp so the first NACK fires immediately
                        let entry = self.nack_tracking.entry(key).or_insert((now - self.nack_timeout - Duration::from_millis(1), 0));
                        if entry.1 >= self.max_nack_retries {
                            need_pli = true;
                        } else if now.duration_since(entry.0) > self.nack_timeout {
                            missing.push(i);
                            entry.0 = now;
                            entry.1 += 1;
                        }
                    }
                }
                if !missing.is_empty() {
                    nacks.push((frame_id, missing));
                }
            }
        }

        // Clean up stale frames
        for frame_id in stale_frames {
            self.frames_in_progress.remove(&frame_id);
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);
        }

        (nacks, need_pli)
    }

    /// Clean up old frame assemblies.
    pub fn cleanup_stale(&mut self) {
        let cutoff = Duration::from_millis(500);
        let mut dropped = 0u32;
        self.frames_in_progress.retain(|_frame_id, assembly| {
            if assembly.created_at.elapsed() >= cutoff {
                if assembly.received.len() < assembly.total_packets as usize {
                    dropped += 1;
                }
                false // remove
            } else {
                true // keep
            }
        });
        if dropped > 0 {
            eprintln!("[video-recv] Dropped {} incomplete frames", dropped);
        }
        // Also prune stale NACK tracking entries
        self.nack_tracking.retain(|&(fid, _), _| self.frames_in_progress.contains_key(&fid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_packet(frame_id: u32, index: u16, total: u16, keyframe: bool, data: &[u8]) -> UdpVideoPacket {
        UdpVideoPacket::new("streamer1", frame_id, index, total, keyframe, data)
    }

    #[test]
    fn single_packet_frame_completes_immediately() {
        let mut receiver = VideoReceiver::new();
        let pkt = make_packet(0, 0, 1, true, b"keyframe data");
        let result = receiver.process_packet(&pkt);
        assert!(result.is_some());
        let frame = result.unwrap();
        assert_eq!(frame.frame_id, 0);
        assert_eq!(frame.data, b"keyframe data");
        assert!(frame.is_keyframe);
    }

    #[test]
    fn multi_packet_frame_completes_on_last() {
        let mut receiver = VideoReceiver::new();

        let pkt0 = make_packet(1, 0, 3, false, b"part0");
        let pkt1 = make_packet(1, 1, 3, false, b"part1");
        let pkt2 = make_packet(1, 2, 3, false, b"part2");

        assert!(receiver.process_packet(&pkt0).is_none());
        assert!(receiver.process_packet(&pkt1).is_none());
        let result = receiver.process_packet(&pkt2);
        assert!(result.is_some());

        let frame = result.unwrap();
        assert_eq!(frame.data, b"part0part1part2");
    }

    #[test]
    fn out_of_order_packets_still_complete() {
        let mut receiver = VideoReceiver::new();

        let pkt2 = make_packet(1, 2, 3, false, b"c");
        let pkt0 = make_packet(1, 0, 3, false, b"a");
        let pkt1 = make_packet(1, 1, 3, false, b"b");

        assert!(receiver.process_packet(&pkt2).is_none());
        assert!(receiver.process_packet(&pkt0).is_none());
        let result = receiver.process_packet(&pkt1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().data, b"abc"); // reassembled in order
    }

    #[test]
    fn missing_packet_detected() {
        let mut receiver = VideoReceiver::new();
        receiver.nack_timeout = Duration::from_millis(1);

        let pkt0 = make_packet(1, 0, 3, false, b"a");
        let pkt2 = make_packet(1, 2, 3, false, b"c");
        // pkt1 is missing

        receiver.process_packet(&pkt0);
        receiver.process_packet(&pkt2);

        // Wait for nack timeout to expire
        std::thread::sleep(Duration::from_millis(2));

        let (nacks, _need_pli) = receiver.check_missing();
        assert_eq!(nacks.len(), 1);
        assert_eq!(nacks[0].0, 1); // frame_id
        assert_eq!(nacks[0].1, vec![1]); // missing index 1
    }
}
