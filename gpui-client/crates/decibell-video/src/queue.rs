//! The hand-over from the receive thread to a stream's decoder thread: three frames deep.
//!
//! A full queue means the decoder is not keeping up. The frame that does not fit is shed, and
//! with it the reference chain: everything up to the next keyframe is refused at the door and
//! the caller is told to ask the streamer for one (the renderer's discontinuity handling:
//! `StreamVideoPlayer.tsx`, "gate deltas on the next keyframe and ask for one now").
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

/// Frames queued in the steady state.
pub(crate) const CAPACITY: usize = 3;
/// Frames queued while a new decoder catches up.
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
                let limit = if state.catching_up {
                    CATCH_UP
                } else {
                    CAPACITY
                };
                if item.keyframe {
                    // A keyframe starts a new chain: if it would not fit, what is queued is
                    // not worth decoding before it.
                    if state.len >= limit {
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
                } else if state.len >= limit {
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
    /// empty the decoder keeps up, and the queue is three frames deep from then on. Returns
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
        Item {
            packet: Packet::from(vec![tag]),
            codec: Codec::H264,
            keyframe,
            timestamp: tag as i64,
            description: None,
            arrived: Instant::now(),
        }
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
        // Three from now on.
        for tag in 0..4u8 {
            assert_eq!(
                queue.push(item(tag == 0, tag), false).queued,
                tag < 3,
                "{tag}"
            );
        }
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
        assert!(!queue.push(item(true, 0), false).start_consumer);
        assert!(!queue.push(item(false, 1), false).start_consumer);
        queue.push(item(false, 2), false);
        let overflow = queue.push(item(false, 3), false);
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
        for tag in 0..3 {
            queue.push(item(tag == 0, tag), false);
        }
        let key = queue.push(item(true, 9), false);
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
