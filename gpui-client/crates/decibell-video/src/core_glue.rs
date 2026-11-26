//! The core's side of the hub, behind the `core` feature: its `StreamFrame` and pooled
//! `FrameBuf` in, and the pool sized for what the hub holds.
//!
//! Without the feature the crate does not know `decibell-core` (its tests and its Windows
//! build stay small); the app turns it on.

use decibell_core::events::StreamFrame;
use decibell_core::media::frame_pool::{self, FrameBuf};

use crate::hub::{EncodedFrame, VideoHub};
use crate::packet::{Packet, PacketBytes};

/// A core frame buffer as a packet's owner: dropping it returns the allocation to the core's
/// pool.
struct CoreBuf(FrameBuf);

// Safety: a `FrameBuf`'s bytes live in its heap allocation, which does not move with the value.
unsafe impl PacketBytes for CoreBuf {
    fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// 0: `FrameBuf` does not promise (or expose) zeroed bytes after the frame, so the packet
    /// is copied once into a padded buffer of ours (`decibell_video::packet_copies()` counts
    /// them). **The one line to change** when the core's pool reserves and zeroes
    /// `decibell_video::PADDING` bytes after each frame: return that guarantee here, and
    /// frames then reach FFmpeg in the core's own buffer.
    fn zero_padding(&self) -> usize {
        0
    }
}

impl From<FrameBuf> for Packet {
    fn from(buffer: FrameBuf) -> Packet {
        Packet::new(CoreBuf(buffer))
    }
}

impl VideoHub {
    /// The body of the app's `EventSink::stream_frame` / `StreamFrameSink::frame`: called on
    /// the core's `decibell-video-recv` thread (or an encoder thread, for our own preview),
    /// never blocks.
    pub fn push_frame(&self, frame: StreamFrame) {
        self.push(
            &frame.key,
            EncodedFrame {
                codec: frame.codec,
                keyframe: frame.keyframe,
                timestamp: frame.timestamp,
                data: Packet::from(frame.data),
                description: frame.description,
                discontinuity: frame.discontinuity,
            },
        );
    }
}

/// Streams the pool is sized for (more work, with buffers allocated as needed).
pub const POOLED_STREAMS: usize = 8;
/// Buffers the core's pool keeps: per stream the hub's three queued frames, the one in the
/// decoder and the one the receive path is filling.
pub const POOL_BUFFERS: usize = POOLED_STREAMS * 5;
/// The largest allocation the pool keeps between frames. A 2160p keyframe of the streamer's
/// encoder settings is under 100 KB (constant bit rate, a two-frame buffer: the largest frame of
/// the 30 Mbit/s test streams is 94 KB); 1 MiB leaves room for ten times that, and 40 retained
/// buffers can then never hold more than 40 MiB, in practice about 2 MiB.
pub const POOL_BUFFER_BYTES: usize = 1 << 20;

/// Turns the core's frame pool on with the hub's numbers. Call once at boot, before the first
/// stream is watched; returns what `frame_pool::enable` returns.
pub fn enable_frame_pool() -> bool {
    frame_pool::enable(POOL_BUFFERS, POOL_BUFFER_BYTES)
}
