//! The picture pipeline: every picture a message shows (attachment thumbnails and originals,
//! video posters, link-preview images, GIF links) goes through [`Pictures`].
//!
//! - **Keys.** A picture is decoded for one box: [`PicKey`] = where the bytes come from
//!   ([`Source`]) + the box in device pixels + how it fills it. The same attachment drawn in two
//!   boxes (a resize, the viewer) is two decodes of one fetch.
//! - **Who wants what.** The message lists tell the slots which rows are within `NEAR_PX` of the
//!   viewport (`RowSlot::near`); the slots turn those rows into keys and hand them over as an
//!   owner's [`Pictures::set_wants`]. Wanted keys are fetched (visible first, at most
//!   [`Fetcher::lane`]'s count at a time per host), decoded off the UI thread at the box's size,
//!   and kept; a key nobody wants any more is cancelled if it is still fetching or waiting, or
//!   becomes evictable once decoded.
//! - **Memory.** Two byte-bounded LRUs, both in memory only (owner decision: nothing across
//!   restarts): encoded files ([`ENCODED_BUDGET`]: a resize or a later return re-decodes
//!   without the network) and decoded pictures ([`DECODED_BUDGET`] for pictures no list
//!   wants; wanted ones are always kept). An evicted picture leaves the GPU atlas too
//!   (`drop_image`): GPUI never evicts by itself.
//! - **Frames.** Nothing here draws or polls. When a picture arrives the owners that want it
//!   are notified (`cx.notify(list)`): one frame for the swap. Animations advance on one timer
//!   each, only while some list shows them and the window is visible ([`anim`]).
//! - **Placeholders.** ThumbHashes decode on the UI thread (about 20 µs for the ≤ 32 px bitmap,
//!   memoised for 400 hashes as the renderer does): the blur must be in the first frame a row
//!   shows. The bitmap is drawn scaled up by the GPU's bilinear filter, as Chromium scales the
//!   renderer's `background-image` data URL.

pub mod anim;
pub mod decode;
pub mod fetch;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use decibell_attachments::CancellationToken;
use gpui::{App, AppContext as _, Context, Entity, EntityId, Global, RenderImage, SharedString, Subscription, Task, Window};

pub use decode::{Fit, Placement, Spec};
pub use fetch::{Fetcher, Source};

use anim::{FrameSource, Loops};

/// Encoded files kept for re-decoding. Electron kept 128 MiB of encoded thumbnails alive
/// (`imageRetention.ts`) because Blink re-decodes from them; here decoded pictures are kept
/// separately, so the encoded copy only saves a refetch when a picture is drawn at a new size
/// or comes back after its decode was evicted: 24 MiB holds a few hundred thumbnails.
pub const ENCODED_BUDGET: usize = 24 << 20;
/// Decoded pictures no list wants any more, kept for scrolling back and returning to a
/// channel. Each costs its pixels twice (the CPU copy GPUI keeps to upload from, the atlas
/// tile), so this is the lean part: about two screens of thumbnails at scale 1.
pub const DECODED_BUDGET: usize = 24 << 20;
/// Decodes running at once (GPUI's background executor runs other work too).
const DECODES: usize = 4;
/// An animation keeps every frame when they all fit in this; else it decodes as it plays.
pub const ANIM_KEEP_BUDGET: usize = 8 << 20;
/// ThumbHash bitmaps kept (`thumbhash.ts` `DECODED_MAX`).
const PLACEHOLDERS: usize = 400;

/// A picture decoded for a box.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PicKey {
    pub source: Source,
    pub w: u32,
    pub h: u32,
    pub fit: Fit,
    pub upscale: bool,
    scale_bits: u32,
}

impl PicKey {
    pub fn new(source: Source, spec: Spec) -> PicKey {
        let (w, h) = spec.device();
        PicKey { source, w, h, fit: spec.fit, upscale: spec.upscale, scale_bits: spec.scale.to_bits() }
    }

    pub fn spec(&self) -> Spec {
        let scale = f32::from_bits(self.scale_bits);
        Spec { width: self.w as f32 / scale, height: self.h as f32 / scale, scale, fit: self.fit, upscale: self.upscale }
    }
}

/// A decoded picture, ready to paint.
#[derive(Clone)]
pub struct Picture {
    pub image: Arc<RenderImage>,
    pub placement: Placement,
    pub natural: (u32, u32),
}

impl Picture {
    /// Paints the picture in its box (`origin` = the box's top-left), rounded by `radius`.
    pub fn paint(&self, origin: gpui::Point<gpui::Pixels>, corners: gpui::Corners<gpui::Pixels>, window: &mut Window) {
        let bounds = self.placement.bounds(origin);
        let _ = window.paint_image(bounds, self.placement.image_bounds(origin), corners, self.image.clone(), 0, false);
    }
}

/// What a key is now.
#[derive(Clone)]
pub enum Look {
    /// Not here (yet): draw the placeholder.
    Waiting,
    Ready(Picture),
    /// The fetch or decode failed.
    Failed(SharedString),
}

enum State {
    Waiting,
    Ready(Picture),
    Failed(SharedString),
}

struct Entry {
    state: State,
    /// Decoded bytes (CPU copy; animation frames included).
    bytes: usize,
    /// LRU clock: when a list last wanted it.
    used: u64,
    /// Views to notify when it arrives (beyond the owners that want it).
    waiters: Vec<EntityId>,
    started: Instant,
}

/// A fetch or decode ended for `source` (the embeds re-measure a link picture whose box waits
/// for its size, and drop one that failed).
pub struct Settled {
    pub source: Source,
    pub ok: bool,
}

impl gpui::EventEmitter<Settled> for Pictures {}

/// Who wants pictures: a list's slot (the list's entity and the slot's number), the viewer, a
/// Transfers row…
pub type Owner = (EntityId, u8);

#[derive(Clone)]
pub struct Want {
    pub key: PicKey,
    /// On screen: fetched first, and animated.
    pub visible: bool,
}

enum JobState {
    Queued,
    Running { cancel: CancellationToken, _task: Task<()> },
}

struct Job {
    state: JobState,
    visible: bool,
    lane: SharedString,
    /// Order of arrival: rows are handed over top to bottom, nearest the viewport first.
    seq: u64,
}

/// Counters for the measurements.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub fetched: u64,
    pub fetched_bytes: u64,
    pub decoded: u64,
    /// µs spent decoding (background), summed.
    pub decode_us: u64,
    pub decode_max_us: u64,
    pub cancelled: u64,
    pub evicted: u64,
    pub failed: u64,
    /// Animation frames shown.
    pub frames: u64,
    /// Frames decoded for animations (kept or streamed).
    pub anim_decoded: u64,
    /// Timers alive now (one per playing animation).
    pub playing: usize,
}

struct Anim {
    source: Option<Box<FrameSource>>,
    /// Every frame so far while they fit [`ANIM_KEEP_BUDGET`]; `complete` once a loop ended
    /// with all of them kept (then nothing decodes any more).
    kept: Vec<(Arc<RenderImage>, Duration)>,
    kept_bytes: usize,
    keeping: bool,
    complete: bool,
    /// Index of the frame shown (into `kept` when complete).
    current: usize,
    current_delay: Duration,
    /// The decoded frame after the current one, while streaming.
    next: Option<(Arc<RenderImage>, Duration)>,
    /// The timer for the current frame ran out: advance as soon as the next frame is here.
    due: bool,
    loops_done: u32,
    loops: Loops,
    finished: bool,
    timer: Option<Task<()>>,
    decoding: Option<Task<()>>,
    /// Painted since the last advance (else the window doesn't show it: stop).
    painted: bool,
    placement: Placement,
}

/// See the module doc.
pub struct Pictures {
    fetcher: Rc<dyn Fetcher>,
    entries: HashMap<PicKey, Entry>,
    encoded: Encoded,
    wants: HashMap<Owner, Vec<Want>>,
    /// Derived from `wants`: how many owners want each key, and the visible ones.
    pinned: HashMap<PicKey, u32>,
    visible: HashSet<PicKey>,
    jobs: HashMap<Source, Job>,
    running: HashMap<SharedString, usize>,
    decode_queue: VecDeque<PicKey>,
    decoding: HashMap<PicKey, Task<()>>,
    anims: HashMap<PicKey, Anim>,
    clock: u64,
    decoded_bytes: usize,
    placeholders: Placeholders,
    windows: HashMap<gpui::WindowId, Subscription>,
    window_hidden: bool,
    pub stats: Stats,
}

struct Holder(Entity<Pictures>);

impl Global for Holder {}

impl Pictures {
    /// Installs the pipeline with `fetcher` (the core's in the app, a memory one in screens).
    pub fn install(fetcher: Rc<dyn Fetcher>, cx: &mut App) -> Entity<Pictures> {
        let entity = cx.new(|_| Pictures::new(fetcher));
        cx.set_global(Holder(entity.clone()));
        entity
    }

    pub fn global(cx: &App) -> Option<Entity<Pictures>> {
        cx.try_global::<Holder>().map(|holder| holder.0.clone())
    }

    fn new(fetcher: Rc<dyn Fetcher>) -> Pictures {
        Pictures {
            fetcher,
            entries: HashMap::new(),
            encoded: Encoded::default(),
            wants: HashMap::new(),
            pinned: HashMap::new(),
            visible: HashSet::new(),
            jobs: HashMap::new(),
            running: HashMap::new(),
            decode_queue: VecDeque::new(),
            decoding: HashMap::new(),
            anims: HashMap::new(),
            clock: 0,
            decoded_bytes: 0,
            placeholders: Placeholders::default(),
            windows: HashMap::new(),
            window_hidden: false,
            stats: Stats::default(),
        }
    }

    /// What `key` looks like now (no side effects: call [`Self::request`] for one not wanted).
    pub fn look(&self, key: &PicKey) -> Look {
        match self.entries.get(key).map(|entry| &entry.state) {
            Some(State::Ready(picture)) => Look::Ready(picture.clone()),
            Some(State::Failed(error)) => Look::Failed(error.clone()),
            _ => Look::Waiting,
        }
    }

    /// The closest picture of the same source already decoded for another box: shown scaled
    /// while the right size decodes (a resize), so nothing ever flashes back to the blur.
    pub fn stand_in(&self, key: &PicKey) -> Option<Picture> {
        self.entries
            .iter()
            .filter(|(other, entry)| other.source == key.source && other.fit == key.fit && matches!(entry.state, State::Ready(_)))
            .min_by_key(|(other, _)| (other.w as i64 - key.w as i64).abs() + (other.h as i64 - key.h as i64).abs())
            .and_then(|(_, entry)| match &entry.state {
                State::Ready(picture) => Some(picture.clone()),
                _ => None,
            })
    }

    /// The file of `source` now (a copy, a save): from memory when it is there, else fetched
    /// outside the queues (one request, the caller's).
    pub fn fetch_now(&mut self, source: &Source, cx: &mut Context<Self>) -> Task<Result<Bytes, SharedString>> {
        if let Some(bytes) = self.encoded.get(source) {
            return Task::ready(Ok(bytes));
        }
        self.fetcher.fetch(source, CancellationToken::new(), cx)
    }

    /// Hands over bytes the app holds (see [`Source::Local`]): kept in the encoded LRU like a
    /// fetched file. Cheap to repeat.
    pub fn provide(&mut self, source: Source, bytes: Bytes) {
        if self.encoded.map.contains_key(&source) {
            return;
        }
        self.encoded.insert(source, bytes);
    }

    /// Decoded bytes held now (CPU copies), and encoded bytes.
    pub fn bytes(&self) -> (usize, usize) {
        (self.decoded_bytes, self.encoded.total)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Fetches in flight or queued.
    pub fn pending(&self) -> usize {
        self.jobs.len() + self.decode_queue.len() + self.decoding.len()
    }

    /// Animations whose timer runs now.
    pub fn playing(&self) -> usize {
        self.anims.values().filter(|anim| anim.timer.is_some()).count()
    }

    /// `view` draws `key` now (a row on screen before the list told its slots, the viewer, a
    /// Transfers row): fetch it if nobody did, and notify `view` when it arrives.
    pub fn request(&mut self, key: &PicKey, view: Option<EntityId>, cx: &mut Context<Self>) {
        self.enter(key, view);
        self.pump(cx);
    }

    /// Records `key` as waiting (with `view` to notify) and queues its fetch or decode, without
    /// starting anything yet.
    fn enter(&mut self, key: &PicKey, view: Option<EntityId>) {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(key) {
            entry.used = self.clock;
            if let (State::Waiting, Some(view)) = (&entry.state, view)
                && !entry.waiters.contains(&view)
            {
                entry.waiters.push(view);
            }
            return;
        }
        self.entries.insert(
            key.clone(),
            Entry { state: State::Waiting, bytes: 0, used: self.clock, waiters: view.into_iter().collect(), started: Instant::now() },
        );
        self.queue(key);
    }

    /// Replaces what `owner` wants (an empty list: nothing any more).
    pub fn set_wants(&mut self, owner: Owner, wants: Vec<Want>, cx: &mut Context<Self>) {
        if wants.is_empty() {
            self.wants.remove(&owner);
        } else {
            self.wants.insert(owner, wants.clone());
        }
        self.pinned.clear();
        self.visible.clear();
        for list in self.wants.values() {
            for want in list {
                *self.pinned.entry(want.key.clone()).or_default() += 1;
                if want.visible {
                    self.visible.insert(want.key.clone());
                }
            }
        }
        // New wants queue; the visible ones jump the fetch queue.
        for want in &wants {
            self.enter(&want.key, None);
            if let Some(job) = self.jobs.get_mut(&want.key.source) {
                job.visible |= want.visible;
            }
        }
        // A failure is forgotten when its row goes: coming back retries (as a remounted
        // `<img>` asks again).
        let pinned = &self.pinned;
        self.entries.retain(|key, entry| !matches!(entry.state, State::Failed(_)) || pinned.contains_key(key));
        self.cancel_unwanted(cx);
        self.update_animations(cx);
        self.evict(cx);
        self.pump(cx);
    }

    fn wanted(&self, key: &PicKey) -> bool {
        self.pinned.contains_key(key) || self.entries.get(key).is_some_and(|entry| !entry.waiters.is_empty())
    }

    /// Waiting keys nobody wants any more go; a fetch whose keys all went is cancelled.
    fn cancel_unwanted(&mut self, _cx: &mut Context<Self>) {
        let gone: Vec<PicKey> = self
            .entries
            .iter()
            .filter(|(key, entry)| matches!(entry.state, State::Waiting) && !self.pinned.contains_key(*key) && entry.waiters.is_empty())
            .map(|(key, _)| key.clone())
            .collect();
        for key in &gone {
            self.entries.remove(key);
            self.decoding.remove(key);
            self.stats.cancelled += 1;
        }
        self.decode_queue.retain(|key| !gone.contains(key));
        let live: HashSet<&Source> = self.entries.iter().filter(|(_, e)| matches!(e.state, State::Waiting)).map(|(k, _)| &k.source).collect();
        let dead: Vec<Source> = self.jobs.keys().filter(|source| !live.contains(source)).cloned().collect();
        for source in dead {
            if let Some(job) = self.jobs.remove(&source) {
                if let JobState::Running { cancel, .. } = job.state {
                    cancel.cancel();
                    if let Some(count) = self.running.get_mut(&job.lane) {
                        *count = count.saturating_sub(1);
                    }
                }
            }
        }
    }

    /// A waiting key: decode from the encoded copy, or fetch (started by the next [`Self::pump`]).
    fn queue(&mut self, key: &PicKey) {
        if self.encoded.get(&key.source).is_some() {
            if !self.decode_queue.contains(key) && !self.decoding.contains_key(key) {
                if self.visible.contains(key) {
                    self.decode_queue.push_front(key.clone());
                } else {
                    self.decode_queue.push_back(key.clone());
                }
            }
            return;
        }
        if !self.jobs.contains_key(&key.source) {
            let (lane, _) = self.fetcher.lane(&key.source);
            let visible = self.visible.contains(key);
            self.clock += 1;
            self.jobs.insert(key.source.clone(), Job { state: JobState::Queued, visible, lane, seq: self.clock });
        }
    }

    /// Starts what the limits allow: decodes (visible first), then fetches (visible first;
    /// a decode whose file was evicted meanwhile queues a fetch).
    fn pump(&mut self, cx: &mut Context<Self>) {
        while self.decoding.len() < DECODES {
            let Some(key) = self.decode_queue.pop_front() else { break };
            if !self.wanted(&key) {
                continue;
            }
            let Some(bytes) = self.encoded.get(&key.source) else {
                // Evicted meanwhile: fetch again.
                self.queue(&key);
                continue;
            };
            let spec = key.spec();
            let decode = cx.background_spawn(async move {
                let started = Instant::now();
                let result = decode_any(bytes, spec);
                (result, started.elapsed())
            });
            let task = cx.spawn({
                let key = key.clone();
                async move |this, cx| {
                    let (result, took) = decode.await;
                    let _ = this.update(cx, |this, cx| this.decoded(key, result, took, cx));
                }
            });
            self.decoding.insert(key, task);
        }
        loop {
            let next = self
                .jobs
                .iter()
                .filter(|(source, job)| {
                    matches!(job.state, JobState::Queued) && self.running.get(&job.lane).copied().unwrap_or(0) < self.fetcher.lane(source).1
                })
                .max_by_key(|(_, job)| (job.visible, std::cmp::Reverse(job.seq)))
                .map(|(source, _)| source.clone());
            let Some(source) = next else { break };
            let cancel = CancellationToken::new();
            let fetch = self.fetcher.fetch(&source, cancel.clone(), cx);
            let task = cx.spawn({
                let source = source.clone();
                async move |this, cx| {
                    let result = fetch.await;
                    let _ = this.update(cx, |this, cx| this.fetched(source, result, cx));
                }
            });
            let job = self.jobs.get_mut(&source).expect("queued above");
            *self.running.entry(job.lane.clone()).or_default() += 1;
            job.state = JobState::Running { cancel, _task: task };
        }
    }

    fn fetched(&mut self, source: Source, result: Result<Bytes, SharedString>, cx: &mut Context<Self>) {
        let Some(job) = self.jobs.remove(&source) else { return };
        if let Some(count) = self.running.get_mut(&job.lane) {
            *count = count.saturating_sub(1);
        }
        let waiting: Vec<PicKey> =
            self.entries.iter().filter(|(key, entry)| key.source == source && matches!(entry.state, State::Waiting)).map(|(key, _)| key.clone()).collect();
        match result {
            Ok(bytes) => {
                self.stats.fetched += 1;
                self.stats.fetched_bytes += bytes.len() as u64;
                self.encoded.insert(source, bytes);
                for key in waiting {
                    if !self.decode_queue.contains(&key) && !self.decoding.contains_key(&key) {
                        // Visible first.
                        if self.visible.contains(&key) {
                            self.decode_queue.push_front(key);
                        } else {
                            self.decode_queue.push_back(key);
                        }
                    }
                }
            }
            Err(error) => {
                self.stats.failed += 1;
                for key in waiting {
                    self.fail(&key, error.clone(), cx);
                }
            }
        }
        self.pump(cx);
    }

    fn fail(&mut self, key: &PicKey, error: SharedString, cx: &mut Context<Self>) {
        log::debug!("[pictures] {:?}: {error}", key.source);
        cx.emit(Settled { source: key.source.clone(), ok: false });
        let Some(entry) = self.entries.get_mut(key) else { return };
        entry.state = State::Failed(error);
        let waiters = std::mem::take(&mut entry.waiters);
        self.notify_for(key, waiters, cx);
    }

    fn decoded(&mut self, key: PicKey, result: Result<Decoded, String>, took: Duration, cx: &mut Context<Self>) {
        self.decoding.remove(&key);
        let us = took.as_micros() as u64;
        self.stats.decoded += 1;
        self.stats.decode_us += us;
        self.stats.decode_max_us = self.stats.decode_max_us.max(us);
        if !self.wanted(&key) {
            // The row went before its picture arrived.
            self.entries.remove(&key);
            self.stats.cancelled += 1;
            self.pump(cx);
            return;
        }
        match result {
            Ok(decoded) => {
                let picture = Picture { image: Arc::new(decoded.still.image), placement: decoded.still.placement, natural: decoded.still.natural };
                self.clock += 1;
                let used = self.clock;
                if let Some(entry) = self.entries.get_mut(&key) {
                    log::trace!("[pictures] {:?} ready after {:?}", key.source, entry.started.elapsed());
                    entry.state = State::Ready(picture.clone());
                    entry.bytes = decoded.still.bytes;
                    entry.used = used;
                    self.decoded_bytes += decoded.still.bytes;
                    let waiters = std::mem::take(&mut entry.waiters);
                    if let Some(source) = decoded.anim {
                        self.anims.insert(key.clone(), Anim::new(source, picture, decoded.first_delay));
                    }
                    self.notify_for(&key, waiters, cx);
                }
                cx.emit(Settled { source: key.source.clone(), ok: true });
                self.update_animations(cx);
                self.evict(cx);
            }
            Err(error) => {
                self.stats.failed += 1;
                self.fail(&key, error.into(), cx);
            }
        }
        self.pump(cx);
    }

    /// Redraw whoever shows `key`: the owners that want it and the extra waiters.
    fn notify_for(&self, key: &PicKey, waiters: Vec<EntityId>, cx: &mut Context<Self>) {
        let mut views = waiters;
        for (owner, wants) in &self.wants {
            if !views.contains(&owner.0) && wants.iter().any(|want| &want.key == key) {
                views.push(owner.0);
            }
        }
        for view in views {
            notify_view(cx, view);
        }
    }

    /// Unwanted decoded pictures beyond the budget go, least recently wanted first.
    fn evict(&mut self, cx: &mut Context<Self>) {
        let unpinned: usize = self
            .entries
            .iter()
            .filter(|(key, entry)| !self.pinned.contains_key(*key) && entry.waiters.is_empty() && matches!(entry.state, State::Ready(_)))
            .map(|(_, entry)| entry.bytes)
            .sum();
        if unpinned <= DECODED_BUDGET {
            return;
        }
        let mut candidates: Vec<(u64, PicKey)> = self
            .entries
            .iter()
            .filter(|(key, entry)| !self.pinned.contains_key(*key) && entry.waiters.is_empty() && matches!(entry.state, State::Ready(_)))
            .map(|(key, entry)| (entry.used, key.clone()))
            .collect();
        candidates.sort_by_key(|(used, _)| *used);
        let mut over = unpinned - DECODED_BUDGET;
        for (_, key) in candidates {
            if over == 0 {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                over = over.saturating_sub(entry.bytes);
                self.release(key, entry, cx);
            }
        }
    }

    fn release(&mut self, key: PicKey, entry: Entry, cx: &mut App) {
        self.decoded_bytes = self.decoded_bytes.saturating_sub(entry.bytes);
        self.stats.evicted += 1;
        let mut images = Vec::new();
        if let State::Ready(picture) = entry.state {
            images.push(picture.image);
        }
        if let Some(anim) = self.anims.remove(&key) {
            images.extend(anim.kept.into_iter().map(|(image, _)| image));
            images.extend(anim.next.map(|(image, _)| image));
        }
        drop_images(images, cx);
    }

    /// Forgets everything (sign-out): pictures, files, timers.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let entries: Vec<(PicKey, Entry)> = self.entries.drain().collect();
        for (key, entry) in entries {
            self.release(key, entry, cx);
        }
        for (_, job) in self.jobs.drain() {
            if let JobState::Running { cancel, .. } = job.state {
                cancel.cancel();
            }
        }
        self.running.clear();
        self.decode_queue.clear();
        self.decoding.clear();
        self.encoded = Encoded::default();
        self.wants.clear();
        self.pinned.clear();
        self.visible.clear();
        self.decoded_bytes = 0;
        let placeholders = std::mem::take(&mut self.placeholders);
        drop_images(placeholders.order.into_iter().filter_map(|hash| placeholders.map.get(&hash).cloned().flatten()).collect(), cx);
    }

    // ── Placeholders ─────────────────────────────────────────────────────────────────────────

    /// The ThumbHash `hash` (base64) as a small bordered bitmap with its decoded size, memoised.
    /// `None` for an empty or malformed hash (the flat fill shows instead).
    pub fn placeholder(&mut self, hash: &str, cx: &mut App) -> Option<(Arc<RenderImage>, (u32, u32))> {
        if hash.is_empty() {
            return None;
        }
        if let Some(found) = self.placeholders.map.get(hash) {
            return found.clone().map(|image| {
                let size = image.size(0);
                (image, (size.width.0 as u32 - 2, size.height.0 as u32 - 2))
            });
        }
        let decoded = decibell_attachments::thumbhash::decode_b64(hash).and_then(|d| {
            let rgba = image::RgbaImage::from_raw(d.width as u32, d.height as u32, d.rgba)?;
            Some(Arc::new(decode::render_image(&rgba)))
        });
        let key: SharedString = SharedString::from(hash.to_string());
        self.placeholders.map.insert(key.clone(), decoded.clone());
        self.placeholders.order.push_back(key);
        if self.placeholders.order.len() > PLACEHOLDERS {
            // A quarter at once, as the renderer trims its map.
            let mut gone = Vec::new();
            for _ in 0..PLACEHOLDERS / 4 {
                if let Some(old) = self.placeholders.order.pop_front()
                    && let Some(Some(image)) = self.placeholders.map.remove(&old)
                {
                    gone.push(image);
                }
            }
            // Painted this frame perhaps: dropped from the atlas after it.
            cx.defer(move |cx| drop_images(gone, cx));
        }
        decoded.map(|image| {
            let size = image.size(0);
            (image, (size.width.0 as u32 - 2, size.height.0 as u32 - 2))
        })
    }

    // ── Animations ───────────────────────────────────────────────────────────────────────────

    /// The tile of an animated key painted: it is on screen in a visible window.
    pub fn painted(&mut self, key: &PicKey, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle().window_id();
        if !self.windows.contains_key(&handle) {
            let pictures = cx.entity().downgrade();
            let subscription = window.observe_window_visibility(move |visibility, _, cx| {
                let _ = pictures.update(cx, |this, cx| {
                    this.window_hidden = !visibility.is_visible();
                    this.update_animations(cx);
                });
            });
            self.windows.insert(handle, subscription);
        }
        if let Some(anim) = self.anims.get_mut(key) {
            anim.painted = true;
            if anim.timer.is_none() && !anim.finished && self.visible.contains(key) && !self.window_hidden {
                let (key, this) = (key.clone(), cx.weak_entity());
                cx.defer(move |cx| {
                    let _ = this.update(cx, |this, cx| this.update_animations_for(&key, cx));
                });
            }
        }
    }

    fn update_animations(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<PicKey> = self.anims.keys().cloned().collect();
        for key in keys {
            self.update_animations_for(&key, cx);
        }
        self.stats.playing = self.playing();
    }

    /// Starts or stops `key`'s timer: it runs only while a list shows the picture and the
    /// window is visible.
    fn update_animations_for(&mut self, key: &PicKey, cx: &mut Context<Self>) {
        let should = self.visible.contains(key) && !self.window_hidden;
        let Some(anim) = self.anims.get_mut(key) else { return };
        if !should || anim.finished {
            anim.timer = None;
            anim.due = false;
            return;
        }
        if anim.timer.is_some() || anim.due {
            return;
        }
        anim.painted = true;
        self.arm(key, cx);
        self.prefetch_frame(key, cx);
    }

    fn arm(&mut self, key: &PicKey, cx: &mut Context<Self>) {
        let Some(anim) = self.anims.get_mut(key) else { return };
        let delay = anim.current_delay;
        let key = key.clone();
        anim.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| this.tick(&key, cx));
        }));
    }

    fn tick(&mut self, key: &PicKey, cx: &mut Context<Self>) {
        let Some(anim) = self.anims.get_mut(key) else { return };
        anim.timer = None;
        if !anim.painted {
            // Nothing drew the last frame: the window doesn't show it (minimised, covered on a
            // compositor that stops frames). The next paint starts it again.
            self.stats.playing = self.playing();
            return;
        }
        anim.due = true;
        self.advance(key, cx);
    }

    /// Shows the next frame if it is decoded and due.
    fn advance(&mut self, key: &PicKey, cx: &mut Context<Self>) {
        let Some(anim) = self.anims.get_mut(key) else { return };
        if !anim.due {
            return;
        }
        let next = if anim.complete {
            let index = (anim.current + 1) % anim.kept.len().max(1);
            if index == 0 {
                anim.loops_done += 1;
            }
            anim.current = index;
            anim.kept.get(index).cloned()
        } else {
            anim.next.take()
        };
        if let Loops::Times(times) = anim.loops
            && anim.loops_done >= times
        {
            anim.finished = true;
            anim.due = false;
            return;
        }
        let Some((image, delay)) = next else {
            // Not decoded yet: shown when it is.
            self.prefetch_frame(key, cx);
            return;
        };
        anim.due = false;
        anim.painted = false;
        anim.current_delay = delay;
        let previous = self.entries.get_mut(key).and_then(|entry| match &mut entry.state {
            State::Ready(picture) => Some(std::mem::replace(&mut picture.image, image.clone())),
            _ => None,
        });
        // A streamed frame leaves the atlas once the next one shows; kept frames stay.
        let keeps_previous = anim.keeping || anim.complete;
        if let (Some(previous), false) = (previous, keeps_previous) {
            cx.defer(move |cx| drop_images(vec![previous], cx));
        }
        self.stats.frames += 1;
        self.notify_for(key, Vec::new(), cx);
        self.arm(key, cx);
        self.prefetch_frame(key, cx);
    }

    /// Decodes the frame after the current one in the background (unless every frame is kept
    /// or one is already on its way).
    fn prefetch_frame(&mut self, key: &PicKey, cx: &mut Context<Self>) {
        let Some(anim) = self.anims.get_mut(key) else { return };
        if anim.complete || anim.next.is_some() || anim.decoding.is_some() || anim.finished {
            return;
        }
        let Some(mut source) = anim.source.take() else { return };
        let placement = anim.placement;
        let task = cx.background_spawn(async move {
            let mut looped = false;
            let frame = match source.next() {
                Ok(Some(frame)) => Ok(Some(frame)),
                Ok(None) => {
                    looped = true;
                    source.restart().and_then(|_| source.next())
                }
                Err(e) => Err(e),
            };
            let frame = frame.map(|frame| {
                frame.map(|(rgba, delay)| {
                    let fitted = decode::fit_pixels(&rgba, &placement);
                    (decode::render_image(&fitted), delay, ((fitted.width() + 2) * (fitted.height() + 2) * 4) as usize)
                })
            });
            (source, frame, looped)
        });
        let key = key.clone();
        anim.decoding = Some(cx.spawn(async move |this, cx| {
            let (source, frame, looped) = task.await;
            let _ = this.update(cx, |this, cx| this.frame_decoded(&key, source, frame, looped, cx));
        }));
    }

    #[allow(clippy::type_complexity)]
    fn frame_decoded(
        &mut self,
        key: &PicKey,
        source: Box<FrameSource>,
        frame: Result<Option<(RenderImage, Duration, usize)>, String>,
        looped: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(anim) = self.anims.get_mut(key) else { return };
        anim.decoding = None;
        anim.source = Some(source);
        self.stats.anim_decoded += 1;
        let (image, delay, bytes) = match frame {
            Ok(Some(frame)) => frame,
            // A broken frame or an empty file: stay on what shows.
            _ => {
                anim.finished = true;
                anim.timer = None;
                return;
            }
        };
        let image = Arc::new(image);
        if looped {
            anim.loops_done += 1;
            if anim.keeping {
                // The whole loop fitted: play it from memory from now on.
                anim.complete = true;
                anim.source = None;
                anim.current = anim.kept.len() - 1;
                // The frame just decoded is frame 0 again, which is kept already.
                cx.defer(move |cx| drop_images(vec![image], cx));
                self.advance(key, cx);
                return;
            }
        }
        if anim.keeping {
            if anim.kept_bytes + bytes <= ANIM_KEEP_BUDGET {
                anim.kept.push((image.clone(), delay));
                anim.kept_bytes += bytes;
            } else {
                // Too long to keep: from now on frames are decoded as they play.
                anim.keeping = false;
                let current = self.entries.get(key).and_then(|entry| match &entry.state {
                    State::Ready(picture) => Some(picture.image.clone()),
                    _ => None,
                });
                let gone: Vec<Arc<RenderImage>> =
                    anim.kept.drain(..).map(|(image, _)| image).filter(|image| Some(image.id) != current.as_ref().map(|c| c.id)).collect();
                self.decoded_bytes = self.decoded_bytes.saturating_sub(anim.kept_bytes);
                anim.kept_bytes = 0;
                cx.defer(move |cx| drop_images(gone, cx));
            }
        }
        if let Some(anim) = self.anims.get_mut(key) {
            if anim.keeping {
                self.decoded_bytes += bytes;
                if let Some(entry) = self.entries.get_mut(key) {
                    entry.bytes += bytes;
                }
            }
            anim.next = Some((image, delay));
        }
        self.advance(key, cx);
    }
}

impl Anim {
    fn new(source: Box<FrameSource>, first: Picture, first_delay: Duration) -> Anim {
        let loops = source.loops;
        Anim {
            source: Some(source),
            kept: vec![(first.image.clone(), first_delay)],
            kept_bytes: 0,
            keeping: true,
            complete: false,
            current: 0,
            current_delay: first_delay,
            next: None,
            due: false,
            loops_done: 0,
            loops,
            finished: false,
            timer: None,
            decoding: None,
            painted: false,
            placement: first.placement,
        }
    }
}

/// Redraws `view` (a list, the viewer) from inside the store's update.
fn notify_view(cx: &mut App, view: EntityId) {
    cx.notify(view);
}

fn drop_images(images: Vec<Arc<RenderImage>>, cx: &mut App) {
    for image in images {
        cx.drop_image(image, None);
    }
}

/// What a decode produced: the still (an animation's first frame) and, for an animation, its
/// frame source positioned after that frame.
struct Decoded {
    still: decode::Still,
    anim: Option<Box<FrameSource>>,
    first_delay: Duration,
}

/// A `data:image/…;base64,…` picture (a community's, from central) as a key drawn in a
/// `w` × `h` box (`object-cover`): its bytes handed over as a local file.
pub fn data_url_key(data: &str, w: f32, h: f32, scale: f32, cx: &mut App) -> Option<PicKey> {
    use base64::Engine as _;
    use std::hash::{Hash, Hasher};
    let pictures = Pictures::global(cx)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    let source = Source::Local(hasher.finish());
    let needs = !pictures.read(cx).encoded.map.contains_key(&source);
    if needs {
        let payload = data.split_once(',').map(|(_, p)| p)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
        pictures.update(cx, |p, _| p.provide(source.clone(), Bytes::from(bytes)));
    }
    Some(PicKey::new(source, Spec { width: w, height: h, scale, fit: Fit::Cover, upscale: true }))
}

fn decode_any(bytes: Bytes, spec: Spec) -> Result<Decoded, String> {
    if matches!(decode::sniff(&bytes), decode::Kind::Gif | decode::Kind::WebP)
        && let Some(mut source) = FrameSource::open(bytes.clone())?
    {
        let (first, delay) = source.next()?.ok_or("an animation without frames")?;
        let placement = decode::place(spec, source.size);
        let fitted = decode::fit_pixels(&first, &placement);
        let still = decode::Still {
            bytes: ((fitted.width() + 2) * (fitted.height() + 2) * 4) as usize,
            image: decode::render_image(&fitted),
            placement,
            natural: source.size,
        };
        // One frame: a still after all.
        let animated = {
            let mut probe = FrameSource::open(bytes)?.ok_or("not an animation")?;
            probe.next()?;
            probe.next()?.is_some()
        };
        return Ok(Decoded { still, anim: animated.then(|| Box::new(source)), first_delay: delay });
    }
    Ok(Decoded { still: decode::decode_still(&bytes, spec)?, anim: None, first_delay: Duration::ZERO })
}

// ── The encoded LRU ─────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Encoded {
    map: HashMap<Source, (Bytes, u64)>,
    order: BTreeMap<u64, Source>,
    tick: u64,
    total: usize,
}

impl Encoded {
    fn get(&mut self, source: &Source) -> Option<Bytes> {
        let (bytes, tick) = self.map.get_mut(source)?;
        self.order.remove(tick);
        self.tick += 1;
        *tick = self.tick;
        self.order.insert(self.tick, source.clone());
        Some(bytes.clone())
    }

    fn insert(&mut self, source: Source, bytes: Bytes) {
        if let Some((old, tick)) = self.map.remove(&source) {
            self.order.remove(&tick);
            self.total -= old.len();
        }
        self.tick += 1;
        self.total += bytes.len();
        self.order.insert(self.tick, source.clone());
        self.map.insert(source, (bytes, self.tick));
        // The newest always stays (a GIF bigger than the budget plays from it).
        while self.total > ENCODED_BUDGET && self.map.len() > 1 {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((bytes, _)) = self.map.remove(&oldest) {
                self.total -= bytes.len();
            }
        }
    }
}

#[derive(Default)]
struct Placeholders {
    map: HashMap<SharedString, Option<Arc<RenderImage>>>,
    order: VecDeque<SharedString>,
}

#[cfg(test)]
mod tests;
