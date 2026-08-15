use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use super::video_packet::{
    UdpFecPacket, UdpVideoPacket, FEC_GROUP_SIZE, SENDER_ID_SIZE, UDP_MAX_PAYLOAD,
};

/// Wire-safety caps. `total_packets` comes straight off an untrusted
/// datagram and sizes the reassembly buffer (`total_packets *
/// UDP_MAX_PAYLOAD`); unbounded, a single crafted packet allocates ~75 MB.
/// A 4K IDR is well under 2048 fragments, so anything larger is malformed.
/// `MAX_FRAMES_IN_PROGRESS` bounds concurrent partial frames per sender so
/// a peer spraying distinct frame_ids can't grow memory without bound.
const MAX_PACKETS_PER_FRAME: u16 = 2048;
const MAX_FRAMES_IN_PROGRESS: usize = 64;

/// Concurrent streamer bound. Real sessions have a handful of streams at
/// most; anything past this evicts the least-recently-active sender.
const MAX_SENDERS: usize = 8;

/// One lifetime for incomplete assemblies, shared by every cleanup path.
/// (There used to be two — 150ms inside check_missing and 500ms in
/// cleanup_stale — which made the effective NACK window depend on which
/// ran first.)
const STALE_FRAME_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a completed-but-out-of-order frame may wait for its
/// predecessor (in flight via NACK/FEC) before the gap is declared lost.
/// Comfortably covers one NACK round trip; short enough that a real loss
/// converges on a PLI quickly.
const DELIVERY_HOLD: Duration = Duration::from_millis(80);
const MAX_PENDING_DELIVERY: usize = 32;

/// A keyframe behind the delivery cursor by less than this is a late
/// retransmit (drop); by this much or more it's a stream restart with
/// frame ids re-zeroed (accept). ~8s of frames at 60fps.
const RESTART_THRESHOLD: u32 = 512;

/// FEC bookkeeping cap: one group per FEC_GROUP_SIZE fragments, plus one.
/// Without this a peer could push unlimited FEC packets into one frame's
/// `fec_groups` Vec.
const MAX_FEC_GROUPS: usize = (MAX_PACKETS_PER_FRAME as usize / FEC_GROUP_SIZE as usize) + 1;

/// Senders with no packets for this long are forgotten entirely.
const SENDER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// A batch of retransmit requests for one frame of one streamer.
pub struct NackRequest {
    pub target: String,
    pub frame_id: u32,
    pub missing: Vec<u16>,
}

/// Stored FEC group data for recovery.
struct FecGroup {
    group_start: u16,
    group_count: u16,
    payload_size_xor: u16,
    payload: [u8; UDP_MAX_PAYLOAD],
    /// Set once this group has reconstructed its missing packet, so a
    /// duplicate FEC packet can't recover twice.
    used: bool,
}

/// Tracks in-progress frame assembly.
///
/// Hot-path layout: one contiguous buffer with a fixed slot per packet
/// index, plus a per-slot size table. One allocation per *frame*;
/// compacts in place when the frame completes.
struct FrameAssembly {
    total_packets: u16,
    /// Slot i occupies buf[i*UDP_MAX_PAYLOAD ..][..sizes[i]].
    buf: Vec<u8>,
    /// Per-slot payload size; None = not received yet.
    sizes: Vec<Option<u16>>,
    received_count: u16,
    is_keyframe: bool,
    created_at: Instant,
    /// Codec byte from the first packet of the frame. Plan B: drives
    /// WebCodecs decoder configuration on the viewer side.
    codec: u8,
    fec_groups: Vec<FecGroup>,
}

impl FrameAssembly {
    fn new(total_packets: u16, is_keyframe: bool, codec: u8) -> Self {
        FrameAssembly {
            total_packets,
            buf: vec![0u8; total_packets as usize * UDP_MAX_PAYLOAD],
            sizes: vec![None; total_packets as usize],
            received_count: 0,
            is_keyframe,
            created_at: Instant::now(),
            codec,
            fec_groups: Vec::new(),
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

    /// Add a FEC group, bounded and deduplicated by group_start (NACK-era
    /// retransmit duplicates would otherwise pile up).
    fn add_fec_group(&mut self, group: FecGroup) {
        if self.fec_groups.len() >= MAX_FEC_GROUPS {
            return;
        }
        if self
            .fec_groups
            .iter()
            .any(|g| g.group_start == group.group_start)
        {
            return;
        }
        self.fec_groups.push(group);
    }

    /// Try FEC recovery. Each group can independently recover one missing
    /// packet, so a frame missing one packet in each of two groups is
    /// still recoverable (the old per-frame `fec_recovered` flag allowed
    /// only one recovery total). Loops until a full pass makes no
    /// progress. Returns true if the frame is now complete.
    fn try_fec_recovery(&mut self) -> bool {
        if self.fec_groups.is_empty() || self.is_complete() {
            return self.is_complete();
        }

        loop {
            let mut progressed = false;
            for gi in 0..self.fec_groups.len() {
                if self.fec_groups[gi].used {
                    continue;
                }
                // saturating_add: group_start/group_count are untrusted u16s;
                // a crafted FEC packet could overflow the sum (release wraps,
                // debug panics) and corrupt the recovery window.
                let group_start = self.fec_groups[gi].group_start;
                let group_end = group_start
                    .saturating_add(self.fec_groups[gi].group_count)
                    .min(self.total_packets);

                // Count missing packets in this FEC group
                let mut missing_count = 0u16;
                let mut missing_idx = 0u16;
                for i in group_start..group_end {
                    if self.sizes[i as usize].is_none() {
                        missing_count += 1;
                        missing_idx = i;
                    }
                }

                // Can only recover if exactly 1 packet is missing in the group
                if missing_count != 1 {
                    continue;
                }

                // Reconstruct: XOR the FEC payload with all received packets
                // in the group
                let mut recovered = self.fec_groups[gi].payload;
                let mut recovered_size = self.fec_groups[gi].payload_size_xor;

                for i in group_start..group_end {
                    if i == missing_idx {
                        continue;
                    }
                    if let Some(size) = self.sizes[i as usize] {
                        // XOR payload bytes (received packets are variable-
                        // length, but FEC was computed with zero-padding to
                        // UDP_MAX_PAYLOAD)
                        let start = i as usize * UDP_MAX_PAYLOAD;
                        for (j, &b) in
                            self.buf[start..start + size as usize].iter().enumerate()
                        {
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
                    self.fec_groups[gi].used = true;
                    progressed = true;
                    log::debug!(
                        "[video-recv] FEC recovered packet {} (frame has {}/{} now)",
                        missing_idx,
                        self.received_count,
                        self.total_packets
                    );
                    if self.is_complete() {
                        return true;
                    }
                }
            }
            if !progressed {
                return false;
            }
        }
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

/// Wrap-aware "a strictly before b" for u32 frame ids.
fn seq_before(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < 0x8000_0000
}

/// Per-streamer receive state. Everything is keyed per sender because
/// each streamer numbers its frames independently from 0 — a shared
/// frame_id keyspace made two concurrent streams reassemble into each
/// other's buffers and pointed NACK/PLI at whichever streamer's packet
/// happened to arrive last.
struct SenderState {
    username: String,
    frames_in_progress: HashMap<u32, FrameAssembly>,
    /// Completed frames waiting for their predecessor (in flight via
    /// NACK/FEC) so delivery stays in frame_id order. Only ever holds
    /// deltas — keyframes deliver immediately and reset the chain.
    pending_delivery: BTreeMap<u32, (Instant, ReassembledFrame)>,
    /// The frame_id the next in-order delta must have. None = waiting
    /// for a keyframe (initial state, and after a gap was declared
    /// lost); deltas are undecodable in that state and are dropped.
    next_delivery: Option<u32>,
    nack_tracking: HashMap<(u32, u16), (Instant, u32)>, // (frame_id, pkt_idx) -> (last_nack_time, retry_count)
    /// True once any keyframe has been delivered — drives the PLI
    /// cadence (aggressive until first picture, relaxed after).
    has_keyframe: bool,
    last_pli: Option<Instant>,
    last_activity: Instant,
    /// Sticky PLI request raised by delivery-order give-ups; consumed
    /// by check_missing.
    want_pli: bool,
}

impl SenderState {
    fn new(username: String, now: Instant) -> Self {
        SenderState {
            username,
            frames_in_progress: HashMap::new(),
            pending_delivery: BTreeMap::new(),
            next_delivery: None,
            nack_tracking: HashMap::new(),
            has_keyframe: false,
            last_pli: None,
            last_activity: now,
            want_pli: false,
        }
    }

    /// Route a completed frame through the ordered-delivery gate.
    /// Returns every frame that is now deliverable, in order. Out-of-
    /// order completions (NACK/FEC finishing an older frame after a
    /// newer one) used to be handed to the decoder as-is, which fed it
    /// deltas with missing references — corruption until the next PLI.
    fn accept_completed(&mut self, frame: ReassembledFrame, now: Instant) -> Vec<ReassembledFrame> {
        let fid = frame.frame_id;
        if frame.is_keyframe {
            // A keyframe *slightly* behind the cursor is a late NACK
            // retransmit of one we already moved past — delivering it
            // would rewind the chain. A keyframe *far* behind is a
            // stream restart (ids reset to 0) and must be accepted.
            if let Some(next) = self.next_delivery {
                let behind = next.wrapping_sub(fid);
                if behind > 0 && behind < RESTART_THRESHOLD {
                    return Vec::new();
                }
            }
            // A keyframe is always decodable and resets the chain.
            // Anything pending from before it is superseded.
            self.pending_delivery.retain(|&k, _| seq_before(fid, k));
            self.next_delivery = Some(fid.wrapping_add(1));
            self.has_keyframe = true;
            let mut out = vec![frame];
            out.extend(self.flush_pending());
            return out;
        }
        match self.next_delivery {
            // No keyframe yet (or the chain was abandoned after a lost
            // gap): a delta has nothing to predict from.
            None => Vec::new(),
            Some(next) => {
                if fid == next {
                    self.next_delivery = Some(fid.wrapping_add(1));
                    let mut out = vec![frame];
                    out.extend(self.flush_pending());
                    out
                } else if seq_before(fid, next) {
                    // Late duplicate / already-superseded frame.
                    Vec::new()
                } else {
                    // Gap before this frame — hold briefly; the missing
                    // frame is usually a NACK round-trip away.
                    if self.pending_delivery.len() < MAX_PENDING_DELIVERY {
                        self.pending_delivery.insert(fid, (now, frame));
                    }
                    Vec::new()
                }
            }
        }
    }

    fn flush_pending(&mut self) -> Vec<ReassembledFrame> {
        let mut out = Vec::new();
        while let Some(next) = self.next_delivery {
            match self.pending_delivery.remove(&next) {
                Some((_, f)) => {
                    self.next_delivery = Some(next.wrapping_add(1));
                    out.push(f);
                }
                None => break,
            }
        }
        out
    }

    /// Give up on delivery gaps older than DELIVERY_HOLD. Once the gap
    /// frame is declared lost every held delta after it is undecodable,
    /// so the whole pending set is dropped and the chain re-arms on the
    /// next keyframe (via want_pli → PLI → IDR).
    fn expire_pending(&mut self, now: Instant) {
        let expired = self
            .pending_delivery
            .values()
            .next()
            .map(|(held_at, _)| now.duration_since(*held_at) > DELIVERY_HOLD)
            .unwrap_or(false);
        if expired {
            log::debug!(
                "[video-recv] '{}': gap before frame {:?} not recovered in {:?} — dropping {} held frames, requesting keyframe",
                self.username,
                self.pending_delivery.keys().next(),
                DELIVERY_HOLD,
                self.pending_delivery.len(),
            );
            self.pending_delivery.clear();
            self.next_delivery = None;
            self.want_pli = true;
        }
    }
}

/// Jitter buffer and frame reassembly for incoming video packets,
/// keyed per streamer.
pub struct VideoReceiver {
    senders: HashMap<[u8; SENDER_ID_SIZE], SenderState>,
    nack_timeout: Duration,
    max_nack_retries: u32,
    /// Minimum spacing between PLIs to one sender, by keyframe state.
    pli_interval_cold: Duration,
    pli_interval_warm: Duration,
}

impl VideoReceiver {
    pub fn new() -> Self {
        VideoReceiver {
            senders: HashMap::new(),
            nack_timeout: Duration::from_millis(50),
            max_nack_retries: 3,
            pli_interval_cold: Duration::from_millis(500),
            pli_interval_warm: Duration::from_secs(1),
        }
    }

    fn sender_entry(&mut self, id: &[u8; SENDER_ID_SIZE], username: impl FnOnce() -> String, now: Instant) -> &mut SenderState {
        if !self.senders.contains_key(id) && self.senders.len() >= MAX_SENDERS {
            // Evict the least-recently-active sender.
            if let Some(&stale) = self
                .senders
                .iter()
                .min_by_key(|(_, s)| s.last_activity)
                .map(|(k, _)| k)
            {
                self.senders.remove(&stale);
            }
        }
        self.senders
            .entry(*id)
            .or_insert_with(|| SenderState::new(username(), now))
    }

    /// Process an incoming video packet. Returns every frame that became
    /// deliverable, in decode order (usually empty or one).
    pub fn process_packet(&mut self, pkt: &UdpVideoPacket) -> Vec<ReassembledFrame> {
        // Copy fields from packed struct to avoid unaligned access UB
        let frame_id = { pkt.frame_id };
        let packet_index = { pkt.packet_index };
        let total_packets = { pkt.total_packets };
        let is_keyframe = pkt.is_keyframe();
        let pkt_codec = { pkt.codec };

        // Wire safety: a frame with zero packets or an index outside
        // its own declared range is malformed.
        if total_packets == 0 || packet_index >= total_packets {
            return Vec::new();
        }

        // Wire safety: cap the declared fragment count so a single crafted
        // packet can't force a huge (~75 MB) reassembly-buffer allocation.
        if total_packets > MAX_PACKETS_PER_FRAME {
            return Vec::new();
        }

        let now = Instant::now();
        let sender = self.sender_entry(&pkt.sender_id, || pkt.sender_username(), now);
        sender.last_activity = now;

        // Skip fragments for frames the delivery cursor has already
        // passed — late NACK retransmits for delivered frames would
        // otherwise rebuild a whole assembly that can never deliver.
        if let Some(next) = sender.next_delivery {
            if !is_keyframe && seq_before(frame_id, next) {
                return Vec::new();
            }
        }

        // Bound concurrent partial frames: evict the oldest so a peer
        // spraying distinct frame_ids can't grow memory without bound.
        if !sender.frames_in_progress.contains_key(&frame_id)
            && sender.frames_in_progress.len() >= MAX_FRAMES_IN_PROGRESS
        {
            let oldest = sender
                .frames_in_progress
                .iter()
                .min_by_key(|(_, a)| a.created_at)
                .map(|(&id, _)| id);
            if let Some(oldest) = oldest {
                sender.frames_in_progress.remove(&oldest);
                sender.nack_tracking.retain(|&(fid, _), _| fid != oldest);
            }
        }

        let frame = sender
            .frames_in_progress
            .entry(frame_id)
            .or_insert_with(|| FrameAssembly::new(total_packets, is_keyframe, pkt_codec));

        frame.insert(packet_index, pkt.payload_data());

        // Check if frame is complete (directly or after FEC recovery)
        let complete = frame.is_complete() || frame.try_fec_recovery();

        if complete {
            let assembly = sender.frames_in_progress.remove(&frame_id).unwrap();
            sender.nack_tracking.retain(|&(fid, _), _| fid != frame_id);
            let reassembled = ReassembledFrame {
                frame_id,
                is_keyframe: assembly.is_keyframe,
                streamer_username: sender.username.clone(),
                codec: assembly.codec,
                description: None,
                data: assembly.into_data(),
            };
            return sender.accept_completed(reassembled, now);
        }

        Vec::new()
    }

    /// Process an incoming FEC packet. Returns frames that became
    /// deliverable through recovery.
    pub fn process_fec_packet(&mut self, pkt: &UdpFecPacket) -> Vec<ReassembledFrame> {
        let frame_id = { pkt.frame_id };
        let group_start = { pkt.group_start };
        let group_count = { pkt.group_count };
        let payload_size_xor = { pkt.payload_size_xor };

        let now = Instant::now();
        let Some(sender) = self.senders.get_mut(&pkt.sender_id) else {
            return Vec::new();
        };
        sender.last_activity = now;
        let Some(frame) = sender.frames_in_progress.get_mut(&frame_id) else {
            return Vec::new();
        };

        frame.add_fec_group(FecGroup {
            group_start,
            group_count,
            payload_size_xor,
            payload: pkt.payload,
            used: false,
        });

        if frame.try_fec_recovery() {
            let assembly = sender.frames_in_progress.remove(&frame_id).unwrap();
            sender.nack_tracking.retain(|&(fid, _), _| fid != frame_id);
            let reassembled = ReassembledFrame {
                frame_id,
                is_keyframe: assembly.is_keyframe,
                streamer_username: sender.username.clone(),
                codec: assembly.codec,
                description: None,
                data: assembly.into_data(),
            };
            return sender.accept_completed(reassembled, now);
        }

        Vec::new()
    }

    /// Periodic maintenance: expire delivery holds, compute NACK
    /// retransmit requests, and decide which streamers need a PLI.
    /// Returns (nacks, pli_targets). PLI targets are already throttled
    /// per sender — send one UdpKeyframeRequest to each name returned.
    pub fn check_missing(&mut self) -> (Vec<NackRequest>, Vec<String>) {
        let now = Instant::now();
        let mut nacks: Vec<NackRequest> = Vec::new();
        let mut plis: Vec<String> = Vec::new();

        for sender in self.senders.values_mut() {
            sender.expire_pending(now);
            let mut need_pli = sender.want_pli;

            for (&frame_id, assembly) in &sender.frames_in_progress {
                // Find missing packet indices once the frame has had a
                // chance to arrive in full.
                if assembly.created_at.elapsed() > self.nack_timeout {
                    let mut missing = Vec::new();
                    for i in 0..assembly.total_packets {
                        if assembly.sizes[i as usize].is_none() {
                            let key = (frame_id, i);
                            // Initial entry uses a past timestamp so the first
                            // NACK fires immediately
                            let entry = sender.nack_tracking.entry(key).or_insert((
                                now - self.nack_timeout - Duration::from_millis(1),
                                0,
                            ));
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
                        nacks.push(NackRequest {
                            target: sender.username.clone(),
                            frame_id,
                            missing,
                        });
                    }
                }
            }

            if need_pli {
                let interval = if sender.has_keyframe {
                    self.pli_interval_warm
                } else {
                    self.pli_interval_cold
                };
                let due = sender
                    .last_pli
                    .map(|t| now.duration_since(t) >= interval)
                    .unwrap_or(true);
                if due {
                    sender.last_pli = Some(now);
                    sender.want_pli = false;
                    plis.push(sender.username.clone());
                }
            }
        }

        (nacks, plis)
    }

    /// Drop incomplete assemblies older than STALE_FRAME_TIMEOUT and
    /// forget senders that have gone quiet.
    pub fn cleanup_stale(&mut self) {
        let mut dropped = 0u32;
        for sender in self.senders.values_mut() {
            sender.frames_in_progress.retain(|_frame_id, assembly| {
                if assembly.created_at.elapsed() >= STALE_FRAME_TIMEOUT {
                    dropped += 1;
                    false
                } else {
                    true
                }
            });
            let frames = &sender.frames_in_progress;
            sender
                .nack_tracking
                .retain(|&(fid, _), _| frames.contains_key(&fid));
        }
        if dropped > 0 {
            log::info!("[video-recv] Dropped {} incomplete frames", dropped);
        }
        self.senders
            .retain(|_, s| s.last_activity.elapsed() < SENDER_IDLE_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_packet(frame_id: u32, index: u16, total: u16, keyframe: bool, data: &[u8]) -> UdpVideoPacket {
        UdpVideoPacket::new("streamer1", frame_id, index, total, keyframe, data)
    }

    fn make_packet_from(
        sender: &str,
        frame_id: u32,
        index: u16,
        total: u16,
        keyframe: bool,
        data: &[u8],
    ) -> UdpVideoPacket {
        UdpVideoPacket::new(sender, frame_id, index, total, keyframe, data)
    }

    /// Seed the delivery chain: frame 0 keyframe, single packet.
    fn seed_keyframe(receiver: &mut VideoReceiver, sender: &str) {
        let pkt = make_packet_from(sender, 0, 0, 1, true, b"idr");
        let out = receiver.process_packet(&pkt);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_keyframe);
    }

    #[test]
    fn single_packet_keyframe_completes_immediately() {
        let mut receiver = VideoReceiver::new();
        let pkt = make_packet(0, 0, 1, true, b"keyframe data");
        let result = receiver.process_packet(&pkt);
        assert_eq!(result.len(), 1);
        let frame = &result[0];
        assert_eq!(frame.frame_id, 0);
        assert_eq!(frame.data, b"keyframe data");
        assert!(frame.is_keyframe);
        assert_eq!(frame.streamer_username, "streamer1");
    }

    #[test]
    fn delta_without_keyframe_is_dropped() {
        let mut receiver = VideoReceiver::new();
        let pkt = make_packet(5, 0, 1, false, b"delta");
        assert!(receiver.process_packet(&pkt).is_empty());
    }

    #[test]
    fn multi_packet_frame_completes_on_last() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

        let pkt0 = make_packet(1, 0, 3, false, b"part0");
        let pkt1 = make_packet(1, 1, 3, false, b"part1");
        let pkt2 = make_packet(1, 2, 3, false, b"part2");

        assert!(receiver.process_packet(&pkt0).is_empty());
        assert!(receiver.process_packet(&pkt1).is_empty());
        let result = receiver.process_packet(&pkt2);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].data, b"part0part1part2");
    }

    #[test]
    fn out_of_order_packets_still_complete() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

        let pkt2 = make_packet(1, 2, 3, false, b"c");
        let pkt0 = make_packet(1, 0, 3, false, b"a");
        let pkt1 = make_packet(1, 1, 3, false, b"b");

        assert!(receiver.process_packet(&pkt2).is_empty());
        assert!(receiver.process_packet(&pkt0).is_empty());
        let result = receiver.process_packet(&pkt1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].data, b"abc"); // reassembled in order
    }

    #[test]
    fn two_senders_reassemble_independently() {
        // Both senders use frame_id 0/1 — the shared-keyspace bug merged
        // their fragments into one assembly.
        let mut receiver = VideoReceiver::new();
        let a0 = make_packet_from("alice", 0, 0, 2, true, b"aaaa");
        let b0 = make_packet_from("bob", 0, 0, 2, true, b"bbbb");
        let a1 = make_packet_from("alice", 0, 1, 2, true, b"AAAA");
        let b1 = make_packet_from("bob", 0, 1, 2, true, b"BBBB");

        assert!(receiver.process_packet(&a0).is_empty());
        assert!(receiver.process_packet(&b0).is_empty());
        let a = receiver.process_packet(&a1);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].data, b"aaaaAAAA");
        assert_eq!(a[0].streamer_username, "alice");
        let b = receiver.process_packet(&b1);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].data, b"bbbbBBBB");
        assert_eq!(b[0].streamer_username, "bob");
    }

    #[test]
    fn late_completion_delivers_in_order() {
        // Frame 2 completes before frame 1; delivery must hold 2 until 1
        // lands, then release both in order.
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

        // Frame 1: fragment 0 of 2 arrives, fragment 1 delayed.
        assert!(receiver.process_packet(&make_packet(1, 0, 2, false, b"f1a")).is_empty());
        // Frame 2 completes fully.
        assert!(receiver.process_packet(&make_packet(2, 0, 1, false, b"f2")).is_empty());
        // Frame 1's missing fragment arrives (NACK retransmit) — both deliver, in order.
        let out = receiver.process_packet(&make_packet(1, 1, 2, false, b"f1b"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].frame_id, 1);
        assert_eq!(out[0].data, b"f1af1b");
        assert_eq!(out[1].frame_id, 2);
        assert_eq!(out[1].data, b"f2");
    }

    #[test]
    fn stale_delta_is_not_delivered() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");
        assert_eq!(receiver.process_packet(&make_packet(1, 0, 1, false, b"f1")).len(), 1);
        assert_eq!(receiver.process_packet(&make_packet(2, 0, 1, false, b"f2")).len(), 1);
        // A late duplicate of frame 1 must not reach the decoder again.
        assert!(receiver.process_packet(&make_packet(1, 0, 1, false, b"f1")).is_empty());
    }

    #[test]
    fn keyframe_resets_chain_and_supersedes_pending() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");
        // Gap: frame 1 never completes; frame 2 completes and is held.
        assert!(receiver.process_packet(&make_packet(2, 0, 1, false, b"f2")).is_empty());
        // Keyframe 3 arrives → delivered; held frame 2 is superseded.
        let out = receiver.process_packet(&make_packet(3, 0, 1, true, b"idr3"));
        assert_eq!(out.len(), 1);
        assert!(out[0].is_keyframe);
        assert_eq!(out[0].frame_id, 3);
        // Chain continues from 4.
        let out = receiver.process_packet(&make_packet(4, 0, 1, false, b"f4"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].frame_id, 4);
    }

    #[test]
    fn missing_packet_detected() {
        let mut receiver = VideoReceiver::new();
        receiver.nack_timeout = Duration::from_millis(1);
        seed_keyframe(&mut receiver, "streamer1");

        let pkt0 = make_packet(1, 0, 3, false, b"a");
        let pkt2 = make_packet(1, 2, 3, false, b"c");
        // pkt1 is missing

        receiver.process_packet(&pkt0);
        receiver.process_packet(&pkt2);

        // Wait for nack timeout to expire
        std::thread::sleep(Duration::from_millis(2));

        let (nacks, _plis) = receiver.check_missing();
        assert_eq!(nacks.len(), 1);
        assert_eq!(nacks[0].target, "streamer1");
        assert_eq!(nacks[0].frame_id, 1);
        assert_eq!(nacks[0].missing, vec![1]); // missing index 1
    }

    #[test]
    fn fec_recovers_single_missing_packet() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

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
        assert!(receiver.process_packet(&pkt0).is_empty());
        assert!(receiver.process_packet(&pkt2).is_empty());

        // Send FEC packet — should recover packet 1 and complete the frame
        let fec_pkt = UdpFecPacket::new("streamer1", 1, 0, 3, size_xor, &xor_payload);
        let result = receiver.process_fec_packet(&fec_pkt);
        assert_eq!(result.len(), 1);
        let frame = &result[0];
        assert_eq!(frame.frame_id, 1);
        // Reassembled: data0 + recovered_data1 + data2
        assert_eq!(&frame.data[..4], data0.as_slice());
        assert_eq!(&frame.data[4..8], data1.as_slice());
        assert_eq!(&frame.data[8..12], data2.as_slice());
    }

    #[test]
    fn fec_recovers_one_packet_in_each_of_two_groups() {
        // The old per-frame `fec_recovered` flag allowed only one FEC
        // recovery per frame; two independent groups must both recover.
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

        // 4-packet frame; groups [0,1] and [2,3]; packets 1 and 3 lost.
        let d0 = b"aaaa"; let d1 = b"bbbb"; let d2 = b"cccc"; let d3 = b"dddd";
        let fec_for = |a: &[u8], b: &[u8]| {
            let mut xor = [0u8; UDP_MAX_PAYLOAD];
            for (j, &x) in a.iter().enumerate() { xor[j] ^= x; }
            for (j, &x) in b.iter().enumerate() { xor[j] ^= x; }
            (xor, (a.len() as u16) ^ (b.len() as u16))
        };
        let (xor_a, size_a) = fec_for(d0, d1);
        let (xor_b, size_b) = fec_for(d2, d3);

        assert!(receiver.process_packet(&make_packet(1, 0, 4, false, d0)).is_empty());
        assert!(receiver.process_packet(&make_packet(1, 2, 4, false, d2)).is_empty());

        let fec_a = UdpFecPacket::new("streamer1", 1, 0, 2, size_a, &xor_a);
        assert!(receiver.process_fec_packet(&fec_a).is_empty()); // recovers pkt 1, frame incomplete
        let fec_b = UdpFecPacket::new("streamer1", 1, 2, 2, size_b, &xor_b);
        let out = receiver.process_fec_packet(&fec_b); // recovers pkt 3 → complete
        assert_eq!(out.len(), 1);
        assert_eq!(&out[0].data[..4], d0.as_slice());
        assert_eq!(&out[0].data[4..8], d1.as_slice());
        assert_eq!(&out[0].data[8..12], d2.as_slice());
        assert_eq!(&out[0].data[12..16], d3.as_slice());
    }

    #[test]
    fn fec_cannot_recover_two_missing_packets_in_one_group() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

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
        assert!(receiver.process_packet(&pkt0).is_empty());

        let fec_pkt = UdpFecPacket::new("streamer1", 1, 0, 3, size_xor, &xor_payload);
        assert!(receiver.process_fec_packet(&fec_pkt).is_empty()); // can't recover 2 missing packets
    }

    #[test]
    fn video_packet_triggers_fec_recovery_if_fec_arrived_first() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");

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
        assert!(receiver.process_packet(&pkt0).is_empty());

        // FEC arrives before pkt2 — can't recover yet (2 missing)
        let fec_pkt = UdpFecPacket::new("streamer1", 1, 0, 3, size_xor, &xor_payload);
        assert!(receiver.process_fec_packet(&fec_pkt).is_empty());

        // pkt2 arrives — now only 1 missing, FEC should kick in
        let pkt2 = make_packet(1, 2, 3, false, data2);
        let result = receiver.process_packet(&pkt2);
        assert_eq!(result.len(), 1);
        let frame = &result[0];
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
        // ~170KB keyframe: 143 full fragments + one short tail, the
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
        let mut completed = Vec::new();
        for (n, &i) in order.iter().enumerate() {
            let pkt = make_packet(7, i, total, true, &payloads[i as usize]);
            let res = receiver.process_packet(&pkt);
            if n + 1 < order.len() {
                assert!(res.is_empty(), "completed early at packet {}", n);
            } else {
                completed = res;
            }
        }
        assert_eq!(completed.len(), 1, "frame must complete on last fragment");
        let frame = &completed[0];
        assert_eq!(frame.data.len(), expected.len());
        assert_eq!(frame.data, expected);
        assert!(frame.is_keyframe);
    }

    #[test]
    fn duplicate_fragments_do_not_double_count_or_corrupt() {
        // NACK retransmits deliver the same fragment twice; the frame
        // must complete exactly when all *distinct* fragments arrived.
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");
        let pkt0 = make_packet(1, 0, 3, false, b"aaaa");
        let pkt1 = make_packet(1, 1, 3, false, b"bbbb");
        let pkt2 = make_packet(1, 2, 3, false, b"cccc");

        assert!(receiver.process_packet(&pkt0).is_empty());
        assert!(receiver.process_packet(&pkt0).is_empty()); // retransmit
        assert!(receiver.process_packet(&pkt1).is_empty());
        assert!(receiver.process_packet(&pkt1).is_empty()); // retransmit
        let out = receiver.process_packet(&pkt2);
        assert_eq!(out.len(), 1, "complete on 3rd distinct");
        assert_eq!(out[0].data, b"aaaabbbbcccc");
    }

    #[test]
    fn out_of_range_index_is_ignored() {
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");
        // index 5 in a 3-packet frame: malformed, must not panic and
        // must not pollute the assembly.
        let bogus = make_packet(1, 5, 3, false, b"evil");
        assert!(receiver.process_packet(&bogus).is_empty());

        let pkt0 = make_packet(1, 0, 3, false, b"aaaa");
        let pkt1 = make_packet(1, 1, 3, false, b"bbbb");
        let pkt2 = make_packet(1, 2, 3, false, b"cccc");
        assert!(receiver.process_packet(&pkt0).is_empty());
        assert!(receiver.process_packet(&pkt1).is_empty());
        let out = receiver.process_packet(&pkt2);
        assert_eq!(out.len(), 1, "valid fragments complete");
        assert_eq!(out[0].data, b"aaaabbbbcccc");
    }

    #[test]
    fn zero_total_packets_is_rejected() {
        let mut receiver = VideoReceiver::new();
        let bogus = make_packet(1, 0, 0, false, b"evil");
        assert!(receiver.process_packet(&bogus).is_empty());
    }

    #[test]
    fn oversized_total_packets_is_rejected() {
        // A crafted packet claiming an enormous fragment count must be
        // dropped before it allocates total_packets * UDP_MAX_PAYLOAD
        // (~75 MB at u16::MAX) and must not create an assembly entry.
        let mut receiver = VideoReceiver::new();
        let evil = make_packet(1, 0, u16::MAX, true, b"boom");
        assert!(receiver.process_packet(&evil).is_empty());
        let over = make_packet(2, 0, MAX_PACKETS_PER_FRAME + 1, true, b"boom");
        assert!(receiver.process_packet(&over).is_empty());
        assert!(receiver
            .senders
            .values()
            .all(|s| s.frames_in_progress.is_empty()));

        // A frame right at the cap is still accepted (creates an assembly).
        let ok = make_packet(3, 0, MAX_PACKETS_PER_FRAME, false, b"ok");
        assert!(receiver.process_packet(&ok).is_empty());
        assert_eq!(
            receiver
                .senders
                .values()
                .map(|s| s.frames_in_progress.len())
                .sum::<usize>(),
            1
        );
    }

    #[test]
    fn concurrent_partial_frames_are_bounded() {
        // Spraying distinct frame_ids that never complete must not grow
        // frames_in_progress without bound.
        let mut receiver = VideoReceiver::new();
        for fid in 0..(MAX_FRAMES_IN_PROGRESS as u32 + 50) {
            // total=2 so the frame never completes on a single fragment
            let pkt = make_packet(fid, 0, 2, false, b"x");
            assert!(receiver.process_packet(&pkt).is_empty());
        }
        assert!(receiver
            .senders
            .values()
            .all(|s| s.frames_in_progress.len() <= MAX_FRAMES_IN_PROGRESS));
    }

    #[test]
    fn sender_count_is_bounded() {
        let mut receiver = VideoReceiver::new();
        for i in 0..(MAX_SENDERS + 4) {
            let name = format!("streamer{}", i);
            let pkt = make_packet_from(&name, 0, 0, 2, false, b"x");
            receiver.process_packet(&pkt);
        }
        assert!(receiver.senders.len() <= MAX_SENDERS);
    }

    #[test]
    fn fec_groups_are_bounded_and_deduped() {
        let mut receiver = VideoReceiver::new();
        // Incomplete 4-packet frame to attach FEC groups to.
        assert!(receiver.process_packet(&make_packet(1, 0, 4, true, b"x")).is_empty());
        let xor = [0u8; UDP_MAX_PAYLOAD];
        for _ in 0..(MAX_FEC_GROUPS + 100) {
            let fec = UdpFecPacket::new("streamer1", 1, 0, 2, 0, &xor);
            let _ = receiver.process_fec_packet(&fec);
        }
        let groups = receiver
            .senders
            .values()
            .flat_map(|s| s.frames_in_progress.values())
            .map(|a| a.fec_groups.len())
            .max()
            .unwrap_or(0);
        assert_eq!(groups, 1, "duplicate group_start must dedupe to one entry");
    }

    #[test]
    fn fec_recovers_short_tail_fragment() {
        // The lost fragment is the short tail — recovery must restore
        // its exact (non-padded) length, and compaction must place it
        // flush after the full-size fragments.
        let mut receiver = VideoReceiver::new();
        seed_keyframe(&mut receiver, "streamer1");
        let data0: Vec<u8> = vec![0xAA; UDP_MAX_PAYLOAD];
        let data1: Vec<u8> = vec![0xBB; UDP_MAX_PAYLOAD];
        let data2: Vec<u8> = (0..200u16).map(|j| j as u8).collect();

        let mut xor_payload = [0u8; UDP_MAX_PAYLOAD];
        for (j, &b) in data0.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data1.iter().enumerate() { xor_payload[j] ^= b; }
        for (j, &b) in data2.iter().enumerate() { xor_payload[j] ^= b; }
        let size_xor =
            (data0.len() as u16) ^ (data1.len() as u16) ^ (data2.len() as u16);

        assert!(receiver.process_packet(&make_packet(9, 0, 3, true, &data0)).is_empty());
        assert!(receiver.process_packet(&make_packet(9, 1, 3, true, &data1)).is_empty());
        // tail (index 2) lost — FEC packet completes the frame
        let fec = UdpFecPacket::new("streamer1", 9, 0, 3, size_xor, &xor_payload);
        let out = receiver.process_fec_packet(&fec);
        assert_eq!(out.len(), 1, "FEC recovery");
        let frame = &out[0];
        assert_eq!(frame.data.len(), UDP_MAX_PAYLOAD * 2 + 200);
        assert_eq!(&frame.data[..UDP_MAX_PAYLOAD], data0.as_slice());
        assert_eq!(&frame.data[UDP_MAX_PAYLOAD..UDP_MAX_PAYLOAD * 2], data1.as_slice());
        assert_eq!(&frame.data[UDP_MAX_PAYLOAD * 2..], data2.as_slice());
    }
}
