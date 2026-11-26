//! Core → UI: the event queue and the one foreground task that drains it.
//!
//! The core calls [`EventSink::event`] from its own threads (tokio workers, the network
//! readers, the voice / video threads). Each event is pushed onto a queue; the UI thread
//! is woken **only when the queue goes from empty to non-empty**, and the pump task then
//! takes the whole queue in one turn and applies it inside a single `App::update`, so a
//! burst costs one wake of the UI thread and at most one frame (GPUI flushes effects and
//! schedules frames once per `update`).
//!
//! Encoded stream frames never come here: they go straight to the installed
//! [`StreamFrameSink`] on the thread that produced them.
//!
//! Locks: one `Mutex<Vec<_>>`, held for a `push` on the core's side and a `mem::swap` on
//! the UI's. Nothing else is shared between the core's threads and the UI thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use decibell_core::events::{CoreEvent, EventSink, StreamFrame, StreamThumbnail};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, Task};

/// What reaches the stores: a named event, or a stream's thumbnail (which the core hands
/// over on its own sink method, as bytes).
// Events are nearly all of the traffic: boxing them to shrink the rare thumbnail's slot
// would cost an allocation per event.
#[allow(clippy::large_enum_variant)]
pub enum Incoming {
    Core(CoreEvent),
    Thumbnail(StreamThumbnail),
}

/// Counters of the queue, for the tests and the headless `pump` dump. Monotonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct PumpStats {
    /// Events pushed by the core.
    pub events: u64,
    /// Times the UI thread was woken (the queue went from empty to non-empty).
    pub wakes: u64,
    /// Turns of the pump task that found something to apply.
    pub drains: u64,
    /// The largest batch one turn applied.
    pub largest_batch: u64,
}

/// The queue between the core's threads and the UI thread.
pub struct EventQueue {
    items: Mutex<Vec<Incoming>>,
    wake: UnboundedSender<()>,
    closed: AtomicBool,
    events: AtomicU64,
    wakes: AtomicU64,
    drains: AtomicU64,
    largest_batch: AtomicU64,
}

impl EventQueue {
    /// The queue and the receiver its pump waits on ([`spawn_pump`]).
    pub fn new() -> (Arc<EventQueue>, UnboundedReceiver<()>) {
        let (wake, woken) = unbounded();
        let queue = Arc::new(EventQueue {
            items: Mutex::new(Vec::new()),
            wake,
            closed: AtomicBool::new(false),
            events: AtomicU64::new(0),
            wakes: AtomicU64::new(0),
            drains: AtomicU64::new(0),
            largest_batch: AtomicU64::new(0),
        });
        (queue, woken)
    }

    /// Queue one item. From any thread; never blocks beyond the push.
    pub fn push(&self, item: Incoming) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        let was_empty = {
            let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
            let was_empty = items.is_empty();
            items.push(item);
            was_empty
        };
        self.events.fetch_add(1, Ordering::Relaxed);
        if was_empty {
            self.wakes.fetch_add(1, Ordering::Relaxed);
            // The receiver is gone only when the app is: nothing to wake then.
            let _ = self.wake.unbounded_send(());
        }
    }

    /// Take everything queued into `into` (which must be empty: its allocation becomes
    /// the queue's, so steady state allocates nothing).
    fn take(&self, into: &mut Vec<Incoming>) {
        debug_assert!(into.is_empty());
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::swap(&mut *items, into);
    }

    /// Stop accepting events (shutdown): later pushes are dropped.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    pub fn stats(&self) -> PumpStats {
        PumpStats {
            events: self.events.load(Ordering::Relaxed),
            wakes: self.wakes.load(Ordering::Relaxed),
            drains: self.drains.load(Ordering::Relaxed),
            largest_batch: self.largest_batch.load(Ordering::Relaxed),
        }
    }
}

/// Start the foreground task that drains `queue`: per wake it takes the whole queue and
/// hands it to `apply` inside one `App::update`. The task parks on the wake channel
/// between bursts (no timer, no polling) and ends when the task is dropped.
pub fn spawn_pump(
    queue: Arc<EventQueue>,
    mut woken: UnboundedReceiver<()>,
    cx: &mut App,
    mut apply: impl FnMut(&mut Vec<Incoming>, &mut App) + 'static,
) -> Task<()> {
    cx.spawn(async move |cx| {
        let mut batch = Vec::new();
        while woken.next().await.is_some() {
            queue.take(&mut batch);
            if batch.is_empty() {
                continue;
            }
            queue.drains.fetch_add(1, Ordering::Relaxed);
            queue
                .largest_batch
                .fetch_max(batch.len() as u64, Ordering::Relaxed);
            cx.update(|cx| apply(&mut batch, cx));
            // `apply` normally drains it; never carry an event into the next turn twice.
            batch.clear();
        }
    })
}

/// Where encoded stream frames go: called on the core's video receive / encoder threads,
/// once per whole decrypted frame of a stream the UI subscribed to
/// (`decibell_core::events::set_stream_frame_sink`). Implemented by the video crate
/// (a bounded per-stream decoder queue); it owns its queueing and shedding and must not
/// block. Until one is installed frames are dropped.
pub trait StreamFrameSink: Send + Sync + 'static {
    fn frame(&self, frame: StreamFrame);
    /// `key` stopped being a frame sink: forget its shedding state.
    fn removed(&self, _key: &str) {}
}

/// The process's `EventSink`: events and thumbnails into the queue, frames to the
/// [`StreamFrameSink`].
pub struct ModelSink {
    queue: Arc<EventQueue>,
    // A `Box` inside so the trait object sits behind a thin pointer `ArcSwap` can hold;
    // the per-frame read is lock-free.
    frames: ArcSwapOption<Box<dyn StreamFrameSink>>,
}

impl ModelSink {
    pub fn new(queue: Arc<EventQueue>) -> ModelSink {
        ModelSink {
            queue,
            frames: ArcSwapOption::empty(),
        }
    }

    pub fn set_stream_frame_sink(&self, sink: Option<Box<dyn StreamFrameSink>>) {
        self.frames.store(sink.map(Arc::new));
    }

    pub fn queue(&self) -> &Arc<EventQueue> {
        &self.queue
    }
}

impl EventSink for ModelSink {
    fn event(&self, event: CoreEvent) {
        self.queue.push(Incoming::Core(event));
    }

    fn stream_frame(&self, frame: StreamFrame) {
        if let Some(sink) = &*self.frames.load() {
            sink.frame(frame);
        }
    }

    fn stream_thumbnail(&self, thumb: StreamThumbnail) {
        self.queue.push(Incoming::Thumbnail(thumb));
    }

    fn stream_frame_sink_removed(&self, key: &str) {
        if let Some(sink) = &*self.frames.load() {
            sink.removed(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::AtomicUsize;

    use decibell_core::events::VoicePingUpdatedPayload;
    use gpui::TestAppContext;

    use super::*;

    fn ping(n: u32) -> CoreEvent {
        CoreEvent::VoicePingUpdated(VoicePingUpdatedPayload { latency_ms: n })
    }

    /// 10,000 events queued before the UI thread runs: one wake, one turn of the pump,
    /// one `update`, every event applied in order. (GPUI's test scheduler is
    /// single-threaded, so the producer is this thread here; `tests/pump_real_loop.rs`
    /// does it from 8 threads against the real event loop.)
    #[gpui::test]
    fn a_burst_costs_one_wake_and_one_drain(cx: &mut TestAppContext) {
        let (queue, woken) = EventQueue::new();
        let seen: Rc<RefCell<Vec<u32>>> = Rc::default();
        let updates = Rc::new(RefCell::new(0u32));
        let _pump = cx.update(|cx| {
            let (seen, updates) = (seen.clone(), updates.clone());
            spawn_pump(queue.clone(), woken, cx, move |batch, _| {
                *updates.borrow_mut() += 1;
                for item in batch.drain(..) {
                    if let Incoming::Core(CoreEvent::VoicePingUpdated(p)) = item {
                        seen.borrow_mut().push(p.latency_ms);
                    }
                }
            })
        });
        cx.run_until_parked();
        assert_eq!(queue.stats(), PumpStats::default());

        for i in 0..10_000 {
            queue.push(Incoming::Core(ping(i)));
        }
        cx.run_until_parked();
        let stats = queue.stats();
        assert_eq!(stats.events, 10_000);
        assert_eq!(stats.wakes, 1, "one wake for the whole burst");
        assert_eq!(stats.drains, 1, "one turn of the pump");
        assert_eq!(stats.largest_batch, 10_000);
        assert_eq!(*updates.borrow(), 1, "one App::update, so at most one frame");
        assert!(seen.borrow().iter().copied().eq(0..10_000));

        // The next burst wakes again: exactly once more. A single event is a burst too.
        for i in 0..500 {
            queue.push(Incoming::Core(ping(i)));
        }
        cx.run_until_parked();
        queue.push(Incoming::Core(ping(7)));
        cx.run_until_parked();
        let stats = queue.stats();
        assert_eq!((stats.events, stats.wakes, stats.drains), (10_501, 3, 3));
        assert_eq!(*updates.borrow(), 3);

        // After close (shutdown) nothing is queued and nobody is woken.
        queue.close();
        queue.push(Incoming::Core(ping(1)));
        cx.run_until_parked();
        assert_eq!(queue.stats().events, 10_501);
    }

    /// While the consumer keeps up, wakes never exceed events and every wake finds work:
    /// producers racing a live consumer (the queue alone, no GPUI).
    #[test]
    fn wakes_are_bounded_by_empty_to_non_empty_transitions() {
        let (queue, mut woken) = EventQueue::new();
        let applied = Arc::new(AtomicUsize::new(0));
        let consumer = {
            let (queue, applied) = (queue.clone(), applied.clone());
            std::thread::Builder::new()
                .name("pump-test-consumer".into())
                .spawn(move || {
                    let mut batch = Vec::new();
                    let mut turns = 0u64;
                    while applied.load(Ordering::Relaxed) < 10_000 {
                        if futures::executor::block_on(woken.next()).is_none() {
                            break;
                        }
                        queue.take(&mut batch);
                        assert!(!batch.is_empty(), "a wake always finds the queue non-empty");
                        turns += 1;
                        applied.fetch_add(batch.len(), Ordering::Relaxed);
                        batch.clear();
                    }
                    turns
                })
                .unwrap()
        };
        let producers: Vec<_> = (0..4)
            .map(|t| {
                let queue = queue.clone();
                std::thread::Builder::new()
                    .name(format!("pump-test-{t}"))
                    .spawn(move || {
                        for i in 0..2500 {
                            queue.push(Incoming::Core(ping(i)));
                        }
                    })
                    .unwrap()
            })
            .collect();
        for p in producers {
            p.join().unwrap();
        }
        let turns = consumer.join().unwrap();
        let stats = queue.stats();
        assert_eq!(applied.load(Ordering::Relaxed), 10_000);
        assert_eq!(stats.events, 10_000);
        assert_eq!(stats.wakes, turns, "every wake was one turn");
        assert!(stats.wakes <= stats.events);
    }

    #[test]
    fn frames_bypass_the_queue() {
        struct Count(Arc<AtomicUsize>, Arc<AtomicUsize>);
        impl StreamFrameSink for Count {
            fn frame(&self, _: StreamFrame) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
            fn removed(&self, _: &str) {
                self.1.fetch_add(1, Ordering::Relaxed);
            }
        }
        let frame = || StreamFrame {
            key: "7".into(),
            codec: 1,
            keyframe: true,
            timestamp: 0,
            data: vec![0; 16],
            description: None,
            discontinuity: false,
        };
        let (queue, _woken) = EventQueue::new();
        let sink = ModelSink::new(queue.clone());
        // No sink installed: dropped, and nothing reaches the UI queue.
        sink.stream_frame(frame());
        sink.stream_frame_sink_removed("7");
        let (frames, removed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        sink.set_stream_frame_sink(Some(Box::new(Count(frames.clone(), removed.clone()))));
        sink.stream_frame(frame());
        sink.stream_frame(frame());
        sink.stream_frame_sink_removed("7");
        assert_eq!(frames.load(Ordering::Relaxed), 2);
        assert_eq!(removed.load(Ordering::Relaxed), 1);
        assert_eq!(queue.stats(), PumpStats::default());
        // A thumbnail is an ordinary queued item.
        sink.stream_thumbnail(StreamThumbnail {
            owner_username: "bob".into(),
            owner_uid: 2,
            data: vec![1, 2, 3],
        });
        assert_eq!(queue.stats().events, 1);
        assert_eq!(queue.stats().wakes, 1);
    }
}
