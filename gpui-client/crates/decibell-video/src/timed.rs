//! Timed streams: a file being played instead of a live stream being watched.
//!
//! A live stream's pictures are published the moment they are decoded and its queue sheds what
//! the decoder cannot keep up with. A file is the opposite on both counts: its pictures are due
//! at their presentation times on the player's clock (the audio clock), and nothing may be
//! shed, the producer waits instead. This module is what a player (`decibell-media`) adds to a
//! [`Stream`] to get that, on the same decoders, slots and presenter as live streams:
//!
//! * [`Stream::set_timing`] gives the stream a [`Timing`]. Its decoder thread then asks
//!   [`Timing::admit`] with each decoded picture's presentation time before copying it into a
//!   slot: the call **blocks until the picture is due** (the player's condition variable, timed
//!   against its clock: one wake per picture, no polling) and says whether to show it or to
//!   drop it (decoded only to reach a seek target, or nothing on screen shows the video). So
//!   the thread decodes exactly one picture ahead and sleeps; while the player is paused it is
//!   parked inside `admit` with the next picture in hand.
//! * [`Stream::push_timed`] queues one packet or says the queue is full ([`TIMED_DEPTH`]
//!   packets): the producer keeps the packet and offers it again after the next `admit`.
//!   Nothing is shed, no keyframe is asked of anybody: a broken chain is reported to the
//!   producer ([`TimedPush::NeedKeyframe`], [`Timing::keyframe_needed`]), which seeks.
//! * Packets carry an **epoch**. A packet of a new epoch (the first after a seek) must be a
//!   keyframe; the decoder is flushed before it (kept open: a seek costs no decoder open) and
//!   `admit` is told the epoch, so pictures of the old position can be told from the new.
//! * A timed stream's decoder reorders (B-frames); an empty packet ([`TimedFrame::end`])
//!   drains the pictures it still holds at the end of the file.
//! * The hub's callbacks never hear of a timed stream: its events go to [`Timing::event`], it
//!   does not stall, and no keyframe request or frame-sink interest reaches the core for it.
//!
//! Everything else is the live path, unchanged: which decoder (Vulkan Video, NVDEC / VA-API,
//! software), what happens when one fails, the slots, one present per published picture.

use std::sync::Arc;
use std::time::Instant;

use crate::codec::Codec;
use crate::hub::{Stream, StreamEvent};
use crate::packet::Packet;
use crate::queue::{Description, Item, TimedQueued};

/// Packets a timed stream's queue holds: the decoder thread has one picture in hand and this
/// much to go on with when it wakes.
pub const TIMED_DEPTH: usize = 3;

/// What becomes of a decoded picture of a timed stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Copy it into a slot and present it now.
    Show,
    /// Drop it.
    Skip,
}

/// A timed stream's clock and listener. Called on the stream's decoder thread.
pub trait Timing: Send + Sync + 'static {
    /// A picture with presentation time `pts` (microseconds, the packets' time base), decoded
    /// from packets of `epoch`, is ready. Returns when it is due. Must return (with
    /// [`Admit::Skip`]) once the player is closing or `epoch` is no longer current: the decoder
    /// thread cannot stop while it is in here.
    fn admit(&self, epoch: u32, pts: i64) -> Admit;
    /// No packet arrived for [`crate::STALLED_AFTER`]. True: the decoder thread leaves and its
    /// decoder is freed (the last picture stays on screen); the next packet must then be a
    /// keyframe. False: it goes on waiting.
    fn idle(&self) -> bool;
    /// The decoder lost its state (an error, a decoder that was given up): what is queued was
    /// dropped and the next packet must be a keyframe, i.e. the producer seeks to where it is.
    fn keyframe_needed(&self);
    fn event(&self, event: StreamEvent);
}

/// One packet of a file.
pub struct TimedFrame {
    pub codec: Codec,
    pub keyframe: bool,
    /// Presentation time in microseconds.
    pub timestamp: i64,
    pub data: Packet,
    /// The codec's configuration record (the container's extradata: avcC, hvcC, av1C, …), the
    /// same shared bytes with every packet.
    pub description: Option<Arc<[u8]>>,
    pub epoch: u32,
}

impl TimedFrame {
    /// The end of the file: the decoder gives out the pictures it holds for reordering.
    pub fn end(codec: Codec, epoch: u32) -> TimedFrame {
        TimedFrame {
            codec,
            keyframe: false,
            timestamp: 0,
            data: Packet::from(Vec::new()),
            description: None,
            epoch,
        }
    }
}

/// What [`Stream::push_timed`] did with a packet.
pub enum TimedPush {
    Queued,
    /// [`TIMED_DEPTH`] packets wait: offer it again after the next [`Timing::admit`].
    Full(TimedFrame),
    /// The decoder starts on a keyframe and this was not one: the packet was dropped.
    NeedKeyframe,
    /// Nothing displays the stream (no [`crate::Viewer`]) or it was removed: dropped.
    Closed,
}

impl Stream {
    /// Makes this a timed stream (see the module docs), or a live one again with `None`. Set
    /// it before the first viewer and the first packet.
    pub fn set_timing(&self, timing: Option<Arc<dyn Timing>>) {
        self.shared.target.set_reorder(timing.is_some());
        *self.shared.timing.write() = timing;
    }

    /// Packets waiting for the decoder thread.
    pub fn queued(&self) -> usize {
        self.shared.queue.len()
    }

    /// Queues one packet of a timed stream for its decoder thread (started here if there is
    /// none). Never blocks, never sheds.
    pub fn push_timed(&self, frame: TimedFrame) -> TimedPush {
        let stream = &self.shared;
        if stream.viewer_count() == 0 {
            return TimedPush::Closed;
        }
        if stream.queue.len() >= TIMED_DEPTH {
            return TimedPush::Full(frame);
        }
        stream
            .target
            .counters
            .received
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let queued = stream.queue.push_timed(Item {
            packet: frame.data,
            codec: frame.codec,
            keyframe: frame.keyframe,
            timestamp: frame.timestamp,
            description: frame.description.map(Description::Shared),
            arrived: Instant::now(),
            epoch: frame.epoch,
        });
        match queued {
            TimedQueued::Queued { start_consumer } => {
                if start_consumer {
                    self.hub.clone().start_decoder(stream.clone());
                }
                TimedPush::Queued
            }
            TimedQueued::NeedKeyframe => TimedPush::NeedKeyframe,
            TimedQueued::Stopped => TimedPush::Closed,
        }
    }

    /// Drops what is queued; the next packet must be a keyframe (a seek: follow it with packets
    /// of a new epoch).
    pub fn reset_timed(&self) {
        self.shared.queue.gate();
    }
}
