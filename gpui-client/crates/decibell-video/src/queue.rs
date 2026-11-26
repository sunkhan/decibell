//! The hand-over from the receive thread to a stream's decoder thread: about 100 ms of frames
//! deep, never fewer than three.
//!
//! A full queue means the decoder is not keeping up. The frame that does not fit is shed, and
//! with it the reference chain: everything up to the next keyframe is refused at the door and
//! the caller is told to ask the streamer for one (the renderer's discontinuity handling:
//! `StreamVideoPlayer.tsx`, "gate deltas on the next keyframe and ask for one now").
//!
//! "Full" is a matter of time, not of a count (a deliberate difference from the renderer, which
//! sheds at `decodeQueueSize > 3`): a frame is queued while the oldest frame waiting has waited
//! less than [`DEPTH`], or fewer than [`MIN_DEPTH`] wait. Three frames are 25 ms at 120 fps: a
//! decoder thread the scheduler holds back for that long (the viewer's CPU is busy with a game)
//! would lose everything up to the next keyframe, which is a visible freeze and a keyframe's
//! worth of bitrate. With 100 ms the frames wait instead, the decoder works the backlog off at
//! its own speed (a millisecond or two per frame; only the newest picture of the burst reaches
//! the screen) and the stream is back at its one-frame latency a few frames later. A decoder
//! that is slower than the stream for good still overflows, 100 ms behind at the most, and
//! sheds exactly as before. The queue measures the stream's rate by what it is: 100 ms of
//! frames are 12 at 120 fps, 6 at 60, and the three-frame floor for anything under 30.
//!
//! A decoder that has just been opened is given room to catch up: opening one (FFmpeg's device
//! context, the frames pool, five slots) can take tens of milliseconds with Vulkan Video and a
//! few hundred with the vendor decoders (NVDEC creates a CUDA context: 160 ms measured; the
//! NVDEC-backed VA driver takes 370 ms to initialise), many frame intervals at 120 fps, and
//! shedding then would cost every stream a second keyframe at its start. Until the decoder
//! has published a picture and found the queue empty, the queue holds up to [`CATCH_UP`]
//! frames (half a second at 120 fps; the decoder then works them off at its own speed, a
//! millisecond or two each).
//!
//! One mutex, held for a few moves of a small struct; nothing is allocated or freed under it.

use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::codec::Codec;
use crate::packet::Packet;

/// Frames that are always queued in the steady state, however long the oldest has waited.
pub(crate) const MIN_DEPTH: usize = 3;
/// In the steady state a frame is queued while the oldest one waiting has waited less than
/// this: the scheduling gap of the decoder thread that is ridden out instead of shed.
pub(crate) const DEPTH: Duration = Duration::from_millis(100);
/// Frames queued while a new decoder catches up, and the queue's fixed capacity (its slots are
/// inline: nothing is allocated whatever the depth).
pub(crate) const CATCH_UP: usize = 64;

/// One encoded frame waiting for the decoder.
pub(crate) struct Item {
    pub packet: Packet,
    pub codec: Codec,
    pub keyframe: bool,
    pub timestamp: i64,
    pub description: Option<Vec<u8>>,
    pub arrived: Instant,
}

struct State {
    items: [Option<Item>; CATCH_UP],
    head: usize,
    len: usize,
    /// Deltas are refused until a keyframe arrives.
    need_key: bool,
    /// The decoder is new and has not emptied the queue yet: the limit is [`CATCH_UP`].
    catching_up: bool,
    /// A decoder thread is consuming (or about to).
    consumer: bool,
    consumer_waiting: bool,
    /// The consumer must leave; frames are refused.
    stopped: bool,
}

impl State {
    /// Whether a frame arriving at `now` does not fit: see the module docs.
    fn is_full(&self, now: Instant) -> bool {
        if self.len >= CATCH_UP {
            return true;
        }
        if self.catching_up || self.len < MIN_DEPTH {
            return false;
        }
        self.items[self.head]
            .as_ref()
            .is_none_or(|oldest| now.saturating_duration_since(oldest.arrived) >= DEPTH)
    }
}

pub(crate) struct FrameQueue {
    state: Mutex<State>,
    ready: Condvar,
}

/// What became of a pushed frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Pushed {
    pub queued: bool,
    /// Frames lost by this push: the pushed one if it was refused, and queued ones a keyframe
    /// made moot.
    pub shed: u32,
    /// The chain broke now: ask the streamer for a keyframe.
    pub request_keyframe: bool,
    /// No decoder thread is consuming: the caller starts one (the queue already counts it).
    pub start_consumer: bool,
}

pub(crate) enum Popped {
    Item(Item),
    /// Nothing arrived in time. The consumer is deregistered: it must exit.
    Idle,
    /// The stream stopped. The consumer is deregistered: it must exit.
    Stopped,
}

impl FrameQueue {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                items: [const { None }; CATCH_UP],
                head: 0,
                len: 0,
                need_key: true,
                catching_up: true,
                consumer: false,
                consumer_waiting: false,
                stopped: true,
            }),
            ready: Condvar::new(),
        }
    }

    /// Queues a frame or sheds it. `discontinuity`: the producer lost a frame before this one.
    pub fn push(&self, item: Item, discontinuity: bool) -> Pushed {
        // Whatever is dropped is dropped after the lock is released (a packet's drop returns
        // its buffer to a pool).
        let mut dropped: [Option<Item>; CATCH_UP + 1] = [const { None }; CATCH_UP + 1];
        let mut pushed = Pushed {
            queued: false,
            shed: 0,
            request_keyframe: false,
            start_consumer: false,
        };
        {
            let mut state = self.state.lock();
            if state.stopped {
                pushed.shed = 1;
                dropped[CATCH_UP] = Some(item);
            } else {
                if discontinuity && !item.keyframe && !state.need_key {
                    state.need_key = true;
                    pushed.request_keyframe = true;
                }
                let full = state.is_full(item.arrived);
                if item.keyframe {
                    // A keyframe starts a new chain: if it would not fit, what is queued is
                    // not worth decoding before it.
                    if full {
                        for slot in 0..CATCH_UP {
                            dropped[slot] = state.items[slot].take();
                        }
                        pushed.shed = state.len as u32;
                        state.head = 0;
                        state.len = 0;
                    }
                    state.need_key = false;
                } else if state.need_key {
                    pushed.shed = 1;
                    dropped[CATCH_UP] = Some(item);
                    drop(state);
                    drop(dropped);
                    return pushed;
                } else if full {
                    state.need_key = true;
                    // A decoder that fell this far behind is not catching up.
                    state.catching_up = false;
                    pushed.shed = 1;
                    pushed.request_keyframe = true;
                    dropped[CATCH_UP] = Some(item);
                    drop(state);
                    drop(dropped);
                    return pushed;
                }
                let tail = (state.head + state.len) % CATCH_UP;
                state.items[tail] = Some(item);
                state.len += 1;
                pushed.queued = true;
                if !state.consumer {
                    state.consumer = true;
                    pushed.start_consumer = true;
                } else if state.consumer_waiting {
                    self.ready.notify_one();
                }
            }
        }
        drop(dropped);
        pushed
    }

    /// The next frame, waiting up to `idle` for one.
    pub fn pop(&self, idle: Duration) -> Popped {
        let mut state = self.state.lock();
        let deadline = Instant::now() + idle;
        loop {
            if state.stopped {
                state.consumer = false;
                return Popped::Stopped;
            }
            if state.len > 0 {
                let head = state.head;
                let item = state.items[head].take();
                state.head = (head + 1) % CATCH_UP;
                state.len -= 1;
                if let Some(item) = item {
                    return Popped::Item(item);
                }
                continue;
            }
            state.consumer_waiting = true;
            let timed_out = self.ready.wait_until(&mut state, deadline).timed_out();
            state.consumer_waiting = false;
            if timed_out && state.len == 0 && !state.stopped {
                state.consumer = false;
                // Whatever comes next meets a new decoder.
                state.need_key = true;
                state.catching_up = true;
                return Popped::Idle;
            }
        }
    }

    /// Called by the decoder thread after a new decoder's pictures: once it finds the queue
    /// empty the decoder keeps up, and the queue is [`DEPTH`] deep from then on. Returns
    /// whether that is now the case.
    pub fn caught_up(&self) -> bool {
        let mut state = self.state.lock();
        if state.len == 0 {
            state.catching_up = false;
        }
        !state.catching_up
    }

    /// Opens the door (a viewer exists). Frames are refused until this is called.
    pub fn start(&self) {
        let mut state = self.state.lock();
        state.stopped = false;
        state.need_key = true;
        state.catching_up = true;
    }

    /// Closes the door, empties the queue and makes the consumer leave.
    pub fn stop(&self) {
        let mut dropped: [Option<Item>; CATCH_UP] = [const { None }; CATCH_UP];
        {
            let mut state = self.state.lock();
            state.stopped = true;
            state.need_key = true;
            for slot in 0..CATCH_UP {
                dropped[slot] = state.items[slot].take();
            }
            state.head = 0;
            state.len = 0;
            self.ready.notify_all();
        }
        drop(dropped);
    }

    /// The decoder lost its state (an error, a new decoder): drop what is queued and refuse
    /// deltas until a keyframe. Returns how many queued frames were dropped.
    pub fn gate(&self) -> u32 {
        let mut dropped: [Option<Item>; CATCH_UP] = [const { None }; CATCH_UP];
        let count;
        {
            let mut state = self.state.lock();
            state.need_key = true;
            state.catching_up = true;
            count = state.len as u32;
            for slot in 0..CATCH_UP {
                dropped[slot] = state.items[slot].take();
            }
            state.head = 0;
            state.len = 0;
        }
        drop(dropped);
        count
    }

    pub fn len(&self) -> usize {
        self.state.lock().len
    }

    pub fn has_consumer(&self) -> bool {
        self.state.lock().consumer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(keyframe: bool, tag: u8) -> Item {
        item_at(keyframe, tag, Instant::now())
    }

    fn item_at(keyframe: bool, tag: u8, arrived: Instant) -> Item {
        Item {
            packet: Packet::from(vec![tag]),
            codec: Codec::H264,
            keyframe,
            timestamp: tag as i64,
            description: None,
            arrived,
        }
    }

    /// Frames of a stream at `fps`, the `n`th arriving `n` frame intervals after `start`.
    fn frame(start: Instant, fps: u32, n: u32, keyframe: bool) -> Item {
        item_at(keyframe, n as u8, start + Duration::from_secs(1) * n / fps)
    }

    fn pop_tag(queue: &FrameQueue) -> Option<u8> {
        match queue.pop(Duration::from_millis(1)) {
            Popped::Item(item) => Some(item.packet.bytes()[0]),
            _ => None,
        }
    }

    #[test]
    fn refuses_everything_until_started_and_deltas_until_a_keyframe() {
        let queue = FrameQueue::new();
        assert!(!queue.push(item(true, 0), false).queued);
        queue.start();
        let delta = queue.push(item(false, 1), false);
        assert_eq!(
            (delta.queued, delta.shed, delta.request_keyframe),
            (false, 1, false)
        );
        let key = queue.push(item(true, 2), false);
        assert!(key.queued && key.start_consumer);
        assert!(queue.push(item(false, 3), false).queued);
        assert_eq!(pop_tag(&queue), Some(2));
        assert_eq!(pop_tag(&queue), Some(3));
    }

    /// A queue whose decoder has caught up once: the steady state.
    fn steady() -> FrameQueue {
        let queue = FrameQueue::new();
        queue.start();
        assert!(queue.push(item(true, 100), false).start_consumer);
        assert_eq!(pop_tag(&queue), Some(100));
        assert!(queue.caught_up());
        queue
    }

    #[test]
    fn a_new_decoder_gets_room_to_catch_up() {
        let queue = FrameQueue::new();
        queue.start();
        for tag in 0..CATCH_UP as u8 {
            assert!(queue.push(item(tag == 0, tag), false).queued, "{tag}");
        }
        for tag in 0..CATCH_UP as u8 {
            // Popping the first frame settles nothing: the decoder is still opening.
            assert!(!queue.caught_up());
            assert_eq!(pop_tag(&queue), Some(tag));
        }
        assert!(queue.caught_up());
        // A hundred milliseconds from now on: of frames a second apart, three.
        let start = Instant::now();
        for n in 0..4 {
            assert_eq!(
                queue.push(frame(start, 1, n, n == 0), false).queued,
                n < 3,
                "{n}"
            );
        }
    }

    /// The steady state holds 100 ms of frames at whatever rate they arrive, and three of a
    /// slower stream: the frames a decoder thread held back for `gap` finds waiting.
    #[test]
    fn the_steady_depth_is_a_hundred_milliseconds_of_frames() {
        for (fps, fits) in [(240u32, 24u32), (120, 12), (60, 6), (30, 3), (5, 3)] {
            let queue = steady();
            let start = Instant::now();
            let mut queued = 0;
            for n in 0..40 {
                let pushed = queue.push(frame(start, fps, n, false), false);
                if !pushed.queued {
                    assert_eq!((pushed.shed, pushed.request_keyframe), (1, true), "{fps} fps");
                    break;
                }
                queued += 1;
            }
            // The frame that arrives exactly 100 ms after the oldest is the first refused.
            assert_eq!(queued, fits, "{fps} fps");
        }
    }

    /// A decoder held back for 60 ms at 120 fps (seven frames) loses nothing; one that takes a
    /// frame now and then (slower than the stream) is shed when it is 100 ms behind.
    #[test]
    fn a_short_gap_is_ridden_out_and_a_long_one_sheds() {
        let queue = steady();
        let start = Instant::now();
        for n in 0..8 {
            let pushed = queue.push(frame(start, 120, n, false), false);
            assert_eq!((pushed.queued, pushed.shed, pushed.request_keyframe), (true, 0, false), "{n}");
        }
        // The decoder is back: it takes them in order, and the queue goes on as before.
        for n in 0..8 {
            assert_eq!(pop_tag(&queue), Some(n));
        }
        assert_eq!(queue.len(), 0);
        // A decoder that takes one frame of every two: the oldest waiting frame ages.
        let mut shed_at = None;
        for n in 8..200 {
            let pushed = queue.push(frame(start, 120, n, false), false);
            if !pushed.queued {
                shed_at = Some((n, queue.len()));
                assert!(pushed.request_keyframe);
                break;
            }
            if n % 2 == 0 {
                assert!(pop_tag(&queue).is_some());
            }
        }
        // 100 ms of frames behind (12 at 120 fps), not before and not later.
        let (n, len) = shed_at.expect("a decoder slower than the stream is shed");
        assert_eq!(len, 12, "shed at frame {n}");
        // Gated: what is queued is still decoded, deltas are refused until a keyframe.
        assert!(!queue.push(frame(start, 120, n + 1, false), false).queued);
    }

    /// A burst that arrives at once (the network held frames back and delivered them together)
    /// is not a slow decoder: it is queued, up to the queue's capacity.
    #[test]
    fn a_burst_arriving_at_once_is_queued() {
        let queue = steady();
        let now = Instant::now();
        for tag in 0..CATCH_UP as u8 {
            assert!(queue.push(item_at(false, tag, now), false).queued, "{tag}");
        }
        let overflow = queue.push(item_at(false, 99, now), false);
        assert_eq!((overflow.queued, overflow.request_keyframe), (false, true));
    }

    #[test]
    fn a_decoder_that_falls_sixteen_frames_behind_is_not_catching_up() {
        let queue = FrameQueue::new();
        queue.start();
        for tag in 0..CATCH_UP as u8 {
            queue.push(item(tag == 0, tag), false);
        }
        let overflow = queue.push(item(false, 99), false);
        assert_eq!((overflow.queued, overflow.request_keyframe), (false, true));
        assert!(queue.caught_up());
    }

    #[test]
    fn overflow_sheds_to_the_next_keyframe_and_asks_for_one_once() {
        let queue = steady();
        let start = Instant::now();
        let late = start + DEPTH;
        assert!(!queue.push(item_at(true, 0, start), false).start_consumer);
        assert!(!queue.push(item_at(false, 1, start), false).start_consumer);
        queue.push(item_at(false, 2, start), false);
        let overflow = queue.push(item_at(false, 3, late), false);
        assert_eq!(
            (overflow.queued, overflow.shed, overflow.request_keyframe),
            (false, 1, true)
        );
        // Still gated, and the request is not repeated per frame.
        assert_eq!(pop_tag(&queue), Some(0));
        let gated = queue.push(item(false, 4), false);
        assert_eq!((gated.queued, gated.request_keyframe), (false, false));
        assert!(queue.push(item(true, 5), false).queued);
        assert_eq!(pop_tag(&queue), Some(1));
        assert_eq!(pop_tag(&queue), Some(2));
        assert_eq!(pop_tag(&queue), Some(5));
    }

    #[test]
    fn a_keyframe_that_does_not_fit_replaces_the_queue() {
        let queue = steady();
        let start = Instant::now();
        for tag in 0..3 {
            queue.push(item_at(tag == 0, tag, start), false);
        }
        let key = queue.push(item_at(true, 9, start + DEPTH), false);
        assert_eq!((key.queued, key.shed), (true, 3));
        assert_eq!(pop_tag(&queue), Some(9));
        assert_eq!(pop_tag(&queue), None);
    }

    #[test]
    fn a_discontinuity_gates_and_asks() {
        let queue = FrameQueue::new();
        queue.start();
        queue.push(item(true, 0), false);
        let broken = queue.push(item(false, 1), true);
        assert_eq!((broken.queued, broken.request_keyframe), (false, true));
        assert!(queue.push(item(true, 2), true).queued);
    }

    #[test]
    fn idle_and_stop_deregister_the_consumer() {
        let queue = FrameQueue::new();
        queue.start();
        assert!(queue.push(item(true, 0), false).start_consumer);
        assert_eq!(pop_tag(&queue), Some(0));
        assert!(matches!(queue.pop(Duration::from_millis(1)), Popped::Idle));
        assert!(!queue.has_consumer());
        // A delta after an idle exit meets a fresh decoder: refused.
        assert!(!queue.push(item(false, 1), false).queued);
        assert!(queue.push(item(true, 2), false).start_consumer);
        queue.stop();
        assert!(matches!(
            queue.pop(Duration::from_millis(1)),
            Popped::Stopped
        ));
        assert_eq!(queue.len(), 0);
    }
}
