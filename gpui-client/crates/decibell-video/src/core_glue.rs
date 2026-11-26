//! The core's side of the hub, behind the `core` feature: its `StreamFrame` and pooled
//! `FrameBuf` in, and the pool sized for what the hub holds.
//!
//! Without the feature the crate does not know `decibell-core` (its tests and its Windows
//! build stay small); the app turns it on.

use decibell_core::events::StreamFrame;
use decibell_core::media::frame_pool::{self, FrameBuf};

use crate::hub::{EncodedFrame, VideoHub};
use crate::packet::{Packet, PacketBytes};

/// What `FrameBuf` says about the bytes after a frame, for a core that does not say.
///
/// The core is gaining a guarantee: its pool allocates `FrameBuf::PADDING` (64) bytes more than
/// the frame and keeps them zeroed, and `FrameBuf::has_padding()` tells whether a given buffer
/// has them (a pooled one does; one made from a plain `Vec` may not). This crate builds against
/// cores from before and after that change. Rust resolves an inherent item before a trait's:
/// with the new core `FrameBuf::PADDING` and `buffer.has_padding()` below are the core's own,
/// with the old one they are these defaults, which say "no padding".
#[allow(dead_code)]
trait PaddingOfOlderCores {
    const PADDING: usize = 0;
    fn has_padding(&self) -> bool {
        false
    }
}

impl PaddingOfOlderCores for FrameBuf {}

/// What the hand-over needs of a core buffer. A trait so that both kinds, padded and not, are
/// tested whichever core is linked.
trait FrameBytes: Send + 'static {
    fn frame(&self) -> &[u8];
    /// How many readable, zeroed bytes follow the frame.
    fn zeroed_after(&self) -> usize;
}

impl FrameBytes for FrameBuf {
    fn frame(&self) -> &[u8] {
        self
    }

    #[allow(clippy::needless_borrow)]
    fn zeroed_after(&self) -> usize {
        if self.has_padding() {
            <FrameBuf>::PADDING
        } else {
            0
        }
    }
}

/// A core frame buffer as a packet's owner: dropping it returns the allocation to the core's
/// pool.
struct CoreBuf<B>(B);

// Safety: a `FrameBuf`'s bytes live in its heap allocation, which does not move with the value;
// `zeroed_after` is the core's own guarantee about what follows them.
unsafe impl<B: FrameBytes> PacketBytes for CoreBuf<B> {
    fn bytes(&self) -> &[u8] {
        self.0.frame()
    }

    /// With the padding FFmpeg needs ([`crate::PADDING`] zeroed bytes), the frame reaches the
    /// decoder in the core's own buffer: no copy. Without (an older core, or a buffer that did
    /// not come from the pool), `Packet::new` copies it once into a padded buffer of ours and
    /// returns this one to the pool at once (`decibell_video::packet_copies()` counts those).
    fn zero_padding(&self) -> usize {
        self.0.zeroed_after()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::PADDING;

    /// A stand-in for the core's buffer after its pool pads: spare zeroed capacity after the
    /// frame, and it says so.
    struct Padded(Vec<u8>);

    impl Padded {
        fn new(frame: &[u8]) -> Self {
            let mut bytes = Vec::with_capacity(frame.len() + PADDING);
            bytes.extend_from_slice(frame);
            bytes.resize(frame.len() + PADDING, 0);
            Self(bytes)
        }
    }

    impl FrameBytes for Padded {
        fn frame(&self) -> &[u8] {
            &self.0[..self.0.len() - PADDING]
        }
        fn zeroed_after(&self) -> usize {
            PADDING
        }
    }

    /// The core's buffer of today: nothing promised after the frame.
    struct Bare(Vec<u8>);

    impl FrameBytes for Bare {
        fn frame(&self) -> &[u8] {
            &self.0
        }
        fn zeroed_after(&self) -> usize {
            0
        }
    }

    fn padding_is_zero(packet: &Packet) -> bool {
        // Safety: every packet has PADDING readable bytes after its contents.
        let tail = unsafe {
            std::slice::from_raw_parts(packet.bytes().as_ptr().add(packet.len()), PADDING)
        };
        tail.iter().all(|&byte| byte == 0)
    }

    /// The rule `FrameBytes for FrameBuf` relies on to serve both cores: a type's own
    /// `PADDING` / `has_padding` are found before the fallback trait's.
    #[test]
    fn a_cores_own_padding_items_win_over_the_fallback() {
        struct NewCore;
        #[allow(dead_code)]
        impl NewCore {
            const PADDING: usize = 64;
            fn has_padding(&self) -> bool {
                true
            }
        }
        impl PaddingOfOlderCores for NewCore {}
        struct OldCore;
        impl PaddingOfOlderCores for OldCore {}
        assert_eq!((<NewCore>::PADDING, NewCore.has_padding()), (64, true));
        assert_eq!((<OldCore>::PADDING, OldCore.has_padding()), (0, false));
    }

    #[test]
    fn a_padded_core_buffer_reaches_the_decoder_where_it_is() {
        let buffer = Padded::new(&[7u8; 300]);
        let address = buffer.frame().as_ptr();
        let packet = Packet::new(CoreBuf(buffer));
        assert_eq!(packet.bytes().as_ptr(), address, "the frame was copied");
        assert_eq!(packet.bytes(), &[7u8; 300]);
        assert!(padding_is_zero(&packet));
    }

    #[test]
    fn an_unpadded_core_buffer_is_copied_once_into_a_padded_one() {
        let buffer = Bare(vec![9u8; 300]);
        let address = buffer.frame().as_ptr();
        let before = crate::packet::copies();
        let packet = Packet::new(CoreBuf(buffer));
        assert!(crate::packet::copies() > before);
        assert_ne!(packet.bytes().as_ptr(), address);
        assert_eq!(packet.bytes(), &[9u8; 300]);
        assert!(padding_is_zero(&packet));
    }

    /// The linked core's own buffer, pooled and not: whatever it promises, the packet is the
    /// frame followed by zeroed padding, and it is copied exactly when the core promises none.
    #[test]
    fn the_linked_cores_buffers_are_carried_as_they_promise() {
        static POOL: frame_pool::FramePool = frame_pool::FramePool::disabled();
        POOL.enable(4, 1 << 20);
        for buffer in [POOL.take_copy(&[5u8; 1000]), FrameBuf::from(vec![5u8; 1000])] {
            let promised = buffer.zeroed_after();
            let address = buffer.frame().as_ptr();
            let packet = Packet::from(buffer);
            assert_eq!(packet.bytes(), &[5u8; 1000]);
            assert!(padding_is_zero(&packet));
            assert_eq!(
                packet.bytes().as_ptr() == address,
                promised >= PADDING,
                "promised {promised} bytes of padding"
            );
        }
    }
}
