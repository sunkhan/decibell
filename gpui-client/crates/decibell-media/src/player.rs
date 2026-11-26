//! One file being played: the control thread that reads it, the clock, and what the UI asks.
//!
//! ```text
//! decibell-media (control thread, one per player)            decibell-video-<n> (the hub's)
//!   read packet ──video──> stash ──> Stream::push_timed ──> decode ──> Timing::admit: sleep until
//!        │                                                              the picture is due on the
//!        └──audio──> decode, resample ──> ring ─────┐                   clock, then publish
//!                                                    v
//!                          device callback: Renderer::render ──> the clock
//! ```
//!
//! * The control thread sleeps (`park`) whenever the ring is full and the video queue is full,
//!   which is nearly always: it is woken by `admit` (the decoder took a packet) and by the
//!   renderer's notes (the ring is half empty, the clock started or stopped, the file's last
//!   sample was played, the device may be closed). No timer, except one one-shot sleep to the
//!   end of a video whose sound ended first.
//! * The clock is the audio renderer's ([`crate::render`]): the media time of the sample being
//!   heard. A file without sound (or without a device) runs on a monotonic clock that starts
//!   with its first picture.
//! * A seek is a new **epoch**: the picture queue is dropped, the decoder flushed at the next
//!   keyframe, pictures before the target decoded and skipped, the sound faded out, trimmed to
//!   the target's sample and faded in, and held behind a gate until the target's picture is up.
//! * Paused: the control thread and the decoder thread are parked (the decoder inside `admit`,
//!   with the next picture in hand), the device stream is closed 250 ms after the fade-out, and
//!   nothing wakes until a command arrives.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use decibell_video::{
    Admit, Codec, DecodeBackend, Packet, Stream, StreamEvent, TimedFrame, TimedPush, Timing, VideoHub, Viewer,
};
use parking_lot::{Condvar, Mutex};

use crate::audio::AudioDecoder;
use crate::demux::{Demuxer, OpenError, Read};
use crate::output::{AudioOutput, OutputStream, RendererCell};
use crate::render::{CHANNELS, Feed, Renderer, Shared, note};
use crate::source::Source;

/// Video packets read ahead of the decoder's queue (so that the audio between them can be
/// reached in a file that interleaves coarsely).
const VIDEO_STASH: usize = 24;
/// A picture later than this is dropped instead of shown (the decoder is catching up).
const LATE_DROP: i64 = 100_000;
/// Resyncs without a picture in between before the video is given up.
const MAX_RESYNCS: u32 = 4;

/// What a player is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Reading the container's header.
    Loading,
    Paused,
    Playing,
    /// Played to its end; play starts it again from the beginning.
    Ended,
    Failed(String),
}

/// What the picture is, once the file is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoInfo {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    /// The decoder in use, once one opened.
    pub backend: Option<DecodeBackend>,
}

/// A player's state as the UI draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub phase: Phase,
    /// Microseconds; 0 while unknown (the renderer's guard for `Infinity`).
    pub duration: i64,
    pub video: Option<VideoInfo>,
    pub has_audio: bool,
    /// The first picture of the current position is on screen.
    pub has_picture: bool,
}

/// Counters for tests and the stats line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayStats {
    /// Pictures handed to the screen.
    pub shown: u64,
    /// Pictures decoded and dropped because they were late.
    pub late: u64,
    /// Pictures decoded on the way to a seek target, or while nothing showed the video.
    pub skipped: u64,
    /// The largest lateness of a shown picture, microseconds.
    pub max_late: u64,
    pub seeks: u64,
    /// Device streams opened.
    pub streams_opened: u64,
}

/// How to open a player.
pub struct Open {
    pub source: Box<dyn Source>,
    /// Where the picture is decoded and drawn; `None`: sound only (an audio attachment).
    pub video: Option<VideoHub>,
    /// The hub's key for this player's stream (unique among players and never a uid).
    pub key: String,
    pub output: Arc<dyn AudioOutput>,
    /// Start here (microseconds): the position this attachment was left at.
    pub start_at: i64,
    pub play: bool,
    pub volume: f32,
    pub muted: bool,
    /// Called (from the player's threads) when [`Player::status`] changed. Must not block.
    pub on_change: Box<dyn Fn() + Send + Sync>,
}

#[derive(Clone, Copy)]
struct Mono {
    pts: i64,
    at: Instant,
    running: bool,
}

impl Mono {
    fn now(&self, at: Instant) -> i64 {
        if self.running {
            self.pts + at.saturating_duration_since(self.at).as_micros() as i64
        } else {
            self.pts
        }
    }
}

struct State {
    // What the UI asked for.
    want_play: bool,
    seek: Option<i64>,
    closing: bool,
    visible: bool,
    volume: f32,
    muted: bool,
    looping: bool,
    resync: bool,
    // The clock and the epoch.
    epoch: u32,
    target: i64,
    first_shown: bool,
    mono: Mono,
    /// The audio renderer's clock is the clock (false: the monotonic one).
    audio_clock: bool,
    audio: Option<Arc<Shared>>,
    frame_micros: i64,
    /// The file has been read to its end (for the current epoch).
    at_end: bool,
    status: Status,
    resyncs: u32,
    /// The last picture's time (the UI's position while the clock is not the current epoch's).
    last_shown: i64,
}

struct Inner {
    state: Mutex<State>,
    due: Condvar,
    control: std::sync::OnceLock<std::thread::Thread>,
    abort: Arc<AtomicBool>,
    on_change: Box<dyn Fn() + Send + Sync>,
    shown: AtomicU64,
    late: AtomicU64,
    skipped: AtomicU64,
    max_late: AtomicU64,
    seeks: AtomicU64,
    streams_opened: AtomicU64,
    /// Tests: every shown picture's media time and the instant it was handed over.
    presented: Mutex<Option<Box<dyn Fn(i64, Instant) + Send>>>,
}

impl Inner {
    fn wake(&self) {
        if let Some(control) = self.control.get() {
            control.unpark();
        }
    }

    /// The clock: media time now, and whether it is running.
    fn clock(&self, state: &State, at: Instant) -> (i64, bool) {
        if state.audio_clock
            && let Some(audio) = &state.audio
        {
            return match audio.now(at) {
                Some((pts, running, epoch)) if epoch == state.epoch => (pts, running),
                // The renderer has not reached this epoch's samples yet.
                _ => (state.target, false),
            };
        }
        (state.mono.now(at), state.mono.running)
    }

    fn set_phase(&self, state: &mut State, phase: Phase) -> bool {
        if state.status.phase == phase {
            return false;
        }
        state.status.phase = phase;
        true
    }
}

impl Timing for Inner {
    fn admit(&self, epoch: u32, pts: i64) -> Admit {
        // The decoder took a packet: there is room for the next.
        self.wake();
        let mut state = self.state.lock();
        loop {
            if state.closing || epoch != state.epoch {
                return Admit::Skip;
            }
            if pts + state.frame_micros / 2 < state.target {
                // On the way from the keyframe to the seek target.
                self.skipped.fetch_add(1, Ordering::Relaxed);
                return Admit::Skip;
            }
            let now = Instant::now();
            if !state.first_shown {
                // The picture of the position itself: shown at once, playing or not, and the
                // sound may start.
                state.first_shown = true;
                state.resyncs = 0;
                state.last_shown = pts;
                if !state.audio_clock {
                    state.mono = Mono {
                        pts: state.target,
                        at: now,
                        running: state.want_play,
                    };
                }
                if let Some(audio) = &state.audio {
                    audio.set_gate(true);
                }
                let had = std::mem::replace(&mut state.status.has_picture, true);
                let visible = state.visible;
                drop(state);
                if !had {
                    (self.on_change)();
                }
                return self.hand_over(visible, pts, now, 0);
            }
            if !state.want_play {
                self.due.wait(&mut state);
                continue;
            }
            let (clock, running) = self.clock(&state, now);
            if !running {
                // The sound is not flowing (starting, buffering): the renderer's note wakes us.
                self.due.wait(&mut state);
                continue;
            }
            let wait = pts - clock;
            if wait > 500 {
                self.due
                    .wait_for(&mut state, Duration::from_micros(wait as u64));
                continue;
            }
            let late = (-wait).max(0);
            state.last_shown = pts;
            if late > LATE_DROP.max(state.frame_micros * 2) {
                self.late.fetch_add(1, Ordering::Relaxed);
                return Admit::Skip;
            }
            let visible = state.visible;
            drop(state);
            return self.hand_over(visible, pts, now, late as u64);
        }
    }

    fn idle(&self) -> bool {
        let state = self.state.lock();
        state.closing || state.at_end || !state.want_play
    }

    fn keyframe_needed(&self) {
        self.state.lock().resync = true;
        self.wake();
    }

    fn shutdown(&self) {
        self.abort.store(true, Ordering::Relaxed);
        {
            let mut state = self.state.lock();
            state.closing = true;
            state.want_play = false;
        }
        self.due.notify_all();
        self.wake();
    }

    fn event(&self, event: StreamEvent) {
        let changed = {
            let mut state = self.state.lock();
            match event {
                StreamEvent::DecoderOpened { backend, .. } => match &mut state.status.video {
                    Some(video) if video.backend != Some(backend) => {
                        video.backend = Some(backend);
                        true
                    }
                    _ => false,
                },
                StreamEvent::Picture { width, height } => match &mut state.status.video {
                    Some(video) if (video.width, video.height) != (width, height) => {
                        video.width = width;
                        video.height = height;
                        true
                    }
                    _ => false,
                },
                _ => false,
            }
        };
        if changed {
            (self.on_change)();
        }
    }
}

impl Inner {
    fn hand_over(&self, visible: bool, pts: i64, now: Instant, late: u64) -> Admit {
        if !visible {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return Admit::Skip;
        }
        self.shown.fetch_add(1, Ordering::Relaxed);
        self.max_late.fetch_max(late, Ordering::Relaxed);
        if let Some(presented) = self.presented.lock().as_ref() {
            presented(pts, now);
        }
        Admit::Show
    }
}

/// A file being played. Dropping it stops playback: the sound fades out, the picture goes.
pub struct Player {
    inner: Arc<Inner>,
    viewer: Option<Viewer>,
    hub: Option<(VideoHub, String)>,
}

impl Player {
    pub fn open(open: Open) -> Player {
        let abort = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                want_play: open.play,
                seek: None,
                closing: false,
                visible: true,
                volume: open.volume,
                muted: open.muted,
                looping: false,
                resync: false,
                epoch: 1,
                target: open.start_at.max(0),
                first_shown: false,
                mono: Mono {
                    pts: open.start_at.max(0),
                    at: Instant::now(),
                    running: false,
                },
                audio_clock: false,
                audio: None,
                frame_micros: 40_000,
                at_end: false,
                status: Status {
                    phase: Phase::Loading,
                    duration: 0,
                    video: None,
                    has_audio: false,
                    has_picture: false,
                },
                resyncs: 0,
                last_shown: open.start_at.max(0),
            }),
            due: Condvar::new(),
            control: std::sync::OnceLock::new(),
            abort: abort.clone(),
            on_change: open.on_change,
            shown: AtomicU64::new(0),
            late: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            max_late: AtomicU64::new(0),
            seeks: AtomicU64::new(0),
            streams_opened: AtomicU64::new(0),
            presented: Mutex::new(None),
        });
        // The stream is timed before anything is pushed or shown.
        let (stream, viewer, hub) = match open.video {
            Some(hub) => {
                let stream = hub.stream(&open.key);
                stream.set_timing(Some(inner.clone() as Arc<dyn Timing>));
                let viewer = stream.viewer();
                (Some(stream), Some(viewer), Some((hub, open.key.clone())))
            }
            None => (None, None, None),
        };
        let spawned = std::thread::Builder::new().name("decibell-media".into()).spawn({
            let inner = inner.clone();
            let (source, output) = (open.source, open.output);
            move || {
                let _ = inner.control.set(std::thread::current());
                Control::run(inner, source, output, stream);
            }
        });
        if let Err(error) = spawned {
            let mut state = inner.state.lock();
            state.status.phase = Phase::Failed(format!("Couldn't start the player ({error})"));
        }
        Player { inner, viewer, hub }
    }

    pub fn status(&self) -> Status {
        self.inner.state.lock().status.clone()
    }

    /// What a [`decibell_video::VideoView`] paints, when the player has a picture.
    pub fn viewer(&self) -> Option<&Viewer> {
        self.viewer.as_ref()
    }

    /// Where playback is, in microseconds.
    pub fn position(&self) -> i64 {
        let state = self.inner.state.lock();
        let duration = state.status.duration;
        let position = match state.status.phase {
            Phase::Ended if duration > 0 => duration,
            _ => {
                if let Some(seek) = state.seek {
                    seek
                } else {
                    self.inner.clock(&state, Instant::now()).0
                }
            }
        };
        if duration > 0 { position.clamp(0, duration) } else { position.max(0) }
    }

    pub fn is_playing(&self) -> bool {
        self.inner.state.lock().want_play
    }

    pub fn play(&self) {
        self.command(|state| state.want_play = true);
    }

    pub fn pause(&self) {
        self.command(|state| state.want_play = false);
    }

    pub fn toggle(&self) {
        self.command(|state| state.want_play = !state.want_play);
    }

    /// Moves to `micros` (clamped to the file), exactly: the picture and the sample at that
    /// time, as setting `HTMLMediaElement.currentTime` does. Seeks that arrive while one is in
    /// progress replace each other (a scrub bar being dragged).
    pub fn seek(&self, micros: i64) {
        self.command(|state| {
            let limit = if state.status.duration > 0 { state.status.duration } else { i64::MAX };
            state.seek = Some(micros.clamp(0, limit));
        });
    }

    pub fn set_volume(&self, volume: f32, muted: bool) {
        self.command(|state| {
            state.volume = volume.clamp(0.0, 1.0);
            state.muted = muted;
            if let Some(audio) = &state.audio {
                audio.set_volume(if muted { 0.0 } else { state.volume });
            }
        });
    }

    /// Whether anything on screen shows the picture. Hidden, the file plays on (sound, clock,
    /// decode) and no picture is copied or presented.
    pub fn set_visible(&self, visible: bool) {
        self.command(|state| state.visible = visible);
    }

    pub fn set_looping(&self, looping: bool) {
        self.command(|state| state.looping = looping);
    }

    pub fn stats(&self) -> PlayStats {
        let inner = &self.inner;
        PlayStats {
            shown: inner.shown.load(Ordering::Relaxed),
            late: inner.late.load(Ordering::Relaxed),
            skipped: inner.skipped.load(Ordering::Relaxed),
            max_late: inner.max_late.load(Ordering::Relaxed),
            seeks: inner.seeks.load(Ordering::Relaxed),
            streams_opened: inner.streams_opened.load(Ordering::Relaxed),
        }
    }

    /// Tests: called on the decoder thread with every shown picture's media time.
    #[doc(hidden)]
    pub fn on_presented(&self, hook: impl Fn(i64, Instant) + Send + 'static) {
        *self.inner.presented.lock() = Some(Box::new(hook));
    }

    fn command(&self, change: impl FnOnce(&mut State)) {
        {
            let mut state = self.inner.state.lock();
            change(&mut state);
        }
        self.inner.due.notify_all();
        self.inner.wake();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.inner.abort.store(true, Ordering::Relaxed);
        self.command(|state| {
            state.closing = true;
            state.want_play = false;
        });
        // The decoder thread is out of `admit` now; the picture goes with its last viewer.
        self.viewer = None;
        if let Some((hub, key)) = self.hub.take() {
            hub.remove(&key);
        }
        // The control thread fades the sound out, closes the device and ends on its own.
    }
}

/// What the control thread owns.
struct Control {
    inner: Arc<Inner>,
    demuxer: Demuxer,
    stream: Option<Stream>,
    video: Option<(Codec, Option<Arc<[u8]>>)>,
    stash: VecDeque<(Packet, bool, i64)>,
    /// The end marker still has to be pushed (after the stash).
    end_pending: bool,
    audio: Option<Sound>,
    output: Arc<dyn AudioOutput>,
    at_end: bool,
    epoch: u32,
    playing: bool,
    /// When the monotonic clock took over from a sound track that ended before the picture.
    last_video_pts: i64,
    duration: i64,
}

struct Sound {
    decoder: AudioDecoder,
    feed: Feed,
    cell: RendererCell,
    device: Option<Box<dyn OutputStream>>,
    /// Resampled frames waiting for room in the ring.
    backlog: Vec<f32>,
    backlog_at: usize,
    /// The media time the ring's next frame is at.
    next_pts: i64,
    finished: bool,
    drained: bool,
    /// The decoder has been sent the end and gives what it still holds.
    flushing: bool,
}

impl Control {
    fn run(inner: Arc<Inner>, source: Box<dyn Source>, output: Arc<dyn AudioOutput>, stream: Option<Stream>) {
        let fail = |inner: &Inner, reason: String| {
            log::warn!("media: {reason}");
            let mut state = inner.state.lock();
            if !state.closing {
                state.status.phase = Phase::Failed(reason);
                state.want_play = false;
                drop(state);
                (inner.on_change)();
            }
        };
        let mut demuxer = match Demuxer::open(source, inner.abort.clone()) {
            Ok(demuxer) => demuxer,
            Err(error) => return fail(&inner, error.to_string()),
        };
        if stream.is_none() {
            demuxer.drop_video();
            if demuxer.audio.is_none() {
                return fail(&inner, OpenError::Format("no audio in it".into()).to_string());
            }
        }
        // The sound: its decoder at the device's rate, the ring, the renderer.
        let mut sound = None;
        if let Some((parameters, time_base, start)) = demuxer.audio_parameters() {
            let opened = output.rate().and_then(|rate| {
                let decoder = AudioDecoder::open(parameters, time_base, start, rate).map_err(|error| error.to_string())?;
                let waker = inner.clone();
                let shared = Shared::new(rate, Box::new(move || waker.wake()));
                Ok(Sound {
                    decoder,
                    cell: Arc::new(Mutex::new(Renderer::new(shared.clone()))),
                    feed: Feed::new(shared),
                    device: None,
                    backlog: Vec::new(),
                    backlog_at: 0,
                    next_pts: 0,
                    finished: false,
                    drained: false,
                    flushing: false,
                })
            });
            match opened {
                Ok(opened) => sound = Some(opened),
                Err(reason) if demuxer.video.is_some() && stream.is_some() => {
                    // A video plays without its sound rather than not at all.
                    log::warn!("media: playing without sound: {reason}");
                }
                Err(reason) => return fail(&inner, reason),
            }
        }
        let video = demuxer.video.clone();
        let (duration, target) = {
            let mut state = inner.state.lock();
            state.status.duration = demuxer.duration;
            state.status.has_audio = sound.is_some();
            state.status.video = video.as_ref().map(|video| VideoInfo {
                codec: video.codec,
                width: video.width,
                height: video.height,
                frame_rate: video.frame_rate,
                backend: None,
            });
            if let Some(video) = &video
                && video.frame_rate > 1.0
            {
                state.frame_micros = (1_000_000.0 / video.frame_rate) as i64;
            }
            if let Some(sound) = &sound {
                let shared = sound.feed.shared();
                shared.set_volume(if state.muted { 0.0 } else { state.volume });
                state.audio = Some(shared.clone());
                state.audio_clock = true;
            }
            if demuxer.duration > 0 && state.target >= demuxer.duration {
                state.target = 0;
            }
            state.status.phase = if state.want_play { Phase::Playing } else { Phase::Paused };
            (demuxer.duration, state.target)
        };
        (inner.on_change)();
        let mut control = Control {
            inner,
            demuxer,
            stream,
            video: video.map(|video| (video.codec, video.description)),
            stash: VecDeque::with_capacity(VIDEO_STASH + 1),
            end_pending: false,
            audio: sound,
            output,
            at_end: false,
            epoch: 0,
            playing: false,
            last_video_pts: 0,
            duration,
        };
        // The first position is a seek like any other (epoch 1).
        if let Err(error) = control.go_to(target, 1) {
            return fail(&control.inner, error.to_string());
        }
        if let Err(reason) = control.work() {
            fail(&control.inner, reason);
        }
        control.close();
    }

    /// The loop: commands, notes, then as much reading as there is room for, then sleep.
    fn work(&mut self) -> Result<(), String> {
        loop {
            // Commands.
            let (seek, resync, want_play, closing) = {
                let mut state = self.inner.state.lock();
                (state.seek.take(), std::mem::take(&mut state.resync), state.want_play, state.closing)
            };
            if closing {
                return Ok(());
            }
            if let Some(target) = seek {
                self.seek_to(target)?;
            } else if resync {
                let (position, resyncs) = {
                    let mut state = self.inner.state.lock();
                    state.resyncs += 1;
                    (self.inner.clock(&state, Instant::now()).0, state.resyncs)
                };
                if resyncs > MAX_RESYNCS {
                    return Err("This video can't be decoded on this device".into());
                }
                self.seek_to(position)?;
            }
            if want_play != self.playing {
                self.set_playing(want_play)?;
            }
            self.notes();
            // Read while there is room for what is read.
            let mut worked = false;
            loop {
                let pushed = self.push_video()?;
                let wrote = self.write_audio();
                let read = self.read()?;
                if !(pushed || wrote || read) {
                    break;
                }
                worked = true;
                if self.inner.state.lock().seek.is_some() {
                    break;
                }
            }
            if self.ended() {
                continue;
            }
            if worked {
                continue;
            }
            if let Some(audio) = &self.audio
                && audio.backlog_at < audio.backlog.len()
            {
                audio.feed.want_room();
                // The note may have been due between the last write and the request.
                if audio.feed.room() > 0 && audio.feed.buffered() <= audio.feed.shared().rate() as usize / 2 {
                    continue;
                }
            }
            match self.tail_wait() {
                Some(wait) => std::thread::park_timeout(wait),
                None => std::thread::park(),
            }
        }
    }

    /// A video whose sound has ended runs on the monotonic clock to its last picture: one
    /// sleep until then.
    fn tail_wait(&self) -> Option<Duration> {
        if !self.at_end || !self.playing {
            return None;
        }
        let state = self.inner.state.lock();
        if state.audio_clock {
            return None;
        }
        let end = self.end_pts(&state);
        let (now, running) = self.inner.clock(&state, Instant::now());
        (running && end > now).then(|| Duration::from_micros((end - now) as u64))
    }

    fn end_pts(&self, state: &State) -> i64 {
        (self.last_video_pts + state.frame_micros).max(self.duration)
    }

    /// Whether the file has played to its end (and what follows from that was done).
    fn ended(&mut self) -> bool {
        if !self.at_end || !self.playing || !self.stash.is_empty() || self.end_pending {
            return false;
        }
        let sound_done = self.audio.as_ref().is_none_or(|audio| audio.drained);
        if !sound_done {
            return false;
        }
        let looping = {
            let mut state = self.inner.state.lock();
            if state.audio_clock && self.video.is_some() {
                // The sound ended before the picture: the picture goes on by the wall clock
                // from where the sound stopped.
                let (pts, _) = self.inner.clock(&state, Instant::now());
                state.audio_clock = false;
                state.mono = Mono {
                    pts,
                    at: Instant::now(),
                    running: true,
                };
                drop(state);
                self.inner.due.notify_all();
                return true;
            }
            if self.video.is_some() {
                let (now, _) = self.inner.clock(&state, Instant::now());
                if now < self.end_pts(&state) {
                    return false;
                }
            }
            state.looping
        };
        if looping {
            return self.seek_to(0).is_ok();
        }
        self.playing = false;
        {
            let mut state = self.inner.state.lock();
            state.want_play = false;
            state.mono.running = false;
            state.status.phase = Phase::Ended;
        }
        if let Some(audio) = &self.audio {
            audio.feed.shared().set_playing(false);
        }
        self.inner.due.notify_all();
        (self.inner.on_change)();
        true
    }

    fn set_playing(&mut self, playing: bool) -> Result<(), String> {
        if playing && self.inner.state.lock().status.phase == Phase::Ended {
            // Play at the end starts again from the beginning.
            self.playing = true;
            self.seek_to(0)?;
        }
        self.playing = playing;
        {
            let mut state = self.inner.state.lock();
            let now = Instant::now();
            let position = state.mono.now(now);
            state.mono = Mono {
                pts: position,
                at: now,
                running: playing && state.first_shown,
            };
            let phase = if playing { Phase::Playing } else { Phase::Paused };
            if self.inner.set_phase(&mut state, phase) {
                drop(state);
                (self.inner.on_change)();
            }
        }
        if let Some(audio) = &self.audio {
            audio.feed.shared().set_playing(playing);
        }
        if playing {
            self.ensure_device();
        }
        self.inner.due.notify_all();
        Ok(())
    }

    /// Opens the device stream when the renderer has something to do.
    fn ensure_device(&mut self) {
        let Some(audio) = &mut self.audio else { return };
        if audio.device.as_ref().is_some_and(|device| device.failed()) {
            // Unplugged, or the sound server went away: the renderer is not being called, so
            // it is as silent as it can be. The next stream starts from that silence.
            audio.device = None;
        }
        if audio.device.is_some() {
            return;
        }
        let rate = audio.feed.shared().rate();
        audio.cell.lock().restart();
        let waker = self.inner.clone();
        match self.output.open(audio.cell.clone(), rate, Arc::new(move || waker.wake())) {
            Ok(device) => {
                audio.device = Some(device);
                self.inner.streams_opened.fetch_add(1, Ordering::Relaxed);
            }
            Err(reason) => self.sound_lost(&reason),
        }
    }

    /// No device (none at all, or its rate is no longer the one the file was resampled to):
    /// the file goes on without sound, on the wall clock from where it is.
    fn sound_lost(&mut self, reason: &str) {
        log::warn!("media: playing on without sound: {reason}");
        self.audio = None;
        let mut state = self.inner.state.lock();
        let (pts, _) = self.inner.clock(&state, Instant::now());
        state.audio = None;
        state.audio_clock = false;
        state.mono = Mono {
            pts,
            at: Instant::now(),
            running: self.playing && state.first_shown,
        };
        drop(state);
        self.inner.due.notify_all();
    }

    /// What the renderer said since the last look.
    fn notes(&mut self) {
        let Some(audio) = &mut self.audio else { return };
        let shared = audio.feed.shared().clone();
        let notes = shared.take_notes();
        if notes & (note::CLOCK | note::FLUSHED) != 0 {
            // Pictures waiting for the clock look again.
            self.inner.due.notify_all();
        }
        if notes & note::DRAINED != 0 && audio.finished {
            audio.drained = true;
        }
        let failed = audio.device.as_ref().is_some_and(|device| device.failed());
        let wanted = self.playing && !audio.drained;
        if shared.is_quiet() && !failed {
            if wanted {
                // The renderer went quiet in the instant play was pressed: it is not.
                shared.set_playing(true);
            } else {
                // Silent for a quarter of a second and nothing to play: the device is closed.
                audio.device = None;
            }
        } else if failed || audio.device.is_none() {
            // Something to render (also a seek while paused: the renderer has to leave the old
            // position behind before the new one's samples fit), or the device has to be
            // opened again.
            self.ensure_device();
        }
    }

    fn seek_to(&mut self, target: i64) -> Result<(), String> {
        self.inner.seeks.fetch_add(1, Ordering::Relaxed);
        let epoch = self.epoch + 1;
        self.go_to(target, epoch).map_err(|error| error.to_string())?;
        let was_ended = {
            let mut state = self.inner.state.lock();
            let was_ended = state.status.phase == Phase::Ended;
            if was_ended {
                state.status.phase = if self.playing { Phase::Playing } else { Phase::Paused };
            }
            was_ended
        };
        if was_ended {
            (self.inner.on_change)();
        }
        Ok(())
    }

    /// Everything a new position needs, in the order that keeps old and new apart.
    fn go_to(&mut self, target: i64, epoch: u32) -> Result<(), OpenError> {
        self.epoch = epoch;
        {
            let mut state = self.inner.state.lock();
            state.epoch = epoch;
            state.target = target;
            state.first_shown = false;
            state.at_end = false;
            state.last_shown = target;
            state.audio_clock = self.audio.is_some();
            state.mono = Mono {
                pts: target,
                at: Instant::now(),
                running: false,
            };
            if let Some(audio) = &state.audio {
                // The sound waits for the position's picture.
                audio.set_gate(self.video.is_none());
            }
        }
        // Pictures of the old position waiting in `admit` leave.
        self.inner.due.notify_all();
        self.stash.clear();
        self.end_pending = false;
        self.at_end = false;
        if let Some(stream) = &self.stream {
            stream.reset_timed();
        }
        if let Some(audio) = &mut self.audio {
            audio.decoder.reset(target);
            audio.backlog.clear();
            audio.backlog_at = 0;
            audio.finished = false;
            audio.drained = false;
            audio.flushing = false;
            audio.next_pts = target;
            audio.feed.begin(epoch, target);
        }
        self.demuxer.seek(target)?;
        if self.audio.is_some() {
            self.ensure_device();
        }
        Ok(())
    }

    /// Offers the stash's first packet to the decoder's queue.
    fn push_video(&mut self) -> Result<bool, String> {
        let (Some(stream), Some((codec, description))) = (&self.stream, &self.video) else {
            return Ok(false);
        };
        let mut pushed = false;
        loop {
            let frame = match self.stash.pop_front() {
                Some((data, keyframe, timestamp)) => TimedFrame {
                    codec: *codec,
                    keyframe,
                    timestamp,
                    data,
                    description: description.clone(),
                    epoch: self.epoch,
                },
                None if self.end_pending => {
                    if stream.queued() >= decibell_video::TIMED_DEPTH {
                        return Ok(pushed);
                    }
                    self.end_pending = false;
                    TimedFrame::end(*codec, self.epoch)
                }
                None => return Ok(pushed),
            };
            let was_end = frame.data.is_empty();
            match stream.push_timed(frame) {
                TimedPush::Queued => pushed = true,
                TimedPush::Full(frame) => {
                    if was_end {
                        self.end_pending = true;
                    } else {
                        self.stash.push_front((frame.data, frame.keyframe, frame.timestamp));
                    }
                    return Ok(pushed);
                }
                TimedPush::NeedKeyframe => {
                    if !was_end {
                        // The decoder is gone (it idled out, or failed): start again from the
                        // keyframe before where we are.
                        self.inner.state.lock().resync = true;
                        return Ok(true);
                    }
                }
                TimedPush::Closed => return Ok(pushed),
            }
        }
    }

    /// Moves the backlog into the ring.
    fn write_audio(&mut self) -> bool {
        let Some(audio) = &mut self.audio else { return false };
        let mut wrote = false;
        if audio.backlog_at < audio.backlog.len() {
            let frames = audio.feed.write(&audio.backlog[audio.backlog_at..]);
            audio.backlog_at += frames * CHANNELS;
            wrote = frames > 0;
            if audio.backlog_at >= audio.backlog.len() {
                audio.backlog.clear();
                audio.backlog_at = 0;
            }
        }
        if audio.backlog.is_empty() && self.at_end && !audio.finished && !audio.flushing {
            audio.feed.finish();
            audio.finished = true;
            wrote = true;
        }
        wrote
    }

    /// Reads one packet if what it may turn into has somewhere to go.
    fn read(&mut self) -> Result<bool, String> {
        if self.at_end {
            return Ok(self.drain_audio()?);
        }
        if self.stash.len() >= VIDEO_STASH {
            return Ok(false);
        }
        if let Some(audio) = &self.audio
            && audio.backlog.len() - audio.backlog_at > audio.feed.shared().rate() as usize * CHANNELS / 2
        {
            return Ok(false);
        }
        match self.demuxer.read().map_err(|error| error.to_string())? {
            Read::Video { packet, keyframe, pts } => {
                self.last_video_pts = self.last_video_pts.max(pts);
                self.stash.push_back((packet, keyframe, pts));
            }
            Read::Audio { pts } => {
                let packet = self.demuxer.packet();
                if let Some(audio) = &mut self.audio {
                    audio.decoder.send(packet)?;
                    Self::receive(audio, pts)?;
                }
            }
            Read::End => {
                self.at_end = true;
                self.end_pending = self.video.is_some();
                self.inner.state.lock().at_end = true;
                if let Some(audio) = &mut self.audio {
                    audio.decoder.send(std::ptr::null())?;
                    audio.flushing = true;
                }
            }
        }
        Ok(true)
    }

    /// After the last packet: what the audio decoder still holds.
    fn drain_audio(&mut self) -> Result<bool, String> {
        let Some(audio) = &mut self.audio else { return Ok(false) };
        if !audio.flushing {
            return Ok(false);
        }
        Self::receive(audio, None)?;
        audio.flushing = false;
        Ok(true)
    }

    /// Takes the decoder's frames into the backlog, with silence where the track has a gap
    /// (a track that starts after the picture does).
    fn receive(audio: &mut Sound, packet_pts: Option<i64>) -> Result<(), String> {
        let rate = audio.feed.shared().rate() as i64;
        while let Some(decoded) = audio.decoder.receive(packet_pts)? {
            let gap = decoded.pts - audio.next_pts;
            if gap > 20_000 {
                let frames = (gap * rate / 1_000_000).min(rate * 30) as usize;
                audio.backlog.resize(audio.backlog.len() + frames * CHANNELS, 0.0);
                audio.next_pts += frames as i64 * 1_000_000 / rate;
            }
            audio.backlog.extend_from_slice(decoded.samples);
            audio.next_pts += (decoded.samples.len() / CHANNELS) as i64 * 1_000_000 / rate;
        }
        Ok(())
    }

    /// The player is going: fade out, wait for the device to have played the fade, close it.
    fn close(mut self) {
        if let Some(stream) = &self.stream {
            stream.set_timing(None);
        }
        if let Some(audio) = &mut self.audio {
            let shared = audio.feed.shared().clone();
            shared.set_playing(false);
            if audio.device.is_some() {
                let started = Instant::now();
                while !shared.is_quiet() && started.elapsed() < Duration::from_secs(2) {
                    std::thread::park_timeout(Duration::from_millis(300));
                }
            }
            audio.device = None;
        }
    }
}
