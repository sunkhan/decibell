//! The video receive path from a media datagram to the event sink:
//! [`VideoRecvPath::on_datagram`] takes one inner datagram and hands every
//! frame it completes, reassembled, decrypted and stripped of its in-band
//! description, to `deliver` (in the thread: `events::send_stream_frame`).
//! `run_video_recv_thread` (mod.rs) owns the socket, the media pings and the
//! NACK / PLI maintenance; tests feed datagrams straight in.
//!
//! Per fragment the bytes are copied once, from the datagram into the frame's
//! reassembly buffer (read in place as a `VideoFragment`, never copied into a
//! packet struct first). With the frame pool on (`frame_pool.rs`) that buffer
//! comes from the pool, a sealed frame is decrypted where it lies, and the
//! `FrameBuf` the sink gets is that buffer: a steady stream allocates nothing
//! here. With the pool off (the Electron addon) the frame takes today's
//! path: a fresh buffer per frame, an allocating decrypt.

use std::time::Instant;

use super::frame_crypto::{self, SharedKeyRing};
use super::frame_pool::FramePool;
use super::packet::PACKET_TYPE_PING;
use super::video_packet::{
    FecFragment, UdpKeyframeRequest, VideoFragment, PACKET_TYPE_FEC,
    PACKET_TYPE_KEYFRAME_REQUEST, PACKET_TYPE_VIDEO, WIRE_DESCRIPTION_MAGIC,
};
use super::video_receiver::{ReassembledFrame, VideoReceiver};
use crate::events::{self, StreamFrame};

/// What a datagram asks of the receive loop beyond frames.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Control {
    None,
    /// A watcher asked us (the streamer) for a keyframe.
    KeyframeRequested,
    /// Our media PING came back: the relay has bound this endpoint.
    PingEcho,
}

pub(crate) struct VideoRecvPath {
    receiver: VideoReceiver,
    ring: Option<SharedKeyRing>,
    accepted_video_type: u8,
    /// Frames the last datagram completed; reused, so delivering doesn't
    /// allocate.
    ready: Vec<ReassembledFrame>,
    frames_received: u64,
    /// Presentation timestamps: microseconds on a monotonic clock from the
    /// engine's start. The old `frame_id * 33_333` hardcoded a 30fps clock:
    /// 60fps streams advanced at half real rate, and a stream restart
    /// (frame_id resets to 0) made the timestamp jump backwards. Frames are
    /// painted on arrival, so wall-time timestamps are exactly right.
    ts_epoch: Instant,
}

impl VideoRecvPath {
    pub(crate) fn new(ring: Option<SharedKeyRing>, ts_epoch: Instant, pool: &'static FramePool) -> Self {
        // Strict: an encrypted channel accepts only VIDEO_SEALED chunks; the
        // plain (P2P) path only VIDEO. FEC / PLI / NACK are shared.
        let accepted_video_type = if ring.is_some() {
            frame_crypto::PACKET_TYPE_VIDEO_SEALED
        } else {
            PACKET_TYPE_VIDEO
        };
        VideoRecvPath {
            receiver: VideoReceiver::with_pool(pool),
            ring,
            accepted_video_type,
            ready: Vec::new(),
            frames_received: 0,
            ts_epoch,
        }
    }

    /// For the loop's NACK / PLI maintenance.
    pub(crate) fn receiver(&mut self) -> &mut VideoReceiver {
        &mut self.receiver
    }

    /// Handle one inner datagram (`dgram` non-empty), delivering every frame
    /// it completes.
    pub(crate) fn on_datagram(&mut self, dgram: &[u8], deliver: &mut impl FnMut(StreamFrame)) -> Control {
        let Some(&packet_type) = dgram.first() else {
            return Control::None;
        };
        if packet_type == self.accepted_video_type {
            if let Some(frag) = VideoFragment::parse(dgram) {
                // The receiver keys every piece of state by sender, so
                // concurrent streams reassemble independently and frames
                // come back already in delivery order per streamer.
                self.receiver.process_fragment(&frag, &mut self.ready);
                self.deliver_ready(false, deliver);
            }
        } else if packet_type == PACKET_TYPE_FEC {
            // Only a full-size FEC datagram parses (see FecFragment).
            if let Some(fec) = FecFragment::parse(dgram) {
                self.receiver.process_fec(&fec, &mut self.ready);
                self.deliver_ready(true, deliver);
            }
        } else if packet_type == PACKET_TYPE_KEYFRAME_REQUEST
            && dgram.len() >= std::mem::size_of::<UdpKeyframeRequest>()
        {
            return Control::KeyframeRequested;
        } else if packet_type == PACKET_TYPE_PING {
            return Control::PingEcho;
        }
        Control::None
    }

    fn deliver_ready(&mut self, via_fec: bool, deliver: &mut impl FnMut(StreamFrame)) {
        // Out while `emit` borrows self; the capacity comes back.
        let mut ready = std::mem::take(&mut self.ready);
        for frame in ready.drain(..) {
            self.frames_received += 1;
            if via_fec {
                log::debug!(
                    "[video-recv] Frame {} from '{}' completed via FEC: {} bytes, keyframe={}",
                    frame.frame_id,
                    frame.streamer_id,
                    frame.data.len(),
                    frame.is_keyframe
                );
            } else if frame.is_keyframe || self.frames_received % 300 == 1 {
                log::debug!(
                    "[video-recv] Frame {} from '{}': {} bytes, keyframe={} (total={})",
                    frame.frame_id,
                    frame.streamer_id,
                    frame.data.len(),
                    frame.is_keyframe,
                    self.frames_received
                );
            }
            self.emit(frame, deliver);
        }
        self.ready = ready;
    }

    fn emit(&self, mut frame: ReassembledFrame, deliver: &mut impl FnMut(StreamFrame)) {
        // Only forward frames for streams we're actually watching. See
        // WATCHED_STREAMS: defense against spoofed/over-relayed senders, and
        // it drops the IPC/renderer work for frames nobody subscribed to.
        if !super::is_watched(&frame.streamer_id) {
            return;
        }
        // Watched but not on screen (no UI subscriber — e.g. the focused
        // view hides the grid's other watched streams): skip the decrypt.
        // send_stream_frame re-checks; the player asks for a keyframe when
        // it subscribes again.
        if !events::is_stream_frame_sink(&frame.streamer_id) {
            return;
        }
        // Encrypted channel: the reassembled bytes are one sealed frame —
        // open it under the streamer's epoch key (drop it if that fails).
        if let Some(r) = &self.ring {
            if !frame_crypto::open_video_frame_buf(
                &r.load(),
                &frame.streamer_id,
                frame.frame_id,
                frame.is_keyframe,
                frame.codec,
                &mut frame.data,
            ) {
                log::debug!(
                    "[video-recv] frame {} from '{}' did not open (epoch/key)",
                    frame.frame_id,
                    frame.streamer_id
                );
                return;
            }
        }
        strip_keyframe_description(&mut frame);
        deliver(StreamFrame {
            key: frame.streamer_id,
            codec: frame.codec,
            keyframe: frame.is_keyframe,
            timestamp: self.ts_epoch.elapsed().as_micros() as i64,
            data: frame.data,
            description: frame.description,
            discontinuity: false,
        });
    }
}

/// HEVC/AV1 keyframes carry the sender's decoder description in-band:
/// `[MAGIC][u32 BE: desc_len][desc][bitstream]`. Strip it so `data` is only
/// the bitstream and `description` the WebCodecs decoder configuration
/// record. H.264 keyframes don't use this (SPS/PPS travel inline). The
/// description is the one allocation here (small, keyframes only); the
/// prefix is dropped without moving the bitstream when the buffer is pooled.
fn strip_keyframe_description(frame: &mut ReassembledFrame) {
    let magic = &WIRE_DESCRIPTION_MAGIC;
    if frame.is_keyframe
        && (frame.codec == 3 || frame.codec == 4)
        && frame.data.len() >= magic.len() + 4
        && frame.data[..magic.len()] == magic[..]
    {
        let len_off = magic.len();
        let len = u32::from_be_bytes([
            frame.data[len_off],
            frame.data[len_off + 1],
            frame.data[len_off + 2],
            frame.data[len_off + 3],
        ]) as usize;
        let payload_off = len_off + 4;
        if len > 0 && len < 1024 && frame.data.len() >= payload_off + len {
            frame.description = Some(frame.data[payload_off..payload_off + len].to_vec());
            frame.data.advance(payload_off + len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::frame_crypto::{EpochKeys, KeyRing, PACKET_TYPE_VIDEO_SEALED};
    use crate::media::frame_pool::PoolStats;
    use crate::media::video_packet::{UdpFecPacket, UdpVideoPacket, FEC_GROUP_SIZE, UDP_MAX_PAYLOAD};
    use crate::test_alloc::allocations_on_this_thread;
    use arc_swap::ArcSwap;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    const GOP: usize = 60;
    const FRAMES: usize = 300;
    /// Two GOPs: every frame size of the stream has been seen.
    const WARM_UP: usize = 120;

    fn bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect()
    }

    /// A 60-frame GOP: a 150 KB keyframe, then deltas growing 8–37.5 KB.
    fn stream() -> Vec<(bool, Vec<u8>)> {
        (0..FRAMES)
            .map(|i| {
                let keyframe = i % GOP == 0;
                let len = if keyframe { 150_000 } else { 8_000 + (i % GOP) * 500 };
                (keyframe, bytes(i as u64, len))
            })
            .collect()
    }

    /// The datagrams a streamer `key` sends for `frames`, chunked as
    /// `VideoSender::send` does (sealed first in an encrypted channel), and
    /// returned with each frame's first datagram index. With `fec`, one XOR
    /// packet follows each group of 5 and every 7th frame loses the second
    /// packet of each group, which FEC rebuilds.
    fn datagrams(
        key: &str,
        frames: &[(bool, Vec<u8>)],
        codec: u8,
        seal: Option<&KeyRing>,
        fec: bool,
    ) -> (Vec<Vec<u8>>, Vec<usize>) {
        let mut out = Vec::new();
        let mut starts = Vec::new();
        for (fid, (keyframe, data)) in frames.iter().enumerate() {
            starts.push(out.len());
            let fid = fid as u32;
            let (wire, packet_type) = match seal {
                Some(r) => (
                    frame_crypto::seal_video_frame(r, 77, fid, *keyframe, codec, data).unwrap(),
                    PACKET_TYPE_VIDEO_SEALED,
                ),
                None => (data.clone(), PACKET_TYPE_VIDEO),
            };
            let chunks: Vec<&[u8]> = wire.chunks(UDP_MAX_PAYLOAD).collect();
            let total = chunks.len() as u16;
            for (g, group) in chunks.chunks(FEC_GROUP_SIZE as usize).enumerate() {
                let start = g * FEC_GROUP_SIZE as usize;
                let mut xor = [0u8; UDP_MAX_PAYLOAD];
                let mut size_xor = 0u16;
                for (k, chunk) in group.iter().enumerate() {
                    for (j, b) in chunk.iter().enumerate() {
                        xor[j] ^= b;
                    }
                    size_xor ^= chunk.len() as u16;
                    if fec && fid % 7 == 3 && k == 1 {
                        continue; // lost
                    }
                    let mut p =
                        UdpVideoPacket::new_with_codec(key, fid, (start + k) as u16, total, *keyframe, codec, chunk);
                    p.packet_type = packet_type;
                    out.push(p.to_bytes());
                }
                if fec {
                    out.push(UdpFecPacket::new(key, fid, start as u16, group.len() as u16, size_xor, &xor).to_bytes());
                }
            }
        }
        (out, starts)
    }

    /// Rings for a channel where `key` streams and "me" watches.
    fn rings(key: &str) -> (KeyRing, SharedKeyRing) {
        let keys = EpochKeys::derive(1, &[9u8; 32], &[key.to_string(), "me".to_string()]);
        (
            KeyRing::empty(key).rotated(keys.clone()),
            Arc::new(ArcSwap::from_pointee(KeyRing::empty("me").rotated(keys))),
        )
    }

    /// Watch `key` and put it on screen. (Both sets' writers serialise
    /// themselves: concurrent tests can't lose each other's keys.)
    fn watch(key: &str) {
        events::testing::install_recording_sink();
        crate::media::watch_stream_add(key);
        events::set_stream_frame_sink(key, true);
    }

    /// One delivered frame, as the test sees it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Seen {
        len: usize,
        keyframe: bool,
        digest: [u8; 32],
    }

    struct Run {
        seen: Vec<Seen>,
        /// Allocations on this thread from the first datagram of frame
        /// WARM_UP to the end.
        steady_allocations: u64,
        all_allocations: u64,
    }

    /// Feed `dgrams` through the real path. The consumer digests each frame,
    /// then drops it through `events::send_stream_frame` (the recording sink
    /// drops stream frames, returning a pooled buffer).
    fn run(dgrams: &[Vec<u8>], starts: &[usize], ring: Option<SharedKeyRing>, pool: &'static FramePool) -> Run {
        let mut seen = Vec::with_capacity(FRAMES);
        let mut path = VideoRecvPath::new(ring, Instant::now(), pool);
        let mut deliver = |f: StreamFrame| {
            let digest: [u8; 32] = Sha256::digest(&f.data[..]).as_slice().try_into().unwrap();
            seen.push(Seen { len: f.data.len(), keyframe: f.keyframe, digest });
            events::send_stream_frame(f);
        };
        let first = allocations_on_this_thread();
        let mut warm = first;
        for (i, d) in dgrams.iter().enumerate() {
            if i == starts[WARM_UP] {
                warm = allocations_on_this_thread();
            }
            // The loop's NACK / PLI maintenance, every 4th frame, between
            // frames (a frame in flight on a slow test machine could
            // otherwise look lost and be NACKed).
            if starts.binary_search(&i).is_ok_and(|k| k % 4 == 0) {
                let (nacks, plis) = path.receiver().check_missing();
                assert!(nacks.is_empty() && plis.is_empty());
                path.receiver().cleanup_stale();
            }
            assert_eq!(path.on_datagram(d, &mut deliver), Control::None);
        }
        let end = allocations_on_this_thread();
        drop(deliver);
        Run { seen, steady_allocations: end - warm, all_allocations: end - first }
    }

    fn expected(frames: &[(bool, Vec<u8>)]) -> Vec<Seen> {
        frames
            .iter()
            .map(|(keyframe, data)| Seen {
                len: data.len(),
                keyframe: *keyframe,
                digest: Sha256::digest(data).as_slice().try_into().unwrap(),
            })
            .collect()
    }

    /// Design §6 item 6: with the pool on, a watched stream in steady state
    /// allocates nothing between the datagram and the consumer, plain or
    /// sealed, with or without FEC (and FEC recovering losses); with the pool
    /// off the same datagrams give byte-identical frames.
    #[test]
    fn steady_state_allocates_nothing_with_the_pool() {
        const KEY: &str = "4242";
        watch(KEY);
        static OFF: FramePool = FramePool::disabled();
        let frames = stream();
        let want = expected(&frames);
        let (sender_ring, watcher_ring) = rings(KEY);
        for (label, sealed, fec) in [("plain", false, false), ("sealed", true, false), ("plain+fec", false, true), ("sealed+fec", true, true)] {
            let (dgrams, starts) = datagrams(KEY, &frames, 1, sealed.then_some(&sender_ring), fec);
            let ring = || sealed.then(|| watcher_ring.clone());
            let pool: &'static FramePool = Box::leak(Box::new(FramePool::new(16, 4 << 20)));

            let on = run(&dgrams, &starts, ring(), pool);
            let off = run(&dgrams, &starts, ring(), &OFF);
            let stats = pool.stats();
            let steady = (FRAMES - WARM_UP) as f64;
            println!(
                "[{label}] pool on: {} allocations in all, {} over the last {} frames ({:.2}/frame); pool off: {:.2}/frame; {:?}",
                on.all_allocations,
                on.steady_allocations,
                FRAMES - WARM_UP,
                on.steady_allocations as f64 / steady,
                off.steady_allocations as f64 / steady,
                stats
            );
            assert_eq!(on.seen, want, "[{label}] pool on delivers the stream byte for byte");
            assert_eq!(off.seen, want, "[{label}] pool off delivers the same bytes");
            assert_eq!(on.steady_allocations, 0, "[{label}] steady state must not allocate");
            assert!(stats.misses <= 4, "[{label}] the pool is warm after the first GOP: {stats:?}");
            assert_eq!(stats.takes, stats.hits + stats.misses);
            assert_eq!(stats.returned + stats.dropped, stats.takes, "every buffer came back: {stats:?}");
            assert!(off.steady_allocations > 0);
        }
    }

    /// HEVC / AV1 keyframes: the in-band description is split off the same
    /// way pooled or not; a pooled frame keeps its bytes where they were, an
    /// unpooled one starts at its allocation's front (so `into_vec` is free).
    #[test]
    fn keyframe_description_is_stripped_alike() {
        const KEY: &str = "4244";
        watch(KEY);
        static OFF: FramePool = FramePool::disabled();
        let desc = bytes(1, 90);
        let bitstream = bytes(2, 40_000);
        let mut wire = WIRE_DESCRIPTION_MAGIC.to_vec();
        wire.extend_from_slice(&(desc.len() as u32).to_be_bytes());
        wire.extend_from_slice(&desc);
        wire.extend_from_slice(&bitstream);
        let frames = vec![(true, wire)];
        let (sender_ring, watcher_ring) = rings(KEY);
        for sealed in [false, true] {
            let (dgrams, _) = datagrams(KEY, &frames, 3, sealed.then_some(&sender_ring), false);
            let pool: &'static FramePool = Box::leak(Box::new(FramePool::new(4, 1 << 20)));
            for p in [pool, &OFF] {
                let mut path = VideoRecvPath::new(sealed.then(|| watcher_ring.clone()), Instant::now(), p);
                let mut got = Vec::new();
                for d in &dgrams {
                    path.on_datagram(d, &mut |f: StreamFrame| got.push(f));
                }
                assert_eq!(got.len(), 1);
                let f = got.pop().unwrap();
                assert_eq!(f.description.as_deref(), Some(&desc[..]));
                assert_eq!(f.data, bitstream);
                assert_eq!(f.data.is_pooled(), p.is_enabled());
                let ptr = f.data.as_ptr();
                if !p.is_enabled() {
                    let v = f.data.into_vec();
                    assert_eq!(v.as_ptr(), ptr, "unpooled: into_vec is the same allocation");
                }
            }
        }
    }

    /// The Electron addon never enables the pool: what reaches the sink is a
    /// plain `Vec` (the reassembly buffer, or the decrypt's output), and
    /// `into_vec` hands that very allocation on (napi_sink.rs passes it to V8).
    #[test]
    fn pool_off_frames_reach_the_sink_as_the_same_vec() {
        const KEY: &str = "4245";
        watch(KEY);
        static OFF: FramePool = FramePool::disabled();
        let frames = vec![(true, bytes(3, 30_000)), (false, bytes(4, 5_000))];
        let (sender_ring, watcher_ring) = rings(KEY);
        for sealed in [false, true] {
            let (dgrams, _) = datagrams(KEY, &frames, 1, sealed.then_some(&sender_ring), false);
            let mut path = VideoRecvPath::new(sealed.then(|| watcher_ring.clone()), Instant::now(), &OFF);
            let mut got: Vec<StreamFrame> = Vec::new();
            for d in &dgrams {
                path.on_datagram(d, &mut |f: StreamFrame| got.push(f));
            }
            assert_eq!(got.len(), 2);
            for (f, (_, want)) in got.into_iter().zip(&frames) {
                assert!(!f.data.is_pooled());
                let ptr = f.data.as_ptr();
                let v = f.data.into_vec();
                assert_eq!(v.as_ptr(), ptr);
                assert_eq!(&v, want);
            }
        }
        assert_eq!(OFF.stats(), PoolStats::default());
    }

    /// Unwatched or off-screen streams are dropped before the decrypt;
    /// control datagrams reach the loop.
    #[test]
    fn unwatched_frames_and_control_datagrams() {
        static OFF: FramePool = FramePool::disabled();
        events::testing::install_recording_sink();
        let frames = vec![(true, bytes(5, 2_000))];
        let (dgrams, _) = datagrams("4246", &frames, 1, None, false);
        let mut path = VideoRecvPath::new(None, Instant::now(), &OFF);
        let mut n = 0;
        for d in &dgrams {
            path.on_datagram(d, &mut |_| n += 1);
        }
        assert_eq!(n, 0, "not watched");
        let pli = video_packet_keyframe_request();
        assert_eq!(path.on_datagram(&pli, &mut |_| {}), Control::KeyframeRequested);
        assert_eq!(path.on_datagram(&pli[..pli.len() - 1], &mut |_| {}), Control::None);
        let ping = crate::media::packet::UdpAudioPacket::new_ping("me", 1).to_bytes();
        assert_eq!(path.on_datagram(&ping, &mut |_| {}), Control::PingEcho);
        assert_eq!(path.on_datagram(&[], &mut |_| {}), Control::None);
    }

    fn video_packet_keyframe_request() -> Vec<u8> {
        UdpKeyframeRequest::new("me", "4246").to_bytes()
    }
}
