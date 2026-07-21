use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::video_packet::{UdpVideoPacket, UdpFecPacket, UDP_MAX_PAYLOAD};

/// Wire-safety caps. `total_packets` comes straight off an untrusted
/// datagram and sizes the reassembly buffer (`total_packets *
/// UDP_MAX_PAYLOAD`); unbounded, a single crafted packet allocates ~75 MB.
/// A 4K IDR is well under 2048 fragments, so anything larger is malformed.
/// `MAX_FRAMES_IN_PROGRESS` bounds concurrent partial frames so a peer
/// spraying distinct frame_ids can't grow memory without bound.
const MAX_PACKETS_PER_FRAME: u16 = 2048;
const MAX_FRAMES_IN_PROGRESS: usize = 64;

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
///
/// Hot-path layout: one contiguous buffer with a fixed 1400-byte slot
/// per packet index, plus a per-slot size table. The old shape
/// (HashMap<u16, Vec<u8>>) allocated a Vec per fragment (~2000/sec at
/// 1080p60) and then re-copied everything through an unsized Vec in
/// reassemble(); this does one allocation per *frame* and compacts in
/// place when the frame completes.
struct FrameAssembly {
    total_packets: u16,
    /// Slot i occupies buf[i*UDP_MAX_PAYLOAD ..][..sizes[i]].
    buf: Vec<u8>,
    /// Per-slot payload size; None = not received yet.
    sizes: Vec<Option<u16>>,
    received_count: u16,
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
    fn new(total_packets: u16, is_keyframe: bool, streamer_username: String, codec: u8) -> Self {
        FrameAssembly {
            total_packets,
            buf: vec![0u8; total_packets as usize * UDP_MAX_PAYLOAD],
            sizes: vec![None; total_packets as usize],
            received_count: 0,
            is_keyframe,
            created_at: Instant::now(),
            streamer_username,
            codec,
            fec_groups: Vec::new(),
            fec_recovered: false,
        }
    }

    /// Store a fragment. Out-of-range indices and oversized payloads
    /// are dropped (wire safety); duplicates (NACK retransmits)
    /// overwrite their slot without double-counting.
    fn insert(&mut self, index: u16, payload: &[u8]) {
        if index >= self.total_packets || payload.len() > UDP_MAX_PAYLOAD {
            return;
        }
        let start = index as usize * UDP_MAX_PAYLOAD;
        self.buf[start..start + payload.len()].copy_from_slice(payload);
        if self.sizes[index as usize]
            .replace(payload.len() as u16)
            .is_none()
        {
            self.received_count += 1;
        }
    }

    fn is_complete(&self) -> bool {
        self.received_count == self.total_packets
    }

    /// Try FEC recovery across all stored FEC groups. Returns true if a packet
    /// was recovered and the frame is now complete.
    fn try_fec_recovery(&mut self) -> bool {
        if self.fec_recovered || self.fec_groups.is_empty() {
            return false;
        }
        if self.is_complete() {
            return false; // already complete
        }

        for fec in &self.fec_groups {
            // saturating_add: group_start/group_count are untrusted u16s;
            // a crafted FEC packet could overflow the sum (release wraps,
            // debug panics) and corrupt the recovery window.
            let group_end = fec.group_start.saturating_add(fec.group_count).min(self.total_packets);

            // Count missing packets in this FEC group
            let mut missing_count = 0u16;
            let mut missing_idx = 0u16;
            for i in fec.group_start..group_end {
                if self.sizes[i as usize].is_none() {
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
                if let Some(size) = self.sizes[i as usize] {
                    // XOR payload bytes (received packets are variable-length,
                    // but FEC was computed with zero-padding to UDP_MAX_PAYLOAD)
                    let start = i as usize * UDP_MAX_PAYLOAD;
                    for (j, &b) in self.buf[start..start + size as usize].iter().enumerate() {
                        recovered[j] ^= b;
                    }
                    recovered_size ^= size;
                }
            }

            // Validate recovered size
            if recovered_size as usize <= UDP_MAX_PAYLOAD {
                let start = missing_idx as usize * UDP_MAX_PAYLOAD;
                self.buf[start..start + recovered_size as usize]
                    .copy_from_slice(&recovered[..recovered_size as usize]);
                self.sizes[missing_idx as usize] = Some(recovered_size);
                self.received_count += 1;
                self.fec_recovered = true;
                log::debug!("[video-recv] FEC recovered packet {} (frame has {}/{} now)",
                    missing_idx, self.received_count, self.total_packets);
                return self.is_complete();
            }
        }

        false
    }

    /// Compact the slot buffer into contiguous frame data, in packet
    /// order, reusing the buffer itself (copy_within is a memmove and
    /// every destination offset is <= its source offset).
    fn into_data(mut self) -> Vec<u8> {
        let mut write = 0usize;
        for i in 0..self.total_packets as usize {
            let size = match self.sizes[i] {
                Some(s) => s as usize,
                None => continue,
            };
            let start = i * UDP_MAX_PAYLOAD;
            if start != write {
                self.buf.copy_within(start..start + size, write);
            }
            write += size;
        }
        self.buf.truncate(write);
        self.buf
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
        let is_keyframe = pkt.is_keyframe();
        let pkt_codec = { pkt.codec };

        // Wire safety: a frame with zero packets or an index outside
        // its own declared range is malformed.
        if total_packets == 0 || packet_index >= total_packets {
            return None;
        }

        // Wire safety: cap the declared fragment count so a single crafted
        // packet can't force a huge (~75 MB) reassembly-buffer allocation.
        if total_packets > MAX_PACKETS_PER_FRAME {
            return None;
        }

        // Bound concurrent partial frames: evict the oldest so a peer
        // spraying distinct frame_ids can't grow memory without bound.
        if !self.frames_in_progress.contains_key(&frame_id)
            && self.frames_in_progress.len() >= MAX_FRAMES_IN_PROGRESS
        {
            let oldest = self
                .frames_in_progress
                .iter()
                .min_by_key(|(_, a)| a.created_at)
                .map(|(&id, _)| id);
            if let Some(oldest) = oldest {
                self.frames_in_progress.remove(&oldest);
                self.nack_tracking.retain(|&(fid, _), _| fid != oldest);
            }
        }

        let frame = self.frames_in_progress.entry(frame_id).or_insert_with(|| {
            // sender_username() allocates — only pay it when a new
            // assembly is created (per frame), not per fragment.
            FrameAssembly::new(total_packets, is_keyframe, pkt.sender_username(), pkt_codec)
        });

        frame.insert(packet_index, pkt.payload_data());

        // Check if frame is complete (directly or after FEC recovery)
        let complete = frame.is_complete() || frame.try_fec_recovery();

        if complete {
            let mut assembly = self.frames_in_progress.remove(&frame_id).unwrap();
            self.last_complete_frame_id = Some(frame_id);
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);

            return Some(ReassembledFrame {
                frame_id,
                is_keyframe: assembly.is_keyframe,
                streamer_username: std::mem::take(&mut assembly.streamer_username),
                codec: assembly.codec,
                description: None,
                data: assembly.into_data(),
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
            let mut assembly = self.frames_in_progress.remove(&frame_id).unwrap();
            self.last_complete_frame_id = Some(frame_id);
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);

            return Some(ReassembledFrame {
                frame_id,
                is_keyframe: assembly.is_keyframe,
                streamer_username: std::mem::take(&mut assembly.streamer_username),
                codec: assembly.codec,
                description: None,
                data: assembly.into_data(),
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
                    if assembly.sizes[i as usize].is_none() {
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
                if assembly.received_count < assembly.total_packets {
                    dropped += 1;
                }
                false // remove
            } else {
                true // keep
            }
        });
        if dropped > 0 {
            log::info!("[video-recv] Dropped {} incomplete frames", dropped);
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

    // ── Wire-condition tests for the contiguous-slot reassembly ──
    // Self-preview never exercises this path (frames go encoder→TSFN
    // directly), so these simulate what a real watcher session sees:
    // big shuffled keyframes, NACK-retransmit duplicates, malformed
    // headers, and FEC recovery of the short tail fragment.

    /// Deterministic Fisher–Yates with an inline LCG (no rand dep).
    fn shuffle<T>(items: &mut [T], mut seed: u64) {
        for i in (1..items.len()).rev() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let j = (seed >> 33) as usize % (i + 1);
            items.swap(i, j);
        }
    }

    #[test]
    fn large_shuffled_keyframe_reassembles_byte_exact() {
        // ~200KB keyframe: 143 full fragments + one short tail, the
        // realistic worst case for a 1080p60 IDR frame.
        let total: u16 = 144;
        let tail_len = 137usize;
        let mut expected = Vec::new();
        let mut payloads: Vec<Vec<u8>> = Vec::new();
        for i in 0..total {
            let len = if i == total - 1 { tail_len } else { UDP_MAX_PAYLOAD };
            let chunk: Vec<u8> =
                (0..len).map(|j| (i as usize * 31 + j) as u8).collect();
            expected.extend_from_slice(&chunk);
            payloads.push(chunk);
        }

        let mut order: Vec<u16> = (0..total).collect();
        shuffle(&mut order, 0xDEC1BE11);

        let mut receiver = VideoReceiver::new();
        let mut completed = None;
        for (n, &i) in order.iter().enumerate() {
            let pkt = make_packet(7, i, total, true, &payloads[i as usize]);
            let res = receiver.process_packet(&pkt);
            if n + 1 < order.len() {
                assert!(res.is_none(), "completed early at packet {}", n);
            } else {
                completed = res;
            }
        }
        let frame = completed.expect("frame must complete on last fragment");
        assert_eq!(frame.data.len(), expected.len());
        assert_eq!(frame.data, expected);
        assert!(frame.is_keyframe);
    }

    #[test]
    fn duplicate_fragments_do_not_double_count_or_corrupt() {
        // NACK retransmits deliver the same fragment twice; the frame
        // must complete exactly when all *distinct* fragments arrived.
        let mut receiver = VideoReceiver::new();
        let pkt0 = make_packet(1, 0, 3, false, b"aaaa");
        let pkt1 = make_packet(1, 1, 3, false, b"bbbb");
        let pkt2 = make_packet(1, 2, 3, false, b"cccc");

        assert!(receiver.process_packet(&pkt0).is_none());
        assert!(receiver.process_packet(&pkt0).is_none()); // retransmit
        assert!(receiver.process_packet(&pkt1).is_none());
        assert!(receiver.process_packet(&pkt1).is_none()); // retransmit
        let frame = receiver.process_packet(&pkt2).expect("complete on 3rd distinct");
        assert_eq!(frame.data, b"aaaabbbbcccc");
    }

    #[test]
    fn out_of_range_index_is_ignored() {
        let mut receiver = VideoReceiver::new();
        // index 5 in a 3-packet frame: malformed, must not panic and
        // must not pollute the assembly.
        let bogus = make_packet(1, 5, 3, false, b"evil");
        assert!(receiver.process_packet(&bogus).is_none());

        let pkt0 = make_packet(1, 0, 3, false, b"aaaa");
        let pkt1 = make_packet(1, 1, 3, false, b"bbbb");
        let pkt2 = make_packet(1, 2, 3, false, b"cccc");
        assert!(receiver.process_packet(&pkt0).is_none());
        assert!(receiver.process_packet(&pkt1).is_none());
        let frame = receiver.process_packet(&pkt2).expect("valid fragments complete");
        assert_eq!(frame.data, b"aaaabbbbcccc");
    }

    #[test]
    fn zero_total_packets_is_rejected() {
        let mut receiver = VideoReceiver::new();
        let bogus = make_packet(1, 0, 0, false, b"evil");
        assert!(receiver.process_packet(&bogus).is_none());
    }

    #[test]
    fn oversized_total_packets_is_rejected() {
        // A crafted packet claiming an enormous fragment count must be
        // dropped before it allocates total_packets * UDP_MAX_PAYLOAD
        // (~75 MB at u16::MAX) and must not create an assembly entry.
        let mut receiver = VideoReceiver::new();
        let evil = make_packet(1, 0, u16::MAX, true, b"boom");
        assert!(receiver.process_packet(&evil).is_none());
        let over = make_packet(2, 0, MAX_PACKETS_PER_FRAME + 1, true, b"boom");
        assert!(receiver.process_packet(&over).is_none());
        assert_eq!(receiver.frames_in_progress.len(), 0);

        // A frame right at the cap is still accepted (creates an assembly).
        let ok = make_packet(3, 0, MAX_PACKETS_PER_FRAME, false, b"ok");
        assert!(receiver.process_packet(&ok).is_none());
        assert_eq!(receiver.frames_in_progress.len(), 1);
    }

    #[test]
    fn concurrent_partial_frames_are_bounded() {
        // Spraying distinct frame_ids that never complete must not grow
        // frames_in_progress without bound.
        let mut receiver = VideoReceiver::new();
        for fid in 0..(MAX_FRAMES_IN_PROGRESS as u32 + 50) {
            // total=2 so the frame never completes on a single fragment
            let pkt = make_packet(fid, 0, 2, false, b"x");
            assert!(receiver.process_packet(&pkt).is_none());
        }
        assert!(receiver.frames_in_progress.len() <= MAX_FRAMES_IN_PROGRESS);
    }

    #[test]
    fn fec_recovers_short_tail_fragment() {
        // The lost fragment is the short tail — recovery must restore
        // its exact (non-padded) length, and compaction must place it
        // flush after the full-size fragments.
        let mut receiver = VideoReceiver::new();
        let data0: Vec<u8> = vec![0xAA; UDP_MAX_PAYLOAD];
        let data1: Vec<u8> = vec![0xBB; UDP_MAX_PAYLOAD];
        let data2: Vec<u8> = (0..200u16).map(|j| j as u8).collect();

        let mut xor_payload = [0u8; UDP_MAX_PAYLOAD];
        for (j, &b) in data0.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data1.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data2.iter().enumerate() { xor_payload[j] ^= b; }
        let size_xor =
            (data0.len() as u16) ^ (data1.len() as u16) ^ (data2.len() as u16);

        assert!(receiver.process_packet(&make_packet(9, 0, 3, true, &data0)).is_none());
        assert!(receiver.process_packet(&make_packet(9, 1, 3, true, &data1)).is_none());
        // tail (index 2) lost — FEC packet completes the frame
        let fec = UdpFecPacket::new("streamer1", 9, 0, 3, size_xor, &xor_payload);
        let frame = receiver.process_fec_packet(&fec).expect("FEC recovery");
        assert_eq!(frame.data.len(), UDP_MAX_PAYLOAD * 2 + 200);
        assert_eq!(&frame.data[..UDP_MAX_PAYLOAD], data0.as_slice());
        assert_eq!(&frame.data[UDP_MAX_PAYLOAD..UDP_MAX_PAYLOAD * 2], data1.as_slice());
        assert_eq!(&frame.data[UDP_MAX_PAYLOAD * 2..], data2.as_slice());
    }
}
