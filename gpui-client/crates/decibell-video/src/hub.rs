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
//!
//! The same holds while every viewer of a stream sits in a window that isn't shown (minimised,
//! suspended by the compositor: [`VideoHub::set_window_visible`]), as `StreamVideoPlayer.tsx`
//! stops decoding while `document.hidden`: the stream is **paused**. Its decoder thread leaves,
//! the core stops delivering its frames (nothing is decrypted or copied for it), its last
//! picture stays for when the window comes back, and that return asks the streamer for a
//! keyframe and decodes from it. A pause is not a stall (no "Reconnecting…"), and the watch
//! itself goes on: the stream's sound, which the core plays apart from the video, never stops.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use futures::task::AtomicWaker;
use gpui::AnyWindowHandle;
use parking_lot::{Mutex, RwLock};

use crate::codec::{Codec, CodecSupport, DecodeBackend, DecodePath, SkippedBackend};
use crate::decode::{Backend, CaptureCell, DecodeError, Failure, FrameCx, LumaRow, PictureDecoder};
use crate::packet::Packet;
use crate::queue::{Description, FrameQueue, Item, Popped};
use crate::select::{FIRST_FRAMES, Order, Selector};
use crate::stats::{Counters, StreamStats};

const RELAXED: Ordering = Ordering::Relaxed;

/// A displayed stream that had a picture and gets no new one for this long has stalled
/// (`StreamVideoPlayer.tsx`'s `STALL_MS`: the "Reconnecting…" indicator): the views are told
/// ([`StreamEvent::Stalled`]), the streamer is asked for a keyframe, and the decoder thread exits
/// and frees its decoder; the next keyframe starts a new one ([`StreamEvent::Resumed`] with its
/// first picture). No timer watches for this: it is the decoder thread's wait for its next
/// frame, which costs nothing while frames arrive and wakes once when they stop.
pub const STALLED_AFTER: Duration = Duration::from_secs(3);
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
    DecoderOpened {
        codec: Codec,
        path: DecodePath,
        backend: DecodeBackend,
    },
    /// This decoder did not open or failed for the stream; the next one in the order is tried
    /// at the next keyframe (`BackendFailed` for each one given up, in order).
    BackendFailed {
        backend: DecodeBackend,
        reason: String,
    },
    /// Every hardware decoder failed for this stream; it continues in software. `reason` is
    /// the last hardware decoder's failure.
    FellBackToSoftware { reason: String },
    /// A decoder failed and was reopened.
    DecoderError { reason: String },
    /// The GPU device is gone; the stream resumes when the renderer has a new one.
    DeviceLost,
    /// The first picture of a decoder is up, or the stream's picture size changed: what a view
    /// waits for to take its spinner down, and the aspect ratio for its layout.
    Picture { width: u32, height: u32 },
    /// The stream had a picture and no new one came for [`STALLED_AFTER`] (the streamer's
    /// connection, ours, or a decoder that produces nothing): the last picture stays up and the
    /// views say "Reconnecting…". Sent once per stall.
    Stalled,
    /// A picture again after [`StreamEvent::Stalled`].
    Resumed,
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
    #[cfg(gpu)]
    pub surface: Arc<crate::surface::Surface>,
    pub counters: Arc<Counters>,
    /// A timed stream (`timed.rs`): its decoders give pictures out in presentation order.
    reorder: AtomicBool,
    /// A file whose pictures are 10 / 12-bit (`Stream::set_deep`).
    deep: AtomicBool,
}

impl StreamTarget {
    /// A target nothing displays: where the capability probe decodes its test frames.
    pub(crate) fn detached() -> StreamTarget {
        StreamTarget {
            #[cfg(gpu)]
            surface: crate::surface::Surface::new(),
            counters: Arc::new(Counters::new()),
            reorder: AtomicBool::new(false),
            deep: AtomicBool::new(false),
        }
    }

    /// Whether a decoder opened for this target reorders (a file) or is low-delay (a stream).
    pub(crate) fn reorder(&self) -> bool {
        self.reorder.load(RELAXED)
    }

    /// Whether the stream's pictures are deeper than 8 bits (a file; said by its demuxer).
    pub(crate) fn deep(&self) -> bool {
        self.deep.load(RELAXED)
    }

    pub(crate) fn set_reorder(&self, reorder: bool) {
        self.reorder.store(reorder, RELAXED);
    }

    pub(crate) fn clear(&self) {
        #[cfg(gpu)]
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
    pub(crate) queue: FrameQueue,
    /// `Some`: a timed stream (`timed.rs`).
    pub(crate) timing: RwLock<Option<Arc<dyn crate::timed::Timing>>>,
    pub(crate) target: StreamTarget,
    viewers: Mutex<Vec<ViewerEntry>>,
    viewer_count: AtomicU32,
    /// A picture was published since the presenter last looked.
    dirty: AtomicBool,
    last_keyframe_request_ms: AtomicU64,
    /// The decoders that failed for this stream (bit `DecodeBackend::index`): not tried for it
    /// again.
    denied: AtomicU8,
    /// 0 none, 1 decode error, 2 device loss, 3 hold the decoder thread for `hold_ms`.
    fault: AtomicU8,
    hold_ms: AtomicU32,
    /// A picture of this stream was published since its first viewer came.
    has_picture: AtomicBool,
    /// [`StreamEvent::Stalled`] was sent and no picture came since.
    stalled: AtomicBool,
    capture: CaptureCell,
    removed: AtomicBool,
    /// [`crate::Orientation`]'s bits: how the stream's pictures are turned when drawn.
    orientation: AtomicU8,
    /// Every viewer is in a hidden window: no decoder, no frames (see the module docs).
    paused: AtomicBool,
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

impl StreamShared {
    pub(crate) fn viewer_count(&self) -> u32 {
        self.viewer_count.load(RELAXED)
    }

    fn timing(&self) -> Option<Arc<dyn crate::timed::Timing>> {
        self.timing.read().clone()
    }

    fn is_timed(&self) -> bool {
        self.timing.read().is_some()
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
    pub(crate) selector: Selector,
    pub(crate) probe: crate::probe::ProbeState,
    started: Instant,
    unknown_frames: AtomicU64,
    presenter: Mutex<Option<gpui::Task<()>>>,
    /// Windows that aren't shown (minimised, suspended): their viewers don't count.
    hidden_windows: Mutex<Vec<AnyWindowHandle>>,
    /// Streams pause while only hidden windows view them (off: the old behaviour, for
    /// measurements).
    pause_hidden: AtomicBool,
}

/// The process's stream playback: see the module docs. Cheap to clone.
#[derive(Clone)]
pub struct VideoHub {
    pub(crate) inner: Arc<HubInner>,
}

struct GlobalHub(VideoHub);
impl gpui::Global for GlobalHub {}

/// The app's hub, for [`shutdown`]: reachable after GPUI's app is gone.
static PROCESS_HUB: std::sync::OnceLock<VideoHub> = std::sync::OnceLock::new();

/// Ends stream playback before the process exits: every stream is stopped and the decoder
/// threads (and a running capability probe) are waited for, up to `timeout`. Call it after
/// the event loop has ended and before `main` returns. A decoder thread frees its decoder
/// with calls into the GPU driver, and a driver that is already running its exit handlers on
/// the main thread crashes under them (seen: a segfault inside NVIDIA's driver when the app
/// quit with a 1440p120 stream playing). Returns whether everything had stopped in time.
pub fn shutdown(timeout: Duration) -> bool {
    let Some(hub) = PROCESS_HUB.get() else {
        return true;
    };
    let streams: Vec<Arc<StreamShared>> = hub.inner.streams.write().drain().map(|(_, s)| s).collect();
    for stream in &streams {
        stream.removed.store(true, RELAXED);
        stream.queue.stop();
        stream.target.clear();
        if let Some(timing) = stream.timing() {
            // A paused file's decoder thread is waiting for its clock.
            timing.shutdown();
        }
    }
    let started = Instant::now();
    while hub.inner.threads.load(RELAXED) > 0 || hub.inner.probe.is_running() {
        if started.elapsed() >= timeout {
            log::warn!(
                "video: {} decoder threads still running at exit",
                hub.inner.threads.load(RELAXED)
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
}

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
        let _ = PROCESS_HUB.set(hub.clone());
        hub
    }

    /// A hub that decodes in software into a counting sink and presents nothing: no window, no
    /// GPU, no presenter task. For tests and benchmarks of everything up to the picture.
    #[doc(hidden)]
    pub fn headless() -> VideoHub {
        VideoHub::with_backend(Arc::new(crate::cpu::CpuBackend::default()))
    }

    pub(crate) fn with_backend(backend: Arc<dyn Backend>) -> VideoHub {
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
                force_software: AtomicBool::new(false),
                selector: Selector::new(crate::select::forced_by_environment()),
                probe: Default::default(),
                started: Instant::now(),
                unknown_frames: AtomicU64::new(0),
                presenter: Mutex::new(None),
                hidden_windows: Mutex::new(Vec::new()),
                pause_hidden: AtomicBool::new(true),
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
                present_windows(cx, &windows);
                if released {
                    // A stream was torn down. Its last pictures are still referenced by the
                    // renderer: by the frames that sampled them, until it notices the GPU has
                    // finished those (it only looks when it draws), and by the bind groups it
                    // caches per picture, until its next frame. With no stream there may be no
                    // next frame for a long time, so: let the GPU finish, look, present once
                    // more (the bind groups go), look again. Two short one-shot timers per
                    // closed stream, and its textures are freed. In a task of its own: the
                    // other streams' pictures are presented meanwhile.
                    let windows = windows.clone();
                    let hub = hub.clone();
                    cx.spawn(async move |cx| {
                        let pause = Duration::from_millis(120);
                        for step in 0..2 {
                            cx.background_executor().timer(pause).await;
                            let Some(inner) = hub.upgrade() else { return };
                            inner.backend.collect();
                            if step == 0 {
                                present_windows(cx, &windows);
                            }
                        }
                    })
                    .detach();
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
                    #[cfg(gpu)]
                    surface: {
                        let surface = crate::surface::Surface::new();
                        let counters = counters.clone();
                        surface.set_draw_observer(Box::new(move |serial| {
                            counters.frame_presented(serial)
                        }));
                        surface
                    },
                    counters,
                    reorder: AtomicBool::new(false),
                    deep: AtomicBool::new(false),
                };
                Arc::new(StreamShared {
                    key: key.into(),
                    index: self.inner.next_index.fetch_add(1, RELAXED),
                    queue: FrameQueue::new(),
                    timing: RwLock::new(None),
                    target,
                    viewers: Mutex::new(Vec::new()),
                    viewer_count: AtomicU32::new(0),
                    dirty: AtomicBool::new(false),
                    last_keyframe_request_ms: AtomicU64::new(0),
                    denied: AtomicU8::new(0),
                    fault: AtomicU8::new(0),
                    hold_ms: AtomicU32::new(0),
                    has_picture: AtomicBool::new(false),
                    stalled: AtomicBool::new(false),
                    capture: CaptureCell::default(),
                    removed: AtomicBool::new(false),
                    orientation: AtomicU8::new(0),
                    paused: AtomicBool::new(false),
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
        if stream.viewer_count.load(RELAXED) == 0 || stream.paused.load(RELAXED) {
            // Not displayed, or only in hidden windows (frames already on their way when the
            // core was told).
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
                description: frame.description.map(Description::Owned),
                arrived: Instant::now(),
                epoch: 0,
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

    /// The window `window` is shown or not (minimised, suspended by the compositor: GPUI's
    /// `Window::is_visible`). A stream whose every viewer is in hidden windows pauses until one
    /// of them is shown again (see the module docs). Called by the app on each visibility
    /// change of its windows; costs nothing when it changes nothing.
    pub fn set_window_visible(&self, window: AnyWindowHandle, visible: bool) {
        {
            let mut hidden = self.inner.hidden_windows.lock();
            let at = hidden.iter().position(|w| *w == window);
            match (visible, at) {
                (true, Some(at)) => {
                    hidden.swap_remove(at);
                }
                (false, None) => hidden.push(window),
                _ => return,
            }
        }
        log::info!("video: window {:?} {}", window.window_id(), if visible { "shown" } else { "hidden" });
        self.inner.reconsider_all();
    }

    /// Whether streams pause while only hidden windows view them (on by default). Off is the
    /// behaviour before the pause existed, kept for measuring the difference (self-tests).
    #[doc(hidden)]
    pub fn set_pause_when_hidden(&self, on: bool) {
        if self.inner.pause_hidden.swap(on, RELAXED) != on {
            self.inner.reconsider_all();
        }
    }

    /// Decode every stream in software from its next decoder on (settings, tests).
    pub fn set_force_software(&self, force: bool) {
        if self.inner.force_software.swap(force, RELAXED) != force {
            self.inner.probe.forget();
        }
    }

    /// Replaces the order decoders are tried in (`None`: the platform's own again), as
    /// `DECIBELL_VIDEO_DECODER` does: exactly these are tried. For tests and settings.
    pub fn set_decoder_order(&self, order: Option<&[DecodeBackend]>) {
        self.inner.selector.force(order.map(Order::new));
        self.inner.probe.forget();
    }

    /// The order that is forced (by `DECIBELL_VIDEO_DECODER` or [`Self::set_decoder_order`]),
    /// if one is.
    pub fn forced_decoder_order(&self) -> Option<Vec<DecodeBackend>> {
        self.inner
            .selector
            .forced()
            .map(|order| order.as_slice().to_vec())
    }

    /// The decoders streams are opened with, in order, on this machine right now.
    pub fn decoder_order(&self) -> Vec<DecodeBackend> {
        self.inner.order().as_slice().to_vec()
    }

    /// Every decoder that was tried for a codec this session and given up, with the reason
    /// (a failed open, or a failure in its first pictures).
    pub fn skipped_decoders(&self) -> Vec<SkippedBackend> {
        self.inner.selector.report()
    }

    /// Whether 10 / 12-bit pictures are handed to the renderer in 16-bit planes here (else their
    /// 8 most significant bits are).
    pub fn deep_planes(&self) -> bool {
        self.inner.backend.deep_planes()
    }

    /// Why `backend` is not tried for `codec` any more this session, if it was given up (any
    /// codec, those only files use included).
    pub fn skip_reason(&self, codec: Codec, backend: DecodeBackend) -> Option<String> {
        self.inner
            .selector
            .skipped(codec, backend)
            .map(|reason| reason.to_string())
    }

    /// Forgets which decoders failed (tests).
    #[doc(hidden)]
    pub fn forget_decoder_failures(&self) {
        self.inner.selector.forget();
        self.inner.probe.forget();
    }

    /// What this machine does with each codec, as far as is known without decoding anything:
    /// the last probe's answer for this GPU and driver when there is one
    /// ([`Self::probe_capabilities`]; `probed` is then set), else the first decoder of the
    /// order that the driver and the FFmpeg build list for the codec. Never blocks. Hardware
    /// answers come from the renderer's GPU: before a window exists only the software decoders
    /// are reported.
    pub fn capabilities(&self) -> [CodecSupport; 3] {
        crate::probe::known(&self.inner)
    }

    /// The decode capabilities to advertise, from a real probe: for each codec the decoders
    /// are tried in order with a small test frame compiled into the binary, and the first that
    /// returns the right picture is the answer. Runs on a thread of its own, once per GPU and
    /// driver version (the result is kept, and with [`Self::set_capabilities_cache`] also
    /// across runs); `done` is called from that thread, or at once when the answer is known.
    /// Call it once a window exists.
    pub fn probe_capabilities(&self, done: impl FnOnce([CodecSupport; 3]) + Send + 'static) {
        crate::probe::start(&self.inner, Box::new(done));
    }

    /// The probe again, from nothing (Settings → Codecs' "Refresh codec capabilities", after
    /// a driver or GPU change the key did not catch): what was given up this session is
    /// forgotten, the kept answer and its file are discarded, every decoder is tried again.
    pub fn reprobe_capabilities(&self, done: impl FnOnce([CodecSupport; 3]) + Send + 'static) {
        self.inner.selector.forget();
        self.inner.probe.discard();
        crate::probe::start(&self.inner, Box::new(done));
    }

    /// A file to keep the probe's result in between runs (keyed by GPU and driver version: a
    /// driver update probes again). Without it every run probes once, lazily.
    pub fn set_capabilities_cache(&self, path: impl Into<std::path::PathBuf>) {
        self.inner.probe.set_cache_file(path.into());
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

/// One `request_present` per window; a closed window is simply skipped.
fn present_windows(cx: &mut gpui::AsyncApp, windows: &[AnyWindowHandle]) {
    cx.update(|cx| {
        for window in windows {
            let _ = window.update(cx, |_, window, _| window.request_present());
        }
    });
}

impl HubInner {
    /// Pause or resume every stream as its viewers' windows are now (see [`Self::reconsider`]).
    fn reconsider_all(&self) {
        let streams: Vec<Arc<StreamShared>> = self.streams.read().values().cloned().collect();
        for stream in streams {
            self.reconsider(&stream);
        }
    }

    /// A stream pauses while all its viewers are in hidden windows and resumes when one of them
    /// is shown again (or a viewer in a window not yet known comes). Timed streams (attachments)
    /// pause by their own player's `set_visible`.
    fn reconsider(&self, stream: &StreamShared) {
        if stream.is_timed() || stream.removed.load(RELAXED) {
            return;
        }
        let hide = self.pause_hidden.load(RELAXED) && {
            // Lock order: the hidden windows, then the stream's viewers (nothing takes them the
            // other way round).
            let hidden = self.hidden_windows.lock();
            let viewers = stream.viewers.lock();
            !viewers.is_empty() && viewers.iter().all(|v| v.window.is_some_and(|w| hidden.contains(&w)))
        };
        if hide {
            if stream.viewer_count.load(RELAXED) > 0 && !stream.paused.swap(true, Ordering::AcqRel) {
                log::info!("video: stream {}: paused (only hidden windows show it)", stream.key);
                // The decoder thread leaves (keeping the picture: see `decoder_thread`), the core
                // stops delivering.
                stream.queue.stop();
                let callbacks = self.callbacks.read().clone();
                (callbacks.interest)(&stream.key, false);
            }
        } else if stream.paused.swap(false, Ordering::AcqRel)
            && stream.viewer_count.load(RELAXED) > 0
        {
            log::info!("video: stream {}: resumed (shown again): asking for a keyframe", stream.key);
            // The reference chain broke while paused: decode from a fresh keyframe (the queue
            // gates deltas until one), as the renderer does on `visibilitychange`.
            stream.queue.start();
            let callbacks = self.callbacks.read().clone();
            (callbacks.interest)(&stream.key, true);
            stream.last_keyframe_request_ms.store(0, RELAXED);
            self.request_keyframe(stream);
        }
    }

    /// The decoders to try for a stream, in order.
    pub(crate) fn order(&self) -> Order {
        if self.force_software.load(RELAXED) {
            return Order::SOFTWARE;
        }
        self.selector.order(self.backend.order())
    }

    /// Somebody replaced the machine's own order (the environment, a test, a setting): what
    /// decodes now is not what the machine does by itself.
    pub(crate) fn order_is_forced(&self) -> bool {
        self.force_software.load(RELAXED) || self.selector.forced().is_some()
    }

    /// A decoder was given up for a codec: what the probe said (if it named that decoder) is
    /// wrong. Under a forced order only this run's answer is; the machine's own is in the file.
    fn probed_answer_is_stale(&self) {
        if self.order_is_forced() {
            self.probe.forget();
        } else {
            self.probe.invalidate();
        }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64 + 1
    }

    fn request_keyframe(&self, stream: &StreamShared) {
        if let Some(timing) = stream.timing() {
            // A file: its producer seeks; the core is not asked for anything.
            timing.keyframe_needed();
            return;
        }
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
        if let Some(timing) = stream.timing() {
            timing.event(event);
            return;
        }
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

    pub(crate) fn start_decoder(self: Arc<Self>, stream: Arc<StreamShared>) {
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
    /// Pictures the open decoder has published.
    pictures: u64,
    /// The last hardware decoder's failure, until a decoder opens after it.
    hardware_failure: Option<String>,
    /// When this thread last published a picture (or started): frames that arrive and give
    /// no picture for [`STALLED_AFTER`] are a stall too.
    last_picture: Instant,
    /// The epoch of the last packet (timed streams: a new one is a seek).
    epoch: u32,
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
        pictures: 0,
        hardware_failure: None,
        last_picture: Instant::now(),
        epoch: 0,
    };
    let stopped = loop {
        let timing = stream.timing();
        let popped = match &timing {
            Some(_) => stream.queue.pop_timed(STALLED_AFTER),
            None => stream.queue.pop(STALLED_AFTER),
        };
        match popped {
            Popped::Item(item) => {
                decode_one(&hub, &stream, &mut decoding, item, timing.as_deref());
                if timing.is_none() && decoding.last_picture.elapsed() >= STALLED_AFTER {
                    // Frames arrive and none becomes a picture.
                    stalled(&hub, &stream);
                }
            }
            Popped::Idle => {
                stalled(&hub, &stream);
                break false;
            }
            Popped::Quiet => {
                // A file whose producer has nothing to send: paused with an empty queue,
                // waiting for the network, or at its end.
                if timing.is_none_or(|timing| timing.idle()) && stream.queue.leave() {
                    break false;
                }
            }
            Popped::Stopped => break true,
        }
    };
    // The decoder's slots go with it; what the surface still shows stays until it is cleared.
    drop(decoding);
    counters.clear_decoder();
    counters.presentation_paused();
    // A pause keeps the last picture for when the window is shown again.
    if stopped && !stream.paused.load(Ordering::Acquire) {
        stream.target.clear();
        hub.release(&stream);
    }
    counters.dump_trace(&stream.key);
    hub.threads.fetch_sub(1, RELAXED);
}

/// No picture for [`STALLED_AFTER`]: say so once (only a stream that had a picture stalls;
/// one that never had one shows its spinner) and ask the streamer for a keyframe, which a live
/// streamer answers at once (the renderer's "ping", its liveness probe).
fn stalled(hub: &HubInner, stream: &StreamShared) {
    if stream.is_timed() {
        return;
    }
    if stream.has_picture.load(RELAXED)
        && stream.viewer_count.load(RELAXED) > 0
        && !stream.removed.load(RELAXED)
        && !stream.paused.load(RELAXED)
        && !stream.stalled.swap(true, Ordering::AcqRel)
    {
        hub.event(stream, StreamEvent::Stalled);
        stream.last_keyframe_request_ms.store(0, RELAXED);
        hub.request_keyframe(stream);
    }
}

fn decode_one(
    hub: &HubInner,
    stream: &StreamShared,
    decoding: &mut Decoding,
    item: Item,
    timing: Option<&dyn crate::timed::Timing>,
) {
    let counters = &stream.target.counters;
    let Item {
        packet,
        codec,
        keyframe,
        timestamp,
        description,
        arrived,
        epoch,
    } = item;

    if epoch != decoding.epoch {
        // A seek in a file: the decoder forgets the old position and starts on this keyframe
        // (kept open when it can flush; a packet that is not a keyframe finds no decoder below
        // and is refused there).
        decoding.epoch = epoch;
        let kept = keyframe
            && decoding
                .decoder
                .as_mut()
                .is_some_and(|decoder| decoder.flush());
        if !kept && decoding.decoder.is_some() {
            decoding.decoder = None;
            counters.clear_decoder();
        }
    }

    let fault = stream.fault.swap(0, RELAXED);
    if fault == 2 {
        hub.backend.simulate_device_loss();
    }
    if fault == 3 {
        // The test hook: this thread does not run for a while, with a frame in hand.
        std::thread::sleep(Duration::from_millis(stream.hold_ms.load(RELAXED) as u64));
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
        let decoder = match open_decoder(hub, stream, decoding, codec, description.as_deref()) {
            Ok(decoder) => decoder,
            Err(error) => {
                counters.shed.fetch_add(1, RELAXED);
                failed(hub, stream, decoding, None, error);
                return;
            }
        };
        counters.opens.fetch_add(1, RELAXED);
        counters.set_decoder(codec, Some(decoder.backend()));
        hub.event(
            stream,
            StreamEvent::DecoderOpened {
                codec,
                path: decoder.path(),
                backend: decoder.backend(),
            },
        );
        if let Some(reason) = decoding.hardware_failure.take()
            && decoder.backend() == DecodeBackend::Software
        {
            hub.event(stream, StreamEvent::FellBackToSoftware { reason });
        }
        decoding.pictures = 0;
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
        timing: timing.map(|timing| (timing, epoch)),
    };
    let backend = decoder.backend();
    let result = if fault == 1 {
        drop(packet);
        Err(DecodeError::decoder("injected decode error"))
    } else {
        decoder.decode(packet, keyframe, timestamp, &cx)
    };
    match result {
        Ok(0) => {}
        Ok(_) => {
            decoding.pictures += 1;
            decoding.last_picture = counters.frame_published(serial, arrived, handed);
            stream.dirty.store(true, Ordering::Release);
            hub.signal.notify();
            if !stream.has_picture.load(RELAXED) {
                stream.has_picture.store(true, RELAXED);
            }
            if stream.stalled.load(RELAXED) && stream.stalled.swap(false, Ordering::AcqRel) {
                hub.event(stream, StreamEvent::Resumed);
            }
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
        Err(error) => failed(hub, stream, decoding, Some(backend), error),
    }
}

/// Opens the first decoder of the order that this stream may use and that opens. One that does
/// not open is given up for the codec, for every stream (`select.rs`).
fn open_decoder(
    hub: &HubInner,
    stream: &StreamShared,
    decoding: &mut Decoding,
    codec: Codec,
    description: Option<&[u8]>,
) -> Result<Box<dyn PictureDecoder>, DecodeError> {
    let counters = &stream.target.counters;
    let order = hub.order();
    let denied = stream.denied.load(RELAXED);
    let mut last_reason: Option<String> = None;
    for &backend in order.as_slice() {
        if denied & (1 << backend.index()) != 0 {
            continue;
        }
        if backend.path() == DecodePath::Hardware && !codec.has_hardware_path() {
            // A codec only files use: software, without a word about the hardware decoders.
            continue;
        }
        if let Some(reason) = hub.selector.skipped(codec, backend) {
            last_reason = Some(format!("{}: {reason}", backend.label()));
            continue;
        }
        if backend.path() == DecodePath::Hardware && !codec.is_stream_codec() {
            // A codec only files use is not in the start-up probe: its test frame is decoded on
            // this decoder the first time a file needs it.
            match crate::probe::file_codec_works(hub, backend, codec) {
                Some(true) => {}
                Some(false) => {
                    last_reason = hub
                        .selector
                        .skipped(codec, backend)
                        .map(|reason| format!("{}: {reason}", backend.label()));
                    continue;
                }
                None => return Err(DecodeError::device_lost("the renderer's GPU device was lost")),
            }
        }
        let started = Instant::now();
        match hub.backend.open(backend, codec, description, &stream.target) {
            Ok(decoder) => {
                log::info!(
                    "video: stream {}: {} decoder for {} opened in {:.1} ms",
                    stream.key,
                    backend.label(),
                    codec.label(),
                    started.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(decoder);
            }
            Err(error) if error.failure == Failure::DeviceLost => return Err(error),
            Err(error) => {
                // A 10-bit file's failure says nothing about the codec's 8-bit streams.
                if !stream.target.deep() && hub.selector.skip(codec, backend, &error.message) {
                    hub.probed_answer_is_stale();
                }
                counters.errors.fetch_add(1, RELAXED);
                gave_up(hub, stream, decoding, backend, error.message.clone());
                last_reason = Some(format!("{}: {}", backend.label(), error.message));
            }
        }
    }
    Err(DecodeError::decoder(match last_reason {
        Some(reason) => format!("no decoder is left for this stream (last: {reason})"),
        None => "no decoder is left for this stream".into(),
    }))
}

/// `backend` is not used for this stream any more: count it, say so, remember why.
fn gave_up(
    hub: &HubInner,
    stream: &StreamShared,
    decoding: &mut Decoding,
    backend: DecodeBackend,
    reason: String,
) {
    let counters = &stream.target.counters;
    counters.fallbacks.fetch_add(1, RELAXED);
    counters.set_fallback_reason(backend, &reason);
    if backend.path() == DecodePath::Hardware {
        decoding.hardware_failure = Some(reason.clone());
    }
    hub.event(stream, StreamEvent::BackendFailed { backend, reason });
}

/// A decoder failed (`backend`), or none could be opened (`None`): drop it, decide what the
/// next one is, start again at a keyframe. A hardware decoder that failed is not used for this
/// stream again, and if it failed within its first pictures, for the codec by nobody.
fn failed(
    hub: &HubInner,
    stream: &StreamShared,
    decoding: &mut Decoding,
    backend: Option<DecodeBackend>,
    error: DecodeError,
) {
    let counters = &stream.target.counters;
    counters.errors.fetch_add(1, RELAXED);
    log::warn!(
        "video: stream {} ({} decode): {error}",
        stream.key,
        backend.map_or("no", DecodeBackend::label)
    );
    let codec = decoding.codec;
    let pictures = std::mem::take(&mut decoding.pictures);
    decoding.decoder = None;
    decoding.codec = None;
    counters.clear_decoder();
    counters.presentation_paused();
    match (error.failure, backend) {
        (Failure::DeviceLost, _) => {
            counters.device_losses.fetch_add(1, RELAXED);
            stream.target.clear();
            hub.event(stream, StreamEvent::DeviceLost);
            // The renderer finds out about its device at its next frame.
            stream.dirty.store(true, Ordering::Release);
            hub.signal.notify();
        }
        (Failure::Decoder, Some(backend)) if backend.path() == DecodePath::Hardware => {
            stream.denied.fetch_or(1 << backend.index(), RELAXED);
            if pictures < FIRST_FRAMES
                && !stream.target.deep()
                && let Some(codec) = codec
                && hub.selector.skip(codec, backend, &error.message)
            {
                hub.probed_answer_is_stale();
            }
            gave_up(hub, stream, decoding, backend, error.message);
        }
        (Failure::Decoder, _) => hub.event(
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
            // A stream that comes back on screen waits for its own first picture.
            self.shared.has_picture.store(false, RELAXED);
            self.shared.stalled.store(false, RELAXED);
            self.shared.queue.start();
            if !self.shared.is_timed() {
                let callbacks = self.hub.callbacks.read().clone();
                (callbacks.interest)(&self.shared.key, true);
                self.shared.last_keyframe_request_ms.store(0, RELAXED);
                self.hub.request_keyframe(&self.shared);
            }
        }
        if self.shared.paused.load(RELAXED) {
            // A viewer in a window not known yet counts as shown.
            self.hub.reconsider(&self.shared);
        }
        Viewer {
            inner: Arc::new(ViewerInner {
                stream: self.clone(),
                id,
                #[cfg(gpu)]
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

    /// The newest picture's size as it is drawn (turned by [`Self::set_orientation`]), when
    /// there is one.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        #[cfg(gpu)]
        return self.shared.target.surface.display_size();
        #[cfg(not(gpu))]
        None
    }

    /// Turns the stream's pictures when they are drawn (a file's display matrix,
    /// `orient.rs`): in the renderer's draw of the surface, from its next draw on. A live
    /// stream is never turned.
    pub fn set_orientation(&self, orientation: crate::Orientation) {
        self.shared.orientation.store(orientation.bits(), RELAXED);
        #[cfg(gpu)]
        self.shared
            .target
            .surface
            .set_orientation(crate::surface::SurfaceOrientation(orientation.bits()));
    }

    /// The picture the stream shows now, kept: a [`crate::Still`] goes on showing it after the
    /// stream is removed (a played file's last picture as its card's poster). No pixel is read
    /// back or copied: the still holds the slot the picture is in (its two plane textures stay
    /// alive with it, a 1080p picture's 3 MB of GPU memory) and draws it like the stream did,
    /// turned the same way. `None`: there is no picture.
    pub fn still(&self) -> Option<crate::Still> {
        #[cfg(gpu)]
        {
            let frame = self.shared.target.surface.latest_frame()?;
            let size = self.picture_size()?;
            let surface = crate::surface::Surface::new();
            surface.set_orientation(crate::surface::SurfaceOrientation(self.orientation().bits()));
            surface.set_frame(frame, 0);
            Some(crate::Still { surface, size })
        }
        #[cfg(not(gpu))]
        None
    }

    /// Says that the stream's pictures are 10 / 12-bit (a file; before its first packet). The
    /// hardware decoders are then asked for the codec's 10-bit profile, and one that fails is
    /// given up for this stream only: it may well decode the codec's 8-bit streams.
    pub fn set_deep(&self, deep: bool) {
        self.shared.target.deep.store(deep, RELAXED);
    }

    pub fn orientation(&self) -> crate::Orientation {
        crate::Orientation::from_bits(self.shared.orientation.load(RELAXED))
    }

    /// Whether the stream has a decoder thread right now.
    pub fn is_decoding(&self) -> bool {
        self.shared.queue.has_consumer()
    }

    /// Decode this stream in software from its next decoder on (`false`: with every decoder
    /// of the order again, also those that failed for it).
    pub fn set_software_only(&self, software: bool) {
        let hardware = DecodeBackend::ALL
            .into_iter()
            .filter(|backend| backend.path() == DecodePath::Hardware)
            .fold(0, |bits, backend| bits | 1 << backend.index());
        self.shared
            .denied
            .store(if software { hardware } else { 0 }, RELAXED);
    }

    /// Whether the stream had a picture and has had no new one for [`STALLED_AFTER`].
    pub fn is_stalled(&self) -> bool {
        self.shared.stalled.load(RELAXED)
    }

    /// Makes the stream's decoder thread sleep for `gap` with its next frame in hand, as a
    /// thread the scheduler holds back does (tests of the queue's depth).
    #[doc(hidden)]
    pub fn hold_decoder(&self, gap: Duration) {
        self.shared
            .hold_ms
            .store(gap.as_millis() as u32, RELAXED);
        self.shared.fault.store(3, RELAXED);
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
    #[cfg(gpu)]
    views: [std::sync::OnceLock<Arc<crate::surface::SurfaceView>>; 3],
}

impl Viewer {
    pub fn stream(&self) -> &Stream {
        &self.inner.stream
    }

    /// Records the window the viewer is painted in: the window that gets a present for each
    /// new picture. Called by [`crate::VideoView`] when it paints.
    pub fn shown_in(&self, window: AnyWindowHandle) {
        let changed = {
            let mut viewers = self.inner.stream.shared.viewers.lock();
            match viewers.iter_mut().find(|entry| entry.id == self.inner.id) {
                Some(entry) if entry.window != Some(window) => {
                    entry.window = Some(window);
                    true
                }
                _ => false,
            }
        };
        if changed && !self.inner.stream.hub.hidden_windows.lock().is_empty() {
            // Its window is known now: it may be a hidden one.
            self.inner.stream.hub.reconsider(&self.inner.stream.shared);
        }
    }

    #[cfg(gpu)]
    pub(crate) fn surface_view(&self, fit: crate::Fit) -> Arc<crate::surface::SurfaceView> {
        use crate::surface::SurfaceFit;
        let (index, fit) = match fit {
            crate::Fit::Contain => (0, SurfaceFit::Contain),
            crate::Fit::Cover => (1, SurfaceFit::Cover),
            crate::Fit::Fill => (2, SurfaceFit::Fill),
        };
        self.inner.views[index]
            .get_or_init(|| {
                Arc::new(crate::surface::SurfaceView {
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
            shared.paused.store(false, Ordering::Release);
            shared.queue.stop();
            shared.target.clear();
            if !shared.is_timed() {
                let callbacks = self.stream.hub.callbacks.read().clone();
                (callbacks.interest)(&shared.key, false);
            }
            self.stream
                .hub
                .release_pending
                .store(true, Ordering::Release);
            self.stream.hub.signal.notify();
        } else if !last && !self.stream.hub.hidden_windows.lock().is_empty() {
            // The viewers left may all be in hidden windows.
            self.stream.hub.reconsider(shared);
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
            assert_eq!(stats.backend, Some(DecodeBackend::Software));
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

    struct NoView;
    impl gpui::Render for NoView {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            gpui::Empty
        }
    }

    fn window(id: u64) -> AnyWindowHandle {
        gpui::WindowHandle::<NoView>::new(gpui::WindowId::from(id)).into()
    }

    /// `StreamVideoPlayer.tsx` stops decoding while `document.hidden` and asks for a keyframe when
    /// shown again: a stream whose viewers are all in hidden windows pauses (no decoder, the core
    /// told to stop delivering, the last picture kept, no stall), and resumes from a keyframe.
    #[test]
    fn a_stream_shown_only_in_a_hidden_window_pauses_and_resumes_from_a_keyframe() {
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
        let (main, other) = (window(1), window(2));
        let stream = hub.stream("9");
        let viewer = stream.viewer();
        viewer.shown_in(main);
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 10);
        wait_for("10 pictures", || stream.stats().published == 10);
        assert_eq!(hub.decoder_threads(), 1);
        log.lock().clear();

        // Another window hidden: nothing changes.
        hub.set_window_visible(other, false);
        assert!(log.lock().is_empty());
        // Ours hidden: the stream pauses.
        hub.set_window_visible(main, false);
        assert_eq!(*log.lock(), ["interest 9 false"]);
        wait_for("the decoder thread to leave", || hub.decoder_threads() == 0);
        // (The headless backend counts open decoders: the GPU one keeps the published picture,
        // which a pause doesn't clear: `decoder_thread`.)
        assert_eq!(hub.live_textures(), 0, "the decoder is freed while hidden");
        let before = stream.stats();
        push_frames(&hub, &stream, &mut file, 5);
        let after = stream.stats();
        assert_eq!((after.received, after.decoded), (before.received, before.decoded), "frames decoded while hidden");
        assert_eq!(after.unwatched - before.unwatched, 5);
        assert_eq!(hub.decoder_threads(), 0);
        assert!(!stream.is_stalled());

        // A viewer whose window isn't known yet counts as shown, and while it lives the stream
        // plays; when it goes the stream pauses again.
        let elsewhere = stream.viewer();
        assert_eq!(log.lock().last().map(String::as_str), Some("keyframe 9"));
        drop(elsewhere);
        assert_eq!(log.lock().last().map(String::as_str), Some("interest 9 false"));
        log.lock().clear();

        // Shown again: the core delivers again, a keyframe is asked for at once (whatever the
        // throttle), and deltas wait for it.
        hub.set_window_visible(main, true);
        assert_eq!(*log.lock(), ["interest 9 true", "keyframe 9"]);
        let gated = stream.stats().dropped;
        push_frames(&hub, &stream, &mut file, 3);
        assert_eq!(stream.stats().dropped - gated, 3, "deltas before the keyframe");
        let published = stream.stats().published;
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 10);
        wait_for("pictures after the return", || stream.stats().published == published + 10);
        assert!(!log.lock().iter().any(|line| line.contains("Stalled")), "a pause is not a stall: {:?}", log.lock());

        // The measurement switch: off, a hidden window decodes as before.
        hub.set_pause_when_hidden(false);
        hub.set_window_visible(main, false);
        assert_eq!(hub.decoder_threads(), 1);
        hub.set_pause_when_hidden(true);
        wait_for("the decoder thread to leave", || hub.decoder_threads() == 0);
        drop(viewer);
        assert_eq!(log.lock().last().map(String::as_str), Some("interest 9 false"));
    }

    /// A platform with scripted hardware decoders (software decode underneath, so pictures are
    /// real): what a machine whose Vulkan Video or VA-API misbehaves looks like to the hub.
    struct Scripted {
        software: crate::cpu::CpuBackend,
        order: Vec<DecodeBackend>,
        /// Per decoder: `None` works; `Some(Err(reason))` does not open; `Some(Ok(n))` fails
        /// after publishing n pictures.
        script: Mutex<HashMap<DecodeBackend, Result<u64, &'static str>>>,
        opens: Mutex<Vec<DecodeBackend>>,
    }

    impl Scripted {
        fn new(
            order: &[DecodeBackend],
            script: &[(DecodeBackend, Result<u64, &'static str>)],
        ) -> Arc<Self> {
            Arc::new(Self {
                software: Default::default(),
                order: order.to_vec(),
                script: Mutex::new(script.iter().cloned().collect()),
                opens: Mutex::default(),
            })
        }

        fn opens(&self, backend: DecodeBackend) -> usize {
            self.opens.lock().iter().filter(|b| **b == backend).count()
        }
    }

    struct ScriptedDecoder {
        inner: Box<dyn PictureDecoder>,
        backend: DecodeBackend,
        fails_after: Option<u64>,
        pictures: u64,
    }

    impl PictureDecoder for ScriptedDecoder {
        fn backend(&self) -> DecodeBackend {
            self.backend
        }

        fn decode(
            &mut self,
            packet: Packet,
            keyframe: bool,
            pts: i64,
            cx: &FrameCx<'_>,
        ) -> crate::decode::DecodeResult<u32> {
            if self.fails_after.is_some_and(|limit| self.pictures >= limit) {
                return Err(DecodeError::decoder("scripted decode failure"));
            }
            let published = self.inner.decode(packet, keyframe, pts, cx)?;
            self.pictures += published as u64;
            Ok(published)
        }
    }

    impl Backend for Scripted {
        fn order(&self) -> Order {
            Order::new(&self.order)
        }

        fn open(
            &self,
            backend: DecodeBackend,
            codec: Codec,
            description: Option<&[u8]>,
            target: &StreamTarget,
        ) -> crate::decode::DecodeResult<Box<dyn PictureDecoder>> {
            self.opens.lock().push(backend);
            let script = self.script.lock().get(&backend).cloned();
            if let Some(Err(reason)) = script {
                return Err(DecodeError::decoder(reason));
            }
            Ok(Box::new(ScriptedDecoder {
                inner: self.software.open(
                    DecodeBackend::Software,
                    codec,
                    description,
                    target,
                )?,
                backend,
                fails_after: script.and_then(Result::ok),
                pictures: 0,
            }))
        }

        fn ready(&self) -> bool {
            true
        }

        fn simulate_device_loss(&self) {}

        fn support(&self, backend: DecodeBackend, codec: Codec) -> CodecSupport {
            CodecSupport {
                backend: Some(backend),
                path: Some(backend.path()),
                ..self.software.support(DecodeBackend::Software, codec)
            }
        }

        fn device_key(&self) -> Option<String> {
            Some("scripted".into())
        }

        fn live_textures(&self) -> i64 {
            self.software.live_textures()
        }

        fn collect(&self) {}
    }

    /// Plays the file's first `count` frames (it starts on a keyframe, as a streamer's answer
    /// to a keyframe request does) and waits until the decoder has taken them all.
    fn play(hub: &VideoHub, stream: &Stream, path: &std::path::Path, count: usize) {
        let mut file = StreamFile::open(path).unwrap();
        push_frames(hub, stream, &mut file, count);
        // Until nothing moves any more: every frame was decoded, shed or lost to an error.
        let mut last = stream.stats();
        let mut quiet = Instant::now();
        wait_for("the frames to be decoded or shed", || {
            let now = stream.stats();
            if now != last {
                last = now;
                quiet = Instant::now();
            }
            last.queue == 0 && quiet.elapsed() > Duration::from_millis(60)
        });
    }

    const AMD: [DecodeBackend; 3] = [
        DecodeBackend::VulkanVideo,
        DecodeBackend::Vaapi,
        DecodeBackend::Software,
    ];

    #[test]
    fn a_decoder_that_does_not_open_is_tried_once_and_the_next_one_plays() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let backend = Scripted::new(
            &AMD,
            &[(DecodeBackend::VulkanVideo, Err("no video decode queue"))],
        );
        let hub = VideoHub::with_backend(backend.clone());
        let events: Arc<Mutex<Vec<StreamEvent>>> = Arc::default();
        hub.set_callbacks(HubCallbacks {
            event: Box::new({
                let events = events.clone();
                move |_, event| events.lock().push(event)
            }),
            ..Default::default()
        });
        let stream = hub.stream("a");
        let _viewer = stream.viewer();
        play(&hub, &stream, &path, 10);
        let stats = stream.stats();
        // The keyframe that found Vulkan Video broken opened VA-API: nothing was lost.
        assert_eq!(
            (stats.backend, stats.decoded, stats.dropped, stats.fallbacks),
            (Some(DecodeBackend::Vaapi), 10, 0, 1)
        );
        assert_eq!(
            stats.fallback_reason.as_deref(),
            Some("vulkan: no video decode queue")
        );
        assert_eq!(
            events.lock()[..2],
            [
                StreamEvent::BackendFailed {
                    backend: DecodeBackend::VulkanVideo,
                    reason: "no video decode queue".into()
                },
                StreamEvent::DecoderOpened {
                    codec: Codec::H264,
                    path: DecodePath::Hardware,
                    backend: DecodeBackend::Vaapi
                },
            ]
        );
        let skipped = hub.skipped_decoders();
        assert_eq!(skipped.len(), 1);
        assert_eq!(
            (skipped[0].codec, skipped[0].backend, &*skipped[0].reason),
            (Codec::H264, DecodeBackend::VulkanVideo, "no video decode queue")
        );
        // What the client advertises follows: VA-API for H.264, and the other codecs still
        // listed with the first decoder (nobody tried them).
        let support = hub.capabilities();
        assert_eq!(support[0].backend, Some(DecodeBackend::Vaapi));
        assert_eq!(support[1].backend, Some(DecodeBackend::VulkanVideo));

        // Another stream of the codec does not try it again.
        let other = hub.stream("b");
        let _other_viewer = other.viewer();
        play(&hub, &other, &path, 5);
        assert_eq!(other.stats().backend, Some(DecodeBackend::Vaapi));
        assert_eq!(other.stats().fallbacks, 0);
        assert_eq!(backend.opens(DecodeBackend::VulkanVideo), 1);
    }

    #[test]
    fn a_decoder_that_fails_in_its_first_pictures_is_given_up_for_the_session() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let backend = Scripted::new(&AMD, &[(DecodeBackend::VulkanVideo, Ok(5))]);
        let hub = VideoHub::with_backend(backend.clone());
        let stream = hub.stream("a");
        let _viewer = stream.viewer();
        play(&hub, &stream, &path, 12);
        let stats = stream.stats();
        // Five pictures, then the failure; the rest of the chain is shed until a keyframe.
        assert_eq!((stats.decoded, stats.errors, stats.fallbacks), (5, 1, 1));
        assert_eq!(stats.backend, None);
        assert!(stats.keyframe_requests >= 2);
        assert_eq!(
            stats.fallback_reason.as_deref(),
            Some("vulkan: scripted decode failure")
        );
        assert_eq!(hub.skipped_decoders().len(), 1);
        // The streamer's keyframe: the next decoder of the order.
        play(&hub, &stream, &path, 10);
        let stats = stream.stats();
        assert_eq!((stats.backend, stats.decoded), (Some(DecodeBackend::Vaapi), 15));
        // And nobody tries Vulkan Video for H.264 again.
        let other = hub.stream("b");
        let _other_viewer = other.viewer();
        play(&hub, &other, &path, 5);
        assert_eq!(other.stats().backend, Some(DecodeBackend::Vaapi));
        assert_eq!(backend.opens(DecodeBackend::VulkanVideo), 1);
    }

    #[test]
    fn a_late_failure_costs_only_the_stream_it_happened_on() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let backend = Scripted::new(
            &AMD,
            &[(DecodeBackend::VulkanVideo, Ok(crate::select::FIRST_FRAMES + 10))],
        );
        let hub = VideoHub::with_backend(backend.clone());
        let stream = hub.stream("a");
        let _viewer = stream.viewer();
        play(&hub, &stream, &path, 50);
        assert_eq!(stream.stats().fallbacks, 1);
        assert!(hub.skipped_decoders().is_empty());
        play(&hub, &stream, &path, 10);
        assert_eq!(stream.stats().backend, Some(DecodeBackend::Vaapi));
        // Another stream starts on Vulkan Video again.
        let other = hub.stream("b");
        let _other_viewer = other.viewer();
        play(&hub, &other, &path, 5);
        assert_eq!(other.stats().backend, Some(DecodeBackend::VulkanVideo));
        // The stream it failed on is given it back when asked (`set_software_only(false)`).
        stream.set_software_only(false);
        hub.remove("b");
    }

    #[test]
    fn the_walk_ends_on_software_and_never_goes_back() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let backend = Scripted::new(
            &AMD,
            &[
                (DecodeBackend::VulkanVideo, Ok(3)),
                (DecodeBackend::Vaapi, Err("vaInitialize failed")),
            ],
        );
        let hub = VideoHub::with_backend(backend.clone());
        let events: Arc<Mutex<Vec<StreamEvent>>> = Arc::default();
        hub.set_callbacks(HubCallbacks {
            event: Box::new({
                let events = events.clone();
                move |_, event| events.lock().push(event)
            }),
            ..Default::default()
        });
        let stream = hub.stream("a");
        let _viewer = stream.viewer();
        play(&hub, &stream, &path, 8);
        play(&hub, &stream, &path, 8);
        let stats = stream.stats();
        assert_eq!(
            (stats.backend, stats.fallbacks, stats.decoded),
            (Some(DecodeBackend::Software), 2, 3 + 8)
        );
        assert_eq!(
            stats.fallback_reason.as_deref(),
            Some("vaapi: vaInitialize failed")
        );
        assert!(events.lock().contains(&StreamEvent::FellBackToSoftware {
            reason: "vaInitialize failed".into()
        }));
        // More keyframes, a software decoder error: the hardware decoders are not tried again.
        stream.inject(Fault::DecodeError);
        play(&hub, &stream, &path, 4);
        play(&hub, &stream, &path, 4);
        assert_eq!(stream.stats().backend, Some(DecodeBackend::Software));
        assert_eq!(backend.opens(DecodeBackend::VulkanVideo), 1);
        assert_eq!(backend.opens(DecodeBackend::Vaapi), 1);
        assert_eq!(backend.opens(DecodeBackend::Software), 2);
    }

    #[test]
    fn a_forced_decoder_that_does_not_work_is_not_replaced_and_not_retried() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let backend = Scripted::new(&AMD, &[(DecodeBackend::Vaapi, Err("no VA driver"))]);
        let hub = VideoHub::with_backend(backend.clone());
        hub.set_decoder_order(Some(&[DecodeBackend::Vaapi]));
        assert_eq!(hub.decoder_order(), [DecodeBackend::Vaapi]);
        let stream = hub.stream("a");
        let _viewer = stream.viewer();
        for _ in 0..3 {
            play(&hub, &stream, &path, 3);
        }
        let stats = stream.stats();
        assert_eq!((stats.backend, stats.decoded), (None, 0));
        assert!(stats.errors >= 3);
        assert_eq!(backend.opens(DecodeBackend::Vaapi), 1);
        assert_eq!(backend.opens(DecodeBackend::Software), 0);
        assert_eq!(hub.capabilities()[0].path, None);
        // Back to the machine's own order: Vulkan Video.
        hub.set_decoder_order(None);
        play(&hub, &stream, &path, 3);
        assert_eq!(stream.stats().backend, Some(DecodeBackend::VulkanVideo));
    }

    /// The probe on a machine where a listed decoder decodes nothing: the answer is the
    /// decoder that returned the test picture, and the broken one is on record.
    #[test]
    fn the_probe_reports_the_first_decoder_that_really_decodes() {
        let backend = Scripted::new(
            &AMD,
            &[
                (DecodeBackend::VulkanVideo, Ok(0)),
                (DecodeBackend::Vaapi, Err("no VA driver")),
            ],
        );
        let hub = VideoHub::with_backend(backend.clone());
        // Listed, before anything was tried.
        assert_eq!(hub.capabilities()[0].backend, Some(DecodeBackend::VulkanVideo));
        assert!(!hub.capabilities()[0].probed);
        let (sender, receiver) = mpsc::channel();
        hub.probe_capabilities(move |result| sender.send(result).unwrap());
        let result = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
        for support in result {
            assert_eq!(support.backend, Some(DecodeBackend::Software));
            assert_eq!(support.path, Some(DecodePath::Software));
            assert!(support.probed);
        }
        assert_eq!(hub.capabilities(), result);
        assert_eq!(hub.skipped_decoders().len(), 6);
        // Asked again: the kept answer, nothing opened.
        let opens = backend.opens.lock().len();
        let (sender, receiver) = mpsc::channel();
        hub.probe_capabilities(move |result| sender.send(result).unwrap());
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), result);
        assert_eq!(backend.opens.lock().len(), opens);
        // Streams never try what the probe found broken.
        if let Some(path) = stream_file("1080p60_h264.dbv") {
            let stream = hub.stream("a");
            let _viewer = stream.viewer();
            play(&hub, &stream, &path, 5);
            assert_eq!(stream.stats().backend, Some(DecodeBackend::Software));
            assert_eq!(stream.stats().fallbacks, 0);
            assert_eq!(backend.opens(DecodeBackend::VulkanVideo), 3);
        }
    }

    /// The decoder thread held back for less than the queue's depth loses nothing and asks for
    /// nothing; held back for longer it sheds and asks for a keyframe, as it always did.
    #[test]
    fn a_short_starvation_is_ridden_out_and_a_long_one_sheds() {
        use crate::dbv::{FeedOptions, Feeder};
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let hub = VideoHub::headless();
        let stream = hub.stream("starved");
        let _viewer = stream.viewer();
        let mut feeder = Feeder::start(
            hub.clone(),
            "starved",
            &path,
            FeedOptions {
                frames: None,
                unpaced: false,
            },
        )
        .unwrap();
        hub.set_callbacks(HubCallbacks {
            request_keyframe: Box::new({
                let answer = feeder.keyframe_answer();
                move |_| answer()
            }),
            ..Default::default()
        });
        wait_for("the stream to play", || stream.stats().published >= 60);
        let before = stream.stats();
        assert_eq!(before.dropped, 0, "the machine is too busy for a paced test");
        // 80 ms at 60 fps: five frames arrive meanwhile, two more than the queue used to hold.
        stream.hold_decoder(Duration::from_millis(80));
        wait_for("the backlog to be worked off", || {
            let stats = stream.stats();
            stats.published >= before.published + 30 && stats.queue == 0
        });
        let after = stream.stats();
        assert_eq!(
            (after.dropped, after.keyframe_requests, after.errors),
            (0, before.keyframe_requests, 0),
            "a gap shorter than the queue's depth costs nothing"
        );
        assert_eq!(after.decoded, after.received - after.queue as u64);

        // 400 ms: the queue overflows at 100 ms, the chain is broken, a keyframe is asked for
        // and the stream goes on with it, on the same decoder. (The streamer's answer arrives
        // while the thread is still held and overflows in turn: a second request, sent when
        // the 500 ms between requests have passed, as with the three-frame queue.)
        stream.hold_decoder(Duration::from_millis(400));
        wait_for("the overflow", || stream.stats().dropped > 0);
        wait_for("the stream to go on", || {
            let stats = stream.stats();
            stats.published >= after.published + 60 && stats.queue == 0
        });
        let end = stream.stats();
        let asked = end.keyframe_requests - after.keyframe_requests;
        assert!((1..=2).contains(&asked), "{asked} keyframe requests");
        assert_eq!((end.errors, end.decoder_opens), (0, 1));
        feeder.stop();
    }

    /// A stream that had a picture and gets no frames says so once after `STALLED_AFTER`, asks
    /// for a keyframe, frees its decoder, and says when its next picture is up.
    #[test]
    fn a_stream_that_stops_stalls_once_and_resumes_with_its_next_picture() {
        let Some(path) = stream_file("1080p60_h264.dbv") else {
            return;
        };
        let hub = VideoHub::headless();
        let events: Arc<Mutex<Vec<StreamEvent>>> = Arc::default();
        hub.set_callbacks(HubCallbacks {
            event: Box::new({
                let events = events.clone();
                move |_, event| events.lock().push(event)
            }),
            ..Default::default()
        });
        let count = |wanted: StreamEvent| events.lock().iter().filter(|e| **e == wanted).count();
        let stream = hub.stream("stalls");
        let _viewer = stream.viewer();
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 30);
        wait_for("30 pictures", || stream.stats().published == 30);
        let sent = Instant::now();
        let requests = stream.stats().keyframe_requests;
        assert!(!stream.is_stalled());
        wait_for("the stall", || stream.is_stalled());
        let waited = sent.elapsed();
        assert!(
            waited >= STALLED_AFTER - Duration::from_millis(50) && waited < STALLED_AFTER + Duration::from_secs(2),
            "stalled after {waited:?}"
        );
        wait_for("the decoder thread to exit", || hub.decoder_threads() == 0);
        assert_eq!(stream.stats().keyframe_requests, requests + 1);
        // Deltas that arrive now are refused (the decoder is gone) and change nothing.
        push_frames(&hub, &stream, &mut file, 5);
        assert!(stream.is_stalled());
        assert_eq!((count(StreamEvent::Stalled), count(StreamEvent::Resumed)), (1, 0));
        // The streamer's keyframe: a new decoder, a picture, the stall is over.
        let mut file = StreamFile::open(&path).unwrap();
        push_frames(&hub, &stream, &mut file, 10);
        wait_for("pictures again", || stream.stats().published == 40);
        assert!(!stream.is_stalled());
        assert_eq!((count(StreamEvent::Stalled), count(StreamEvent::Resumed)), (1, 1));
        assert_eq!(stream.stats().decoder_opens, 2);

        // A stream nobody displays any more does not stall.
        drop(_viewer);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(count(StreamEvent::Stalled), 1);
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
