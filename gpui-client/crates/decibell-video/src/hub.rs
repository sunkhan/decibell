//! The hub: encoded frames in from the receive thread, one decoder thread per displayed stream,
//! pictures out to the renderer, one present request per new picture.
//!
//! ```text
//! receive thread            decibell-video-<n> (one per displayed stream)       main thread
//! hub.push(key, frame) ---> 3-frame queue ---> decoder: decode, copy / upload,
//!   (never blocks)                              wait for the GPU, publish
//!                                               signal ------------------------> presenter task:
//!                                                                                 window.request_present()
//!                                                                                renderer draws the retained
//!                                                                                scene with the newest picture
//! ```
//!
//! What a stream costs when nobody looks at it: nothing. A stream has a decoder thread only
//! while it has a [`Viewer`] and frames arrive; frames for a stream without viewers are dropped
//! at `push` (and the app is told through [`HubCallbacks::interest`], so the core stops
//! delivering them at all: the renderer's frame-sink set).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use futures::task::AtomicWaker;
use gpui::AnyWindowHandle;
use parking_lot::{Mutex, RwLock};

use crate::codec::{Codec, CodecSupport, DecodePath};
use crate::decode::{Backend, CaptureCell, DecodeError, Failure, FrameCx, LumaRow, PictureDecoder};
use crate::packet::Packet;
use crate::queue::{FrameQueue, Item, Popped};
use crate::stats::{Counters, StreamStats};

const RELAXED: Ordering = Ordering::Relaxed;

/// A decoder thread without a frame for this long exits and frees its decoder (the stream
/// stalled or ended without being removed). The next frame starts a new one on a keyframe.
const IDLE_EXIT: Duration = Duration::from_secs(2);
/// At most one keyframe request per stream in this time (the core throttles the same way).
const KEYFRAME_REQUEST_INTERVAL_MS: u64 = 500;

/// One whole encoded frame of a stream, as the core's `StreamFrame` carries it.
pub struct EncodedFrame {
    /// The wire's `VideoCodec` byte.
    pub codec: u8,
    pub keyframe: bool,
    /// Microseconds, increasing.
    pub timestamp: i64,
    pub data: Packet,
    /// The decoder configuration record (hvcC / av1C) the streamer sends with keyframes of
    /// streams whose parameter sets are not in the bitstream.
    pub description: Option<Vec<u8>>,
    /// The producer shed a frame of this stream before this one.
    pub discontinuity: bool,
}

/// Something the app may want to show or log about a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// A decoder was opened (also after every recovery).
    DecoderOpened { codec: Codec, path: DecodePath },
    /// Hardware decode failed for this stream; it continues in software.
    FellBackToSoftware { reason: String },
    /// A decoder failed and was reopened.
    DecoderError { reason: String },
    /// The GPU device is gone; the stream resumes when the renderer has a new one.
    DeviceLost,
    /// The first picture of a decoder is up, or the stream's picture size changed: what a view
    /// waits for to take its spinner down, and the aspect ratio for its layout.
    Picture { width: u32, height: u32 },
}

/// How the hub reaches the rest of the client. Called from decoder threads and from the thread
/// that calls [`VideoHub::push`]; must not block.
pub struct HubCallbacks {
    /// Ask the streamer `key` for a keyframe (the core's `request_stream_keyframe`, or
    /// `force_keyframe` for our own preview). Already throttled per stream.
    pub request_keyframe: Box<KeyframeCallback>,
    /// Stream `key` gained its first viewer (true) or lost its last (false): the core's
    /// `set_stream_frame_sink`.
    pub interest: Box<InterestCallback>,
    pub event: Box<EventCallback>,
}

pub type KeyframeCallback = dyn Fn(&str) + Send + Sync;
pub type InterestCallback = dyn Fn(&str, bool) + Send + Sync;
pub type EventCallback = dyn Fn(&str, StreamEvent) + Send + Sync;

impl Default for HubCallbacks {
    fn default() -> Self {
        Self {
            request_keyframe: Box::new(|_| {}),
            interest: Box::new(|_, _| {}),
            event: Box::new(|_, _| {}),
        }
    }
}

/// A failure to inject into a stream's decoder, for tests of the recovery paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The next decode fails as a decoder error would.
    DecodeError,
    /// The renderer's device is marked lost before the next decode: the renderer creates a new
    /// device and every stream has to move to it.
    DeviceLost,
}

/// Where a stream's pictures go.
pub(crate) struct StreamTarget {
    #[cfg(vk)]
    pub surface: Arc<gpui_wgpu::WgpuSurface>,
    pub counters: Arc<Counters>,
}

impl StreamTarget {
    fn clear(&self) {
        #[cfg(vk)]
        self.surface.clear();
    }
}

struct ViewerEntry {
    id: u64,
    window: Option<AnyWindowHandle>,
}

pub(crate) struct StreamShared {
    key: Box<str>,
    /// The `<n>` of `decibell-video-<n>`.
    index: u64,
    queue: FrameQueue,
    pub(crate) target: StreamTarget,
    viewers: Mutex<Vec<ViewerEntry>>,
    viewer_count: AtomicU32,
    /// A picture was published since the presenter last looked.
    dirty: AtomicBool,
    last_keyframe_request_ms: AtomicU64,
    software_only: AtomicBool,
    /// 0 none, 1 decode error, 2 device loss.
    fault: AtomicU8,
    capture: CaptureCell,
    removed: AtomicBool,
}

/// Wakes the presenter task: at most once per turn it has not handled yet, however many
/// pictures and streams arrive in between.
struct PresentSignal {
    pending: AtomicBool,
    waker: AtomicWaker,
    wakes: AtomicU64,
}

impl PresentSignal {
    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.wakes.fetch_add(1, RELAXED);
            self.waker.wake();
        }
    }
}

pub(crate) struct HubInner {
    streams: RwLock<HashMap<Box<str>, Arc<StreamShared>>>,
    callbacks: RwLock<Arc<HubCallbacks>>,
    pub(crate) backend: Arc<dyn Backend>,
    signal: PresentSignal,
    /// Windows that showed a stream which was torn down: they get one more present, so the
    /// renderer lets go of the stream's textures.
    released_windows: Mutex<Vec<AnyWindowHandle>>,
    release_pending: AtomicBool,
    next_index: AtomicU64,
    next_viewer: AtomicU64,
    /// Decoder threads alive.
    threads: AtomicI64,
    force_software: AtomicBool,
    started: Instant,
    unknown_frames: AtomicU64,
    presenter: Mutex<Option<gpui::Task<()>>>,
}

/// The process's stream playback: see the module docs. Cheap to clone.
#[derive(Clone)]
pub struct VideoHub {
    pub(crate) inner: Arc<HubInner>,
}

struct GlobalHub(VideoHub);
impl gpui::Global for GlobalHub {}

impl VideoHub {
    /// The app's hub, created on first use. No FFmpeg context, thread or texture exists until a
    /// displayed stream receives its first frame.
    pub fn global(cx: &mut gpui::App) -> VideoHub {
        if let Some(hub) = cx.try_global::<GlobalHub>() {
            return hub.0.clone();
        }
        let hub = VideoHub::with_backend(crate::platform::backend());
        hub.attach(cx);
        cx.set_global(GlobalHub(hub.clone()));
        hub
    }

    /// A hub that decodes in software into a counting sink and presents nothing: no window, no
    /// GPU, no presenter task. For tests and benchmarks of everything up to the picture.
    #[doc(hidden)]
    pub fn headless() -> VideoHub {
        VideoHub::with_backend(Arc::new(crate::cpu::CpuBackend::default()))
    }

    pub(crate) fn with_backend(backend: Arc<dyn Backend>) -> VideoHub {
        let force_software = std::env::var("DECIBELL_VIDEO").is_ok_and(|value| value == "software");
        VideoHub {
            inner: Arc::new(HubInner {
                streams: RwLock::new(HashMap::new()),
                callbacks: RwLock::new(Arc::new(HubCallbacks::default())),
                backend,
                signal: PresentSignal {
                    pending: AtomicBool::new(false),
                    waker: AtomicWaker::new(),
                    wakes: AtomicU64::new(0),
                },
                released_windows: Mutex::new(Vec::new()),
                release_pending: AtomicBool::new(false),
                next_index: AtomicU64::new(0),
                next_viewer: AtomicU64::new(1),
                threads: AtomicI64::new(0),
                force_software: AtomicBool::new(force_software),
                started: Instant::now(),
                unknown_frames: AtomicU64::new(0),
                presenter: Mutex::new(None),
            }),
        }
    }

    /// Starts the task that turns "a stream has a new picture" into one
    /// `Window::request_present` per window showing it. It sleeps until a decoder thread wakes
    /// it; it owns no timer.
    fn attach(&self, cx: &mut gpui::App) {
        let hub = Arc::downgrade(&self.inner);
        let task = cx.spawn(async move |cx| {
            let mut windows: Vec<AnyWindowHandle> = Vec::new();
            loop {
                let Some(inner) = hub.upgrade() else { break };
                futures::future::poll_fn(|task_cx| {
                    inner.signal.waker.register(task_cx.waker());
                    if inner.signal.pending.swap(false, Ordering::AcqRel) {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                })
                .await;
                windows.clear();
                for stream in inner.streams.read().values() {
                    if stream.dirty.swap(false, Ordering::AcqRel) {
                        for viewer in stream.viewers.lock().iter() {
                            if let Some(window) = viewer.window
                                && !windows.contains(&window)
                            {
                                windows.push(window);
                            }
                        }
                    }
                }
                let released = inner.release_pending.swap(false, Ordering::AcqRel);
                if released {
                    for window in inner.released_windows.lock().drain(..) {
                        if !windows.contains(&window) {
                            windows.push(window);
                        }
                    }
                }
                let present = |cx: &mut gpui::AsyncApp| {
                    cx.update(|cx| {
                        for window in &windows {
                            // A closed window is simply skipped.
                            let _ = window.update(cx, |_, window, _| window.request_present());
                        }
                    });
                };
                if !released {
                    present(cx);
                } else {
                    // A stream was torn down. Its last pictures are still referenced by the
                    // renderer: by the frames that sampled them, until it notices the GPU has
                    // finished those (it only looks when it draws), and by the bind groups it
                    // caches per picture, until its next frame. With no stream there may be no
                    // next frame for a long time, so: let the GPU finish, look, present once
                    // more (the bind groups go), look again. Two short one-shot timers per
                    // closed stream, and its textures are freed.
                    let pause = Duration::from_millis(120);
                    present(cx);
                    cx.background_executor().timer(pause).await;
                    inner.backend.collect();
                    present(cx);
                    cx.background_executor().timer(pause).await;
                    inner.backend.collect();
                }
                drop(inner);
            }
        });
        *self.inner.presenter.lock() = Some(task);
    }

    pub fn set_callbacks(&self, callbacks: HubCallbacks) {
        *self.inner.callbacks.write() = Arc::new(callbacks);
    }

    /// The stream with this key (the streamer's media key: their uid in decimal), created if
    /// it is new. Creating a stream allocates a few counters and nothing else.
    pub fn stream(&self, key: &str) -> Stream {
        if let Some(shared) = self.inner.streams.read().get(key) {
            return Stream {
                shared: shared.clone(),
                hub: self.inner.clone(),
            };
        }
        let mut streams = self.inner.streams.write();
        let shared = streams
            .entry(key.into())
            .or_insert_with(|| {
                let counters = Arc::new(Counters::new());
                let target = StreamTarget {
                    #[cfg(vk)]
                    surface: {
                        let surface = gpui_wgpu::WgpuSurface::new();
                        let counters = counters.clone();
                        surface.set_draw_observer(Box::new(move |serial| {
                            counters.frame_presented(serial)
                        }));
                        surface
                    },
                    counters,
                };
                Arc::new(StreamShared {
                    key: key.into(),
                    index: self.inner.next_index.fetch_add(1, RELAXED),
                    queue: FrameQueue::new(),
                    target,
                    viewers: Mutex::new(Vec::new()),
                    viewer_count: AtomicU32::new(0),
                    dirty: AtomicBool::new(false),
                    last_keyframe_request_ms: AtomicU64::new(0),
                    software_only: AtomicBool::new(false),
                    fault: AtomicU8::new(0),
                    capture: CaptureCell::default(),
                    removed: AtomicBool::new(false),
                })
            })
            .clone();
        Stream {
            shared,
            hub: self.inner.clone(),
        }
    }

    /// The stream ended (the streamer stopped, we stopped watching, we left the channel): its
    /// decoder thread stops, its textures are freed and its last picture is no longer drawn.
    pub fn remove(&self, key: &str) {
        let removed = self.inner.streams.write().remove(key);
        if let Some(stream) = removed {
            stream.removed.store(true, RELAXED);
            stream.queue.stop();
            stream.target.clear();
            self.inner.release(&stream);
        }
    }

    /// One frame from the receive path. Never blocks: the frame is queued for the stream's
    /// decoder thread (started here if there is none), or shed. Frames of a stream nobody
    /// displays, or that was never created, are dropped.
    pub fn push(&self, key: &str, frame: EncodedFrame) {
        // The map's lock is held for the lookup only: the callbacks below are the app's code.
        let stream = self.inner.streams.read().get(key).cloned();
        let Some(stream) = stream else {
            self.inner.unknown_frames.fetch_add(1, RELAXED);
            return;
        };
        let counters = &stream.target.counters;
        if stream.viewer_count.load(RELAXED) == 0 {
            counters.unwatched.fetch_add(1, RELAXED);
            return;
        }
        counters.received.fetch_add(1, RELAXED);
        let Some(codec) = Codec::from_wire(frame.codec) else {
            counters.shed.fetch_add(1, RELAXED);
            counters.errors.fetch_add(1, RELAXED);
            return;
        };
        let pushed = stream.queue.push(
            Item {
                packet: frame.data,
                codec,
                keyframe: frame.keyframe,
                timestamp: frame.timestamp,
                description: frame.description,
                arrived: Instant::now(),
            },
            frame.discontinuity,
        );
        if pushed.shed > 0 {
            counters.shed.fetch_add(pushed.shed as u64, RELAXED);
        }
        if !pushed.queued || pushed.request_keyframe {
            // Gated until a keyframe: keep asking (throttled) until one arrives.
            self.inner.request_keyframe(&stream);
        }
        if pushed.start_consumer {
            self.inner.clone().start_decoder(stream);
        }
    }

    /// The key stopped being delivered by the core (`EventSink::stream_frame_sink_removed`).
    /// Nothing to forget here: a stream without viewers has no state.
    pub fn sink_removed(&self, _key: &str) {}

    /// Decode every stream in software from its next decoder on (settings, tests).
    pub fn set_force_software(&self, force: bool) {
        self.inner.force_software.store(force, RELAXED);
    }

    /// What this machine does with each codec, for the decode capabilities the client
    /// advertises. Hardware answers come from the renderer's GPU, so ask once a window exists;
    /// before that only the software decoders are reported.
    pub fn capabilities(&self) -> [CodecSupport; 3] {
        Codec::ALL.map(|codec| self.inner.backend.support(codec))
    }

    /// Decoder threads alive right now.
    pub fn decoder_threads(&self) -> i64 {
        self.inner.threads.load(RELAXED)
    }

    /// Plane textures alive right now (0 when no stream is displayed).
    pub fn live_textures(&self) -> i64 {
        self.inner.backend.live_textures()
    }

    /// Times the presenter task was woken (one per burst of new pictures).
    pub fn present_wakes(&self) -> u64 {
        self.inner.signal.wakes.load(RELAXED)
    }

    /// Frames pushed for keys no stream was created for.
    pub fn unknown_frames(&self) -> u64 {
        self.inner.unknown_frames.load(RELAXED)
    }
}

impl HubInner {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64 + 1
    }

    fn request_keyframe(&self, stream: &StreamShared) {
        let now = self.now_ms();
        let last = stream.last_keyframe_request_ms.load(RELAXED);
        if last != 0 && now.saturating_sub(last) < KEYFRAME_REQUEST_INTERVAL_MS {
            return;
        }
        if stream
            .last_keyframe_request_ms
            .compare_exchange(last, now, RELAXED, RELAXED)
            .is_err()
        {
            return;
        }
        stream
            .target
            .counters
            .keyframe_requests
            .fetch_add(1, RELAXED);
        let callbacks = self.callbacks.read().clone();
        (callbacks.request_keyframe)(&stream.key);
    }

    fn event(&self, stream: &StreamShared, event: StreamEvent) {
        log::info!("video: stream {}: {event:?}", stream.key);
        let callbacks = self.callbacks.read().clone();
        (callbacks.event)(&stream.key, event);
    }

    /// One more present for the windows that showed `stream`, then a look at what the GPU has
    /// finished (see the presenter).
    fn release(&self, stream: &StreamShared) {
        {
            let mut released = self.released_windows.lock();
            for viewer in stream.viewers.lock().iter() {
                if let Some(window) = viewer.window
                    && !released.contains(&window)
                {
                    released.push(window);
                }
            }
        }
        self.release_pending.store(true, Ordering::Release);
        self.signal.notify();
    }

    fn start_decoder(self: Arc<Self>, stream: Arc<StreamShared>) {
        let name = format!("decibell-video-{}", stream.index);
        let hub = self.clone();
        let spawned = std::thread::Builder::new().name(name).spawn({
            let stream = stream.clone();
            move || decoder_thread(hub, stream)
        });
        if let Err(error) = spawned {
            log::error!("video: starting a decoder thread: {error}");
            // Nothing consumes the queue: close it so the next frame tries again.
            stream.queue.stop();
            if stream.viewer_count.load(RELAXED) > 0 && !stream.removed.load(RELAXED) {
                stream.queue.start();
            }
        }
    }
}

struct Decoding {
    decoder: Option<Box<dyn PictureDecoder>>,
    codec: Option<Codec>,
    description: Vec<u8>,
    /// The picture size last announced with [`StreamEvent::Picture`] (reset with the decoder).
    announced: (u32, u32),
    /// The decoder is new and the queue still gives it room (see `queue.rs`).
    catching_up: bool,
}

fn decoder_thread(hub: Arc<HubInner>, stream: Arc<StreamShared>) {
    hub.threads.fetch_add(1, RELAXED);
    let counters = &stream.target.counters;
    let mut decoding = Decoding {
        decoder: None,
        codec: None,
        description: Vec::new(),
        announced: (0, 0),
        catching_up: true,
    };
    let stopped = loop {
        match stream.queue.pop(IDLE_EXIT) {
            Popped::Item(item) => decode_one(&hub, &stream, &mut decoding, item),
            Popped::Idle => break false,
            Popped::Stopped => break true,
        }
    };
    // The decoder's slots go with it; what the surface still shows stays until it is cleared.
    drop(decoding);
    counters.clear_decoder();
    counters.presentation_paused();
    if stopped {
        stream.target.clear();
        hub.release(&stream);
    }
    counters.dump_trace(&stream.key);
    hub.threads.fetch_sub(1, RELAXED);
}

fn decode_one(hub: &HubInner, stream: &StreamShared, decoding: &mut Decoding, item: Item) {
    let counters = &stream.target.counters;
    let Item {
        packet,
        codec,
        keyframe,
        timestamp,
        description,
        arrived,
    } = item;

    let fault = stream.fault.swap(0, RELAXED);
    if fault == 2 {
        hub.backend.simulate_device_loss();
    }

    // A new codec, or a keyframe with a new configuration record, needs a new decoder (the
    // renderer's reconfigure).
    let new_description = keyframe
        && description
            .as_deref()
            .is_some_and(|bytes| bytes != decoding.description.as_slice());
    if decoding.decoder.is_none() || decoding.codec != Some(codec) || new_description {
        if !keyframe {
            // A decoder can only start on a keyframe.
            let shed = 1 + stream.queue.gate();
            counters.shed.fetch_add(shed as u64, RELAXED);
            hub.request_keyframe(stream);
            return;
        }
        decoding.decoder = None;
        counters.clear_decoder();
        if !hub.backend.ready() {
            // Between a device loss and the renderer's new device. A present makes the
            // renderer look at its device (that is where it recovers).
            counters
                .shed
                .fetch_add(1 + stream.queue.gate() as u64, RELAXED);
            hub.request_keyframe(stream);
            stream.dirty.store(true, Ordering::Release);
            hub.signal.notify();
            return;
        }
        let mut allow_hardware =
            !stream.software_only.load(RELAXED) && !hub.force_software.load(RELAXED);
        let decoder = loop {
            match hub.backend.open(
                codec,
                description.as_deref(),
                allow_hardware,
                &stream.target,
            ) {
                Ok(decoder) => break decoder,
                // The hardware decoder does not take this stream: software, with the keyframe
                // in hand.
                Err(error) if allow_hardware && error.failure == Failure::Decoder => {
                    allow_hardware = false;
                    stream.software_only.store(true, RELAXED);
                    counters.fallbacks.fetch_add(1, RELAXED);
                    counters.errors.fetch_add(1, RELAXED);
                    hub.event(
                        stream,
                        StreamEvent::FellBackToSoftware {
                            reason: error.message,
                        },
                    );
                }
                Err(error) => {
                    counters.shed.fetch_add(1, RELAXED);
                    failed(hub, stream, decoding, DecodePath::Software, error);
                    return;
                }
            }
        };
        counters.opens.fetch_add(1, RELAXED);
        counters.set_decoder(codec, Some(decoder.path()));
        hub.event(
            stream,
            StreamEvent::DecoderOpened {
                codec,
                path: decoder.path(),
            },
        );
        decoding.decoder = Some(decoder);
        decoding.codec = Some(codec);
        decoding.announced = (0, 0);
        decoding.catching_up = true;
        decoding.description.clear();
        if let Some(description) = &description {
            decoding.description.extend_from_slice(description);
        }
    }
    let Some(decoder) = decoding.decoder.as_mut() else {
        return;
    };

    let serial = counters.frame_started(arrived, packet.len());
    let handed = Instant::now();
    let cx = FrameCx {
        counters,
        serial,
        capture: &stream.capture,
    };
    let path = decoder.path();
    let result = if fault == 1 {
        drop(packet);
        Err(DecodeError::decoder("injected decode error"))
    } else {
        decoder.decode(packet, keyframe, timestamp, &cx)
    };
    match result {
        Ok(0) => {}
        Ok(_) => {
            counters.frame_published(serial, arrived, handed);
            stream.dirty.store(true, Ordering::Release);
            hub.signal.notify();
            if decoding.catching_up && stream.queue.caught_up() {
                decoding.catching_up = false;
            }
            let size = counters.size();
            if size != decoding.announced {
                decoding.announced = size;
                hub.event(
                    stream,
                    StreamEvent::Picture {
                        width: size.0,
                        height: size.1,
                    },
                );
            }
        }
        Err(error) => failed(hub, stream, decoding, path, error),
    }
}

/// A decoder (or its opening) failed: drop it, decide what the next one is, start again at a
/// keyframe. A hardware decoder that failed is answered by decoding in software.
fn failed(
    hub: &HubInner,
    stream: &StreamShared,
    decoding: &mut Decoding,
    path: DecodePath,
    error: DecodeError,
) {
    let counters = &stream.target.counters;
    counters.errors.fetch_add(1, RELAXED);
    log::warn!(
        "video: stream {} ({} decode): {error}",
        stream.key,
        path.label()
    );
    decoding.decoder = None;
    decoding.codec = None;
    counters.clear_decoder();
    counters.presentation_paused();
    match error.failure {
        Failure::DeviceLost => {
            counters.device_losses.fetch_add(1, RELAXED);
            stream.target.clear();
            hub.event(stream, StreamEvent::DeviceLost);
            // The renderer finds out about its device at its next frame.
            stream.dirty.store(true, Ordering::Release);
            hub.signal.notify();
        }
        Failure::Decoder if path == DecodePath::Hardware => {
            stream.software_only.store(true, RELAXED);
            counters.fallbacks.fetch_add(1, RELAXED);
            hub.event(
                stream,
                StreamEvent::FellBackToSoftware {
                    reason: error.message,
                },
            );
        }
        Failure::Decoder => hub.event(
            stream,
            StreamEvent::DecoderError {
                reason: error.message,
            },
        ),
    }
    counters.shed.fetch_add(stream.queue.gate() as u64, RELAXED);
    // Not throttled away by an earlier request: the chain is broken now.
    stream.last_keyframe_request_ms.store(0, RELAXED);
    hub.request_keyframe(stream);
}

/// A handle to one stream of the hub. Cheap to clone.
#[derive(Clone)]
pub struct Stream {
    pub(crate) shared: Arc<StreamShared>,
    pub(crate) hub: Arc<HubInner>,
}

impl Stream {
    pub fn key(&self) -> &str {
        &self.shared.key
    }

    /// Declares that something on screen shows this stream, for as long as the returned value
    /// lives. The first viewer opens the stream's queue and asks for a keyframe; dropping the
    /// last one stops the decoder and frees its textures. Moving a stream between two places
    /// (grid tile to mini-player): create the new viewer before dropping the old one.
    pub fn viewer(&self) -> Viewer {
        let id = self.hub.next_viewer.fetch_add(1, RELAXED);
        let first = {
            let mut viewers = self.shared.viewers.lock();
            viewers.push(ViewerEntry { id, window: None });
            self.shared
                .viewer_count
                .store(viewers.len() as u32, RELAXED);
            viewers.len() == 1
        };
        if first && !self.shared.removed.load(RELAXED) {
            self.shared.queue.start();
            let callbacks = self.hub.callbacks.read().clone();
            (callbacks.interest)(&self.shared.key, true);
            self.shared.last_keyframe_request_ms.store(0, RELAXED);
            self.hub.request_keyframe(&self.shared);
        }
        Viewer {
            inner: Arc::new(ViewerInner {
                stream: self.clone(),
                id,
                #[cfg(vk)]
                views: Default::default(),
            }),
        }
    }

    pub fn stats(&self) -> StreamStats {
        self.shared
            .target
            .counters
            .snapshot(self.shared.queue.len())
    }

    /// The newest picture's size, when there is one.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        #[cfg(vk)]
        return self.shared.target.surface.picture_size();
        #[cfg(not(vk))]
        None
    }

    /// Whether the stream has a decoder thread right now.
    pub fn is_decoding(&self) -> bool {
        self.shared.queue.has_consumer()
    }

    /// Decode this stream in software from its next decoder on.
    pub fn set_software_only(&self, software: bool) {
        self.shared.software_only.store(software, RELAXED);
    }

    /// Makes the stream's decoder fail once (tests of the recovery paths).
    pub fn inject(&self, fault: Fault) {
        self.shared.fault.store(
            match fault {
                Fault::DecodeError => 1,
                Fault::DeviceLost => 2,
            },
            RELAXED,
        );
    }

    /// One luma row of the next published picture, read back from the texture the renderer
    /// samples (tests). `y_fraction`: the row's position, 0.0 top to 1.0 bottom.
    pub fn capture_luma_row(&self, y_fraction: f32) -> mpsc::Receiver<LumaRow> {
        self.shared.capture.request(y_fraction)
    }
}

/// "This stream is on screen": see [`Stream::viewer`]. A [`crate::VideoView`] paints it. Clones
/// are the same viewer; the stream loses it when the last clone is dropped.
#[derive(Clone)]
pub struct Viewer {
    inner: Arc<ViewerInner>,
}

struct ViewerInner {
    stream: Stream,
    id: u64,
    /// What the view paints, one per fit, made on first use.
    #[cfg(vk)]
    views: [std::sync::OnceLock<Arc<gpui_wgpu::WgpuSurfaceView>>; 3],
}

impl Viewer {
    pub fn stream(&self) -> &Stream {
        &self.inner.stream
    }

    /// Records the window the viewer is painted in: the window that gets a present for each
    /// new picture. Called by [`crate::VideoView`] when it paints.
    pub fn shown_in(&self, window: AnyWindowHandle) {
        let mut viewers = self.inner.stream.shared.viewers.lock();
        if let Some(entry) = viewers.iter_mut().find(|entry| entry.id == self.inner.id)
            && entry.window != Some(window)
        {
            entry.window = Some(window);
        }
    }

    #[cfg(vk)]
    pub(crate) fn surface_view(&self, fit: crate::Fit) -> Arc<gpui_wgpu::WgpuSurfaceView> {
        let (index, fit) = match fit {
            crate::Fit::Contain => (0, gpui_wgpu::SurfaceFit::Contain),
            crate::Fit::Cover => (1, gpui_wgpu::SurfaceFit::Cover),
            crate::Fit::Fill => (2, gpui_wgpu::SurfaceFit::Fill),
        };
        self.inner.views[index]
            .get_or_init(|| {
                Arc::new(gpui_wgpu::WgpuSurfaceView {
                    surface: self.inner.stream.shared.target.surface.clone(),
                    fit,
                })
            })
            .clone()
    }
}

impl Drop for ViewerInner {
    fn drop(&mut self) {
        let shared = &self.stream.shared;
        let (last, window) = {
            let mut viewers = shared.viewers.lock();
            let window = viewers
                .iter()
                .position(|entry| entry.id == self.id)
                .and_then(|index| viewers.swap_remove(index).window);
            shared.viewer_count.store(viewers.len() as u32, RELAXED);
            (viewers.is_empty(), window)
        };
        if let Some(window) = window {
            let mut released = self.stream.hub.released_windows.lock();
            if !released.contains(&window) {
                released.push(window);
            }
        }
        if last && !shared.removed.load(RELAXED) {
            // Nobody displays it: decode nothing. The thread leaves and frees its decoder.
            shared.queue.stop();
            shared.target.clear();
            let callbacks = self.stream.hub.callbacks.read().clone();
            (callbacks.interest)(&shared.key, false);
            self.stream
                .hub
                .release_pending
                .store(true, Ordering::Release);
            self.stream.hub.signal.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::dbv::StreamFile;

    /// `gpui-client/vendor/streams/<name>`, when the test streams were generated.
    fn stream_file(name: &str) -> Option<PathBuf> {
        let dir = std::env::var_os("DECIBELL_VIDEO_STREAMS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/streams")
            });
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!(
                "skipped: {} is missing (tools/streamgen/make-streams.sh)",
                path.display()
            );
            return None;
        }
        Some(path)
    }

    /// Pushes the file's next `count` frames as fast as the decoder takes them.
    fn push_frames(hub: &VideoHub, stream: &Stream, file: &mut StreamFile, count: usize) {
        for _ in 0..count {
            let header = file.next_frame().unwrap();
            let mut bytes = vec![0; header.size as usize];
            file.read(&mut bytes).unwrap();
            while stream.stats().queue >= 2 {
                std::thread::sleep(Duration::from_micros(200));
            }
            hub.push(
                stream.key(),
                EncodedFrame {
                    codec: file.codec,
                    keyframe: header.keyframe,
                    timestamp: 0,
                    data: Packet::from(bytes),
                    description: header.description,
                    discontinuity: false,
                },
            );
        }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn every_codec_decodes_every_frame_to_the_right_picture() {
        for name in ["1080p60_h264.dbv", "1080p60_hevc.dbv", "1080p60_av1.dbv"] {
            let Some(path) = stream_file(name) else {
                return;
            };
            let hub = VideoHub::headless();
            let stream = hub.stream(name);
            let viewer = stream.viewer();
            let mut file = StreamFile::open(&path).unwrap();
            push_frames(&hub, &stream, &mut file, 40);
            wait_for("40 pictures", || stream.stats().published == 40);
            let capture = stream.capture_luma_row(1.0 / 36.0);
            push_frames(&hub, &stream, &mut file, 20);
            wait_for("60 pictures", || stream.stats().published == 60);
            let row = capture.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!((row.width, row.height), (1920, 1080), "{name}");
            assert_eq!(
                crate::decode::barcode(&row.bytes) as u64,
                row.serial - 1,
                "{name}"
            );
            let stats = stream.stats();
            assert_eq!(
                (stats.received, stats.decoded, stats.dropped, stats.errors),
                (60, 60, 0, 0)
            );
            assert_eq!(stats.path, Some(DecodePath::Software));
            assert_eq!((stats.width, stats.height), (1920, 1080));
            assert_eq!(hub.decoder_threads(), 1);
            assert_eq!(hub.live_textures(), 1);
            // Nobody displays it any more: the thread leaves and its decoder with it.
            drop(viewer);
            wait_for("the decoder thread to exit", || hub.decoder_threads() == 0);
            assert_eq!(hub.live_textures(), 0);
            assert_eq!(stream.stats().path, None);
        }
    }

    /// HEVC as the streamer sends it: length-prefixed NAL units, the parameter sets only in the
    /// keyframes' description (hvcC).
    #[test]
    fn hevc_with_an_out_of_band_description_decodes() {
        let Some(path) = stream_file("1080p60_hevc.dbv") else {
            return;
        };
        let recording =
            std::env::temp_dir().join(format!("decibell-hvcc-{}.dbv", std::process::id()));
        crate::dbv::write_hvcc_recording(&path, &recording, 60).unwrap();
        let hub = VideoHub::headless();
        let stream = hub.stream("hvcc");
        let _viewer = stream.viewer();
        let mut file = StreamFile::open(&recording).unwrap();
        assert_eq!((file.version, file.codec), (2, 3));
        let capture = stream.capture_luma_row(1.0 / 36.0);
        push_frames(&hub, &stream, &mut file, 60);
        wait_for("60 pictures", || stream.stats().published == 60);
        std::fs::remove_file(&recording).ok();
        let row = capture.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(crate::decode::barcode(&row.bytes) as u64, row.serial - 1);
        let stats = stream.stats();
        assert_eq!(
            (stats.decoded, stats.errors, stats.decoder_opens),
            (60, 0, 1)
        );
        assert_eq!((stats.width, stats.height), (1920, 1080));
    }

    #[test]
    fn frames_of_a_stream_nobody_displays_are_dropped_without_a_thread() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let hub = VideoHub::headless();
        let stream = hub.stream("unwatched");
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 10);
        let stats = stream.stats();
        assert_eq!((stats.unwatched, stats.received, stats.decoded), (10, 0, 0));
        assert_eq!(hub.decoder_threads(), 0);
        // And frames for a key nobody created a stream for.
        hub.push(
            "nobody",
            EncodedFrame {
                codec: 1,
                keyframe: true,
                timestamp: 0,
                data: Packet::from(vec![0u8; 4]),
                description: None,
                discontinuity: false,
            },
        );
        assert_eq!(hub.unknown_frames(), 1);
    }

    #[test]
    fn interest_and_keyframe_requests_reach_the_callbacks() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let hub = VideoHub::headless();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        hub.set_callbacks(HubCallbacks {
            request_keyframe: Box::new({
                let log = log.clone();
                move |key| log.lock().push(format!("keyframe {key}"))
            }),
            interest: Box::new({
                let log = log.clone();
                move |key, on| log.lock().push(format!("interest {key} {on}"))
            }),
            event: Box::new({
                let log = log.clone();
                move |key, event| log.lock().push(format!("event {key} {event:?}"))
            }),
        });
        let stream = hub.stream("7");
        let viewer = stream.viewer();
        let second = stream.viewer();
        assert_eq!(*log.lock(), ["interest 7 true", "keyframe 7"]);

        // Joining mid-stream: deltas are refused until the keyframe that was asked for.
        let mut file = StreamFile::open(&path).unwrap();
        let first = file.next_frame().unwrap();
        let mut keyframe = vec![0; first.size as usize];
        file.read(&mut keyframe).unwrap();
        push_frames(&hub, &stream, &mut file, 5);
        assert_eq!(stream.stats().dropped, 5);
        assert_eq!(hub.decoder_threads(), 0);

        // A decoder error: the decoder is reopened at the next keyframe, which is asked for.
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 10);
        wait_for("10 pictures", || stream.stats().published == 10);
        stream.inject(Fault::DecodeError);
        push_frames(&hub, &stream, &mut file, 5);
        wait_for("the error", || stream.stats().errors == 1);
        let requests = stream.stats().keyframe_requests;
        assert!(requests >= 2, "{requests} keyframe requests");
        assert!(log.lock().iter().any(|line| line.contains("DecoderError")));
        // The streamer answers with a keyframe (the file's first frame again).
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 10);
        wait_for("pictures after the error", || {
            stream.stats().published == 20
        });
        assert_eq!(stream.stats().decoder_opens, 2);

        drop(viewer);
        assert!(!log.lock().iter().any(|line| line == "interest 7 false"));
        drop(second);
        assert_eq!(
            log.lock().last().map(String::as_str),
            Some("interest 7 false")
        );
    }

    #[test]
    fn a_removed_stream_stops_and_a_resolution_change_is_followed() {
        let Some(path) = stream_file("switch_hevc.dbv") else {
            return;
        };
        let hub = VideoHub::headless();
        let stream = hub.stream("switch");
        let _viewer = stream.viewer();
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 250);
        wait_for("250 pictures", || stream.stats().published == 250);
        let stats = stream.stats();
        // 240 frames of 1080p, then 1440p: one decoder throughout (the parameter sets are in
        // the bitstream).
        assert_eq!(
            (stats.width, stats.height, stats.decoder_opens, stats.errors),
            (2560, 1440, 1, 0)
        );
        hub.remove("switch");
        wait_for("the decoder thread to exit", || hub.decoder_threads() == 0);
        push_frames(&hub, &stream, &mut file, 3);
        assert_eq!(hub.unknown_frames(), 3);
    }
}
