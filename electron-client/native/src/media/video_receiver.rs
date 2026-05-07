use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::video_packet::{UdpVideoPacket, UdpFecPacket, UDP_MAX_PAYLOAD};

/// A reassembled video frame ready for decoding.
#[derive(Debug, Clone)]
pub struct ReassembledFrame {
    pub frame_id: u32,
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub streamer_username: String,
    /// Codec byte from the per-packet UdpVideoPacket.codec field
    /// (taken from the first packet of the frame). Drives WebCodecs
    /// decoder configuration on the viewer side.
    pub codec: u8,
    /// Decoder configuration record (avcC / hvcC / av1C). Set on the
    /// receive thread for HEVC/AV1 keyframes — those carry the
    /// description in-band as a length-prefixed prefix on the wire and
    /// are stripped before this struct reaches consumers. None on
    /// non-keyframes and on H.264, where the receiver builds the
    /// description by parsing inline SPS/PPS NALs.
    pub description: Option<Vec<u8>>,
}

/// Stored FEC group data for recovery.
struct FecGroup {
    group_start: u16,
    group_count: u16,
    payload_size_xor: u16,
    payload: [u8; UDP_MAX_PAYLOAD],
}

/// Tracks in-progress frame assembly.
struct FrameAssembly {
    total_packets: u16,
    received: HashMap<u16, Vec<u8>>, // packet_index -> payload
    payload_sizes: HashMap<u16, u16>, // packet_index -> payload_size
    is_keyframe: bool,
    created_at: Instant,
    streamer_username: String,
    /// Codec byte from the first packet of the frame. Plan B: drives
    /// WebCodecs decoder configuration on the viewer side.
    codec: u8,
    fec_groups: Vec<FecGroup>,
    fec_recovered: bool,
}

impl FrameAssembly {
    /// Try FEC recovery across all stored FEC groups. Returns true if a packet
    /// was recovered and the frame is now complete.
    fn try_fec_recovery(&mut self) -> bool {
        if self.fec_recovered || self.fec_groups.is_empty() {
            return false;
        }
        if self.received.len() == self.total_packets as usize {
            return false; // already complete
        }

        for fec in &self.fec_groups {
            let group_end = (fec.group_start + fec.group_count).min(self.total_packets);

            // Count missing packets in this FEC group
            let mut missing_count = 0u16;
            let mut missing_idx = 0u16;
            for i in fec.group_start..group_end {
                if !self.received.contains_key(&i) {
                    missing_count += 1;
                    missing_idx = i;
                }
            }

            // Can only recover if exactly 1 packet is missing in the group
            if missing_count != 1 {
                continue;
            }

            // Reconstruct: XOR the FEC payload with all received packets in the group
            let mut recovered = fec.payload;
            let mut recovered_size = fec.payload_size_xor;

            for i in fec.group_start..group_end {
                if i == missing_idx {
                    continue;
                }
                if let Some(pkt_data) = self.received.get(&i) {
                    // XOR payload bytes (received packets are variable-length,
                    // but FEC was computed with zero-padding to UDP_MAX_PAYLOAD)
                    for (j, &b) in pkt_data.iter().enumerate() {
                        recovered[j] ^= b;
                    }
                    if let Some(&pkt_size) = self.payload_sizes.get(&i) {
                        recovered_size ^= pkt_size;
                    }
                }
            }

            // Validate recovered size
            if recovered_size as usize <= UDP_MAX_PAYLOAD {
                let data = recovered[..recovered_size as usize].to_vec();
                self.payload_sizes.insert(missing_idx, recovered_size);
                self.received.insert(missing_idx, data);
                self.fec_recovered = true;
                eprintln!("[video-recv] FEC recovered packet {} (frame has {}/{} now)",
                    missing_idx, self.received.len(), self.total_packets);
                return self.received.len() == self.total_packets as usize;
            }
        }

        false
    }

    /// Reassemble the complete frame data in packet order.
    fn reassemble(&self) -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0..self.total_packets {
            if let Some(chunk) = self.received.get(&i) {
                data.extend_from_slice(chunk);
            }
        }
        data
    }
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
        let payload_size = { pkt.payload_size };
        let username = pkt.sender_username();

        let pkt_codec = { pkt.codec };
        let frame = self.frames_in_progress.entry(frame_id).or_insert_with(|| {
            FrameAssembly {
                total_packets,
                received: HashMap::new(),
                payload_sizes: HashMap::new(),
                is_keyframe,
                created_at: Instant::now(),
                streamer_username: username.clone(),
                codec: pkt_codec,
                fec_groups: Vec::new(),
                fec_recovered: false,
            }
        });

        frame.received.insert(packet_index, pkt.payload_data().to_vec());
        frame.payload_sizes.insert(packet_index, payload_size);

        // Check if frame is complete (directly or after FEC recovery)
        let complete = frame.received.len() == frame.total_packets as usize
            || frame.try_fec_recovery();

        if complete {
            let assembly = self.frames_in_progress.remove(&frame_id).unwrap();
            self.last_complete_frame_id = Some(frame_id);
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);

            return Some(ReassembledFrame {
                frame_id,
                data: assembly.reassemble(),
                is_keyframe: assembly.is_keyframe,
                streamer_username: assembly.streamer_username,
                codec: assembly.codec,
                description: None,
            });
        }

        None
    }

    /// Process an incoming FEC packet. Returns a complete frame if recovery succeeds.
    pub fn process_fec_packet(&mut self, pkt: &UdpFecPacket) -> Option<ReassembledFrame> {
        let frame_id = { pkt.frame_id };
        let group_start = { pkt.group_start };
        let group_count = { pkt.group_count };
        let payload_size_xor = { pkt.payload_size_xor };

        let frame = self.frames_in_progress.get_mut(&frame_id)?;

        frame.fec_groups.push(FecGroup {
            group_start,
            group_count,
            payload_size_xor,
            payload: pkt.payload,
        });

        if frame.try_fec_recovery() {
            let assembly = self.frames_in_progress.remove(&frame_id).unwrap();
            self.last_complete_frame_id = Some(frame_id);
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);

            return Some(ReassembledFrame {
                frame_id,
                data: assembly.reassemble(),
                is_keyframe: assembly.is_keyframe,
                streamer_username: assembly.streamer_username,
                codec: assembly.codec,
                description: None,
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

    #[test]
    fn fec_recovers_single_missing_packet() {
        let mut receiver = VideoReceiver::new();

        // 3-packet frame, packet 1 is lost
        let data0 = b"aaaa";
        let data1 = b"bbbb";
        let data2 = b"cccc";

        // Compute FEC for the group of 3 packets (XOR payloads + sizes)
        let mut xor_payload = [0u8; UDP_MAX_PAYLOAD];
        for (j, &b) in data0.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data1.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data2.iter().enumerate() { xor_payload[j] ^= b; }
        let size_xor = (data0.len() as u16) ^ (data1.len() as u16) ^ (data2.len() as u16);

        // Send packets 0 and 2 (packet 1 is lost)
        let pkt0 = make_packet(1, 0, 3, false, data0);
        let pkt2 = make_packet(1, 2, 3, false, data2);
        assert!(receiver.process_packet(&pkt0).is_none());
        assert!(receiver.process_packet(&pkt2).is_none());

        // Send FEC packet — should recover packet 1 and complete the frame
        let fec_pkt = UdpFecPacket::new("streamer1", 1, 0, 3, size_xor, &xor_payload);
        let result = receiver.process_fec_packet(&fec_pkt);
        assert!(result.is_some());
        let frame = result.unwrap();
        assert_eq!(frame.frame_id, 1);
        // Reassembled: data0 + recovered_data1 + data2
        assert_eq!(&frame.data[..4], data0.as_slice());
        assert_eq!(&frame.data[4..8], data1.as_slice());
        assert_eq!(&frame.data[8..12], data2.as_slice());
    }

    #[test]
    fn fec_cannot_recover_two_missing_packets() {
        let mut receiver = VideoReceiver::new();

        // 3-packet frame, packets 1 and 2 are both lost — FEC can't help
        let data0 = b"aaaa";
        let data1 = b"bbbb";
        let data2 = b"cccc";

        let mut xor_payload = [0u8; UDP_MAX_PAYLOAD];
        for (j, &b) in data0.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data1.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data2.iter().enumerate() { xor_payload[j] ^= b; }
        let size_xor = (data0.len() as u16) ^ (data1.len() as u16) ^ (data2.len() as u16);

        let pkt0 = make_packet(1, 0, 3, false, data0);
        assert!(receiver.process_packet(&pkt0).is_none());

        let fec_pkt = UdpFecPacket::new("streamer1", 1, 0, 3, size_xor, &xor_payload);
        let result = receiver.process_fec_packet(&fec_pkt);
        assert!(result.is_none()); // can't recover 2 missing packets
    }

    #[test]
    fn video_packet_triggers_fec_recovery_if_fec_arrived_first() {
        let mut receiver = VideoReceiver::new();

        // 3-packet frame: send pkt0, then FEC, then pkt2 (pkt1 lost)
        let data0 = b"xxxx";
        let data1 = b"yyyy";
        let data2 = b"zzzz";

        let mut xor_payload = [0u8; UDP_MAX_PAYLOAD];
        for (j, &b) in data0.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data1.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data2.iter().enumerate() { xor_payload[j] ^= b; }
        let size_xor = (data0.len() as u16) ^ (data1.len() as u16) ^ (data2.len() as u16);

        let pkt0 = make_packet(1, 0, 3, false, data0);
        assert!(receiver.process_packet(&pkt0).is_none());

        // FEC arrives before pkt2 — can't recover yet (2 missing)
        let fec_pkt = UdpFecPacket::new("streamer1", 1, 0, 3, size_xor, &xor_payload);
        assert!(receiver.process_fec_packet(&fec_pkt).is_none());

        // pkt2 arrives — now only 1 missing, FEC should kick in
        let pkt2 = make_packet(1, 2, 3, false, data2);
        let result = receiver.process_packet(&pkt2);
        assert!(result.is_some());
        let frame = result.unwrap();
        assert_eq!(&frame.data[0..4], data0.as_slice());
        assert_eq!(&frame.data[4..8], data1.as_slice());
        assert_eq!(&frame.data[8..12], data2.as_slice());
    }
}
