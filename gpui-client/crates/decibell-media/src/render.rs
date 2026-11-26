//! The player's sound between its decoder and the device: a lock-free ring of decoded stereo
//! samples, the envelope that makes every transition silent, and the audio clock.
//!
//! One producer (the player's control thread: [`Feed`]) and one consumer (the device's callback:
//! [`Renderer::render`]) share a [`Shared`]. Nothing here blocks, locks or allocates on the
//! consumer's side.
//!
//! # No pops
//!
//! The device is only ever handed `sample × shape(envelope) × volume`, and the envelope and the
//! volume are **slewed**: neither moves by more than `1 / ramp` per sample ([`RAMP_MS`], 10 ms
//! from silence to full), and `shape` is a raised cosine (no corner at either end). What the
//! envelope heads for is decided per sample:
//!
//! * 0 while the player is paused, while the gate is shut (a seek's first picture is not up
//!   yet) and while samples of an older epoch are being left behind (a seek);
//! * 0 as soon as the samples left in the ring are only enough to finish the way down from
//!   where the envelope is: **the end of the file and an underrun are the same case**, and
//!   both reach zero on the last sample there is, whatever that sample's value;
//! * 1 otherwise, starting from silence only once [`REBUFFER_MS`] of sound are buffered (or
//!   the end of the file is known).
//!
//! So starting, pausing, resuming, seeking, ending, running dry, a volume or mute change and a
//! change of device (the old device's renderer is paused down to zero and the new one starts
//! from zero) are all the same continuous curve. Samples are consumed only while the envelope
//! is above zero, so nothing is skipped by a pause: the fade-out is the next 10 ms of the
//! file, and playback resumes on the sample after it. The tests at the bottom render each
//! transition from a full-scale constant (the worst case: any gap is a step) and from a sine,
//! and check every sample.
//!
//! # The clock
//!
//! After each callback the renderer publishes "the sample at media time `pts` reaches the
//! device at `instant`" and whether it is consuming. [`Shared::now`] reads that: the media
//! time of the sample being heard now. It stands still while the envelope is at zero.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Samples are interleaved stereo everywhere between the decoder and the device adapter.
pub const CHANNELS: usize = 2;
/// Silence to full level, and back.
pub const RAMP_MS: u32 = 10;
/// What the ring holds.
pub const RING_MS: u32 = 1000;
/// Sound buffered before playback starts from silence (the start, after a seek, after an
/// underrun), unless the end of the file is known.
pub const REBUFFER_MS: u32 = 100;
/// Silence rendered after the envelope reached zero before the device may be paused: more than
/// any device buffers, so the fade-out has been played out.
pub const SETTLE_MS: u32 = 250;

const NO_END: u64 = u64::MAX;
const RELAXED: Ordering = Ordering::Relaxed;

/// What the callback tells the control thread (bits of [`Shared::take_notes`]).
pub mod note {
    /// The clock started or stopped.
    pub const CLOCK: u32 = 1;
    /// Every sample up to the end of the file has been played.
    pub const DRAINED: u32 = 2;
    /// The ring is half empty and the producer was waiting for room.
    pub const ROOM: u32 = 4;
    /// Nothing to play and the tail has been played out: the device may be paused.
    pub const QUIET: u32 = 8;
    /// The samples of the current epoch are the ones being rendered (a seek took effect).
    pub const FLUSHED: u32 = 16;
}

pub struct Shared {
    rate: u32,
    buffer: Box<[UnsafeCell<f32>]>,
    /// Frames; a power of two.
    capacity: u64,
    /// Absolute frame counts.
    write: AtomicU64,
    read: AtomicU64,
    // The newest segment (written by the producer under `segment_seq`).
    segment_seq: AtomicU32,
    segment_epoch: AtomicU32,
    segment_start: AtomicU64,
    segment_pts: AtomicI64,
    /// The absolute frame the file ends at, once the producer knows.
    end: AtomicU64,
    playing: AtomicBool,
    gate: AtomicBool,
    volume: AtomicU32,
    // The clock (written by the consumer under `clock_seq`).
    clock_seq: AtomicU32,
    clock_pts: AtomicI64,
    clock_at: AtomicI64,
    clock_running: AtomicBool,
    clock_epoch: AtomicU32,
    origin: Instant,
    notes: AtomicU32,
    wants_room: AtomicBool,
    quiet: AtomicBool,
    // A pending resynchronisation (`sync_to`): "media time `sync_pts` is due at `sync_at`".
    /// The output device is being changed: the envelope goes to zero and stays there.
    moving: AtomicBool,
    sync_on: AtomicBool,
    sync_pts: AtomicI64,
    sync_at: AtomicI64,
    wake: Box<dyn Fn() + Send + Sync>,
}

// Safety: the buffer's cells are written only by the producer in `[write, read + capacity)`
// and read only by the consumer in `[read, write)`; the indices are atomics.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    /// `wake` is called from the device's callback when it has a note for the control thread
    /// (a state change, never per block): it must not block (`Thread::unpark`).
    pub fn new(rate: u32, wake: Box<dyn Fn() + Send + Sync>) -> Arc<Shared> {
        let frames = (rate as u64 * RING_MS as u64 / 1000).next_power_of_two();
        let buffer = (0..frames as usize * CHANNELS)
            .map(|_| UnsafeCell::new(0.0))
            .collect();
        Arc::new(Shared {
            rate,
            buffer,
            capacity: frames,
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            segment_seq: AtomicU32::new(0),
            segment_epoch: AtomicU32::new(0),
            segment_start: AtomicU64::new(0),
            segment_pts: AtomicI64::new(0),
            end: AtomicU64::new(NO_END),
            playing: AtomicBool::new(false),
            gate: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            clock_seq: AtomicU32::new(0),
            clock_pts: AtomicI64::new(0),
            clock_at: AtomicI64::new(0),
            clock_running: AtomicBool::new(false),
            clock_epoch: AtomicU32::new(0),
            origin: Instant::now(),
            notes: AtomicU32::new(0),
            wants_room: AtomicBool::new(false),
            quiet: AtomicBool::new(true),
            moving: AtomicBool::new(false),
            sync_on: AtomicBool::new(false),
            sync_pts: AtomicI64::new(0),
            sync_at: AtomicI64::new(0),
            wake,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Whether the player wants sound (false: the envelope goes to zero and stays).
    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, RELAXED);
        if playing {
            self.quiet.store(false, RELAXED);
        }
    }

    /// Shut: the sound waits (at zero) for the picture a seek is decoding towards.
    pub fn set_gate(&self, open: bool) {
        self.gate.store(open, RELAXED);
    }

    /// The sound went on without us for a while (the output device was changed: the player
    /// ran on the wall clock meanwhile): when the renderer next starts from silence, it starts
    /// on the sample that is due then, given that media time `pts` (microseconds) was due at
    /// `at`, and leaves the samples before it unplayed. It waits in silence for as long as that
    /// sample is not in the ring (not yet decoded) or already behind what was played. Skipping
    /// happens at envelope zero only: what is handed to the device stays one continuous curve.
    pub fn sync_to(&self, pts: i64, at: Instant) {
        self.sync_pts.store(pts, RELAXED);
        self.sync_at.store(self.ns(at), RELAXED);
        self.sync_on.store(true, Ordering::Release);
    }

    /// Whether a [`Self::sync_to`] has not taken effect yet.
    pub fn sync_pending(&self) -> bool {
        self.sync_on.load(Ordering::Acquire)
    }

    /// Forgets a pending [`Self::sync_to`] (a seek: the new position starts as any other).
    pub fn cancel_sync(&self) {
        self.sync_on.store(false, Ordering::Release);
    }

    /// True while the player moves to another output device: the sound fades to zero (the
    /// next 10 ms of the file, as a pause) and the renderer reports quiet once the old device
    /// has played that out. False again: it starts from silence on whatever calls it next.
    pub fn set_moving(&self, moving: bool) {
        self.moving.store(moving, RELAXED);
        if !moving {
            self.quiet.store(false, RELAXED);
        }
    }

    /// Linear gain, 0.0 to 1.0 (what `HTMLMediaElement.volume` is; muted is 0).
    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), RELAXED);
    }

    /// The notes the callback left since the last call.
    pub fn take_notes(&self) -> u32 {
        self.notes.swap(0, Ordering::AcqRel)
    }

    /// Whether the renderer has nothing to do and its tail has been played out.
    pub fn is_quiet(&self) -> bool {
        self.quiet.load(RELAXED)
    }

    fn note(&self, bits: u32) {
        self.notes.fetch_or(bits, Ordering::AcqRel);
        (self.wake)();
    }

    fn ns(&self, instant: Instant) -> i64 {
        instant.saturating_duration_since(self.origin).as_nanos() as i64
    }

    fn segment(&self) -> (u32, u64, i64) {
        loop {
            let before = self.segment_seq.load(Ordering::Acquire);
            let segment = (
                self.segment_epoch.load(RELAXED),
                self.segment_start.load(RELAXED),
                self.segment_pts.load(RELAXED),
            );
            if before & 1 == 0 && self.segment_seq.load(Ordering::Acquire) == before {
                return segment;
            }
            std::hint::spin_loop();
        }
    }

    /// The media time (microseconds) of the sample reaching the device at `at`, whether the
    /// clock is running, and the epoch it belongs to. `None` before the first callback.
    pub fn now(&self, at: Instant) -> Option<(i64, bool, u32)> {
        loop {
            let before = self.clock_seq.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            let pts = self.clock_pts.load(RELAXED);
            let anchor = self.clock_at.load(RELAXED);
            let running = self.clock_running.load(RELAXED);
            let epoch = self.clock_epoch.load(RELAXED);
            if before & 1 == 0 && self.clock_seq.load(Ordering::Acquire) == before {
                let pts = if running {
                    pts + (self.ns(at) - anchor) / 1000
                } else {
                    pts
                };
                return Some((pts, running, epoch));
            }
            std::hint::spin_loop();
        }
    }
}

/// The producer's end.
pub struct Feed {
    shared: Arc<Shared>,
}

impl Feed {
    pub fn new(shared: Arc<Shared>) -> Feed {
        Feed { shared }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Frames that fit now.
    pub fn room(&self) -> usize {
        let shared = &*self.shared;
        (shared.capacity - (shared.write.load(RELAXED) - shared.read.load(Ordering::Acquire))) as usize
    }

    /// Frames waiting to be played.
    pub fn buffered(&self) -> usize {
        let shared = &*self.shared;
        (shared.write.load(RELAXED) - shared.read.load(Ordering::Acquire)) as usize
    }

    /// Asks for [`note::ROOM`] when the ring is next half empty.
    pub fn want_room(&self) {
        self.shared.wants_room.store(true, RELAXED);
    }

    /// Everything written from here on belongs to `epoch` and starts at media time `pts`
    /// (microseconds). What was written before is left behind by the renderer: faded out over
    /// the next [`RAMP_MS`] and dropped. Epochs start at 1.
    pub fn begin(&mut self, epoch: u32, pts: i64) {
        let shared = &*self.shared;
        shared.end.store(NO_END, RELAXED);
        shared.segment_seq.fetch_add(1, Ordering::AcqRel);
        shared.segment_epoch.store(epoch, RELAXED);
        shared
            .segment_start
            .store(shared.write.load(RELAXED), RELAXED);
        shared.segment_pts.store(pts, RELAXED);
        shared.segment_seq.fetch_add(1, Ordering::AcqRel);
        shared.quiet.store(false, RELAXED);
    }

    /// Writes interleaved stereo frames; returns how many frames fitted.
    pub fn write(&mut self, samples: &[f32]) -> usize {
        let shared = &*self.shared;
        let frames = (samples.len() / CHANNELS).min(self.room());
        let write = shared.write.load(RELAXED);
        let mask = shared.capacity - 1;
        for frame in 0..frames {
            let at = ((write + frame as u64) & mask) as usize * CHANNELS;
            // Safety: frames in `[write, read + capacity)` are the producer's.
            unsafe {
                *shared.buffer[at].get() = samples[frame * CHANNELS];
                *shared.buffer[at + 1].get() = samples[frame * CHANNELS + 1];
            }
        }
        shared.write.store(write + frames as u64, Ordering::Release);
        frames
    }

    /// The file ends with what has been written.
    pub fn finish(&mut self) {
        let shared = &*self.shared;
        shared
            .end
            .store(shared.write.load(RELAXED), Ordering::Release);
    }
}

/// The consumer's end: owned by the device's callback (or handed from one device's to the
/// next's once it is quiet).
pub struct Renderer {
    shared: Arc<Shared>,
    /// 0.0 silence to 1.0 full, slewed.
    envelope: f32,
    volume: f32,
    epoch: u32,
    start: u64,
    pts: i64,
    /// The last frame handed out before gains: what is faded if the data ever ends under a
    /// raised envelope (it cannot by construction; this keeps even that continuous).
    last: [f32; 2],
    ramp: f32,
    rebuffer: u64,
    settle: u64,
    silent_frames: u64,
    running: bool,
    drained: bool,
}

impl Renderer {
    pub fn new(shared: Arc<Shared>) -> Renderer {
        let rate = shared.rate as u64;
        Renderer {
            envelope: 0.0,
            volume: f32::from_bits(shared.volume.load(RELAXED)),
            epoch: 0,
            start: 0,
            pts: 0,
            last: [0.0; 2],
            ramp: (rate * RAMP_MS as u64 / 1000).max(1) as f32,
            rebuffer: rate * REBUFFER_MS as u64 / 1000,
            settle: rate * SETTLE_MS as u64 / 1000,
            silent_frames: 0,
            running: false,
            drained: false,
            shared,
        }
    }

    /// Fills `out` (interleaved stereo). `plays_at`: when its first frame reaches the device.
    pub fn render(&mut self, out: &mut [f32], plays_at: Instant) {
        let shared = &*self.shared;
        let (epoch, start, pts) = shared.segment();
        let write = shared.write.load(Ordering::Acquire);
        let mut read = shared.read.load(RELAXED);
        let wanted = shared.playing.load(RELAXED) && shared.gate.load(RELAXED) && !shared.moving.load(RELAXED);
        let target_volume = f32::from_bits(shared.volume.load(RELAXED));
        let mut flushing = epoch != self.epoch;
        let mut notes = 0;
        // Old samples end where the new segment starts.
        let mut limit = if flushing { write.min(start) } else { write };
        let mut end = if flushing { NO_END } else { shared.end.load(Ordering::Acquire) };
        if end != NO_END {
            limit = limit.min(end);
        }
        let step = 1.0 / self.ramp;
        let mut consumed = false;
        // A pending resynchronisation: from silence, move to the sample due when this block
        // is heard, or stay silent for this block when it cannot be reached yet.
        let mut hold = false;
        if wanted && !flushing && self.envelope == 0.0 && self.epoch != 0 && shared.sync_on.load(Ordering::Acquire) {
            let due = shared.sync_pts.load(RELAXED) + (shared.ns(plays_at) - shared.sync_at.load(RELAXED)) / 1000;
            let frame = self.start as i128 + (due - self.pts) as i128 * shared.rate as i128 / 1_000_000;
            if frame < read as i128 {
                // What is due was played already (the clock is behind the ring): wait for it.
                hold = true;
            } else if frame as u64 + if end == NO_END { self.rebuffer } else { 0 } > limit {
                // Not decoded yet: what is buffered before it will never be played, and leaving
                // it makes room for the producer to reach the sample that is due.
                read = read.max((frame as u64).min(limit));
                hold = true;
            } else {
                read = frame as u64;
                shared.sync_on.store(false, Ordering::Release);
            }
        }

        for frame in out.as_chunks_mut::<CHANNELS>().0 {
            if flushing && self.envelope == 0.0 {
                // The old position is silent: leave it.
                read = start;
                self.epoch = epoch;
                self.start = start;
                self.pts = pts;
                self.drained = false;
                flushing = false;
                end = shared.end.load(Ordering::Acquire);
                limit = if end != NO_END { write.min(end) } else { write };
                notes |= note::FLUSHED;
            }
            let remaining = limit - read;
            // The way down from here takes `envelope × ramp` samples: once no more are left,
            // it must have started.
            let tail = remaining as f32 <= (self.envelope * self.ramp).ceil();
            // From silence it starts only with enough buffered to be worth starting (or the
            // end of the file in sight).
            let starved = self.envelope == 0.0 && remaining < self.rebuffer && end == NO_END;
            let target = if !wanted || flushing || tail || self.epoch == 0 || starved || hold { 0.0 } else { 1.0 };
            if target > self.envelope {
                self.envelope = (self.envelope + step).min(1.0);
            } else if target < self.envelope {
                self.envelope -= step;
                if self.envelope < step * 0.5 {
                    self.envelope = 0.0;
                }
            }
            if self.volume < target_volume {
                self.volume = (self.volume + step).min(target_volume);
            } else if self.volume > target_volume {
                self.volume = (self.volume - step).max(target_volume);
            }
            if self.envelope > 0.0 {
                if remaining > 0 {
                    let at = (read & (shared.capacity - 1)) as usize * CHANNELS;
                    // Safety: frames in `[read, write)` are the consumer's.
                    self.last = unsafe { [*shared.buffer[at].get(), *shared.buffer[at + 1].get()] };
                    read += 1;
                    consumed = true;
                }
                let gain = shape(self.envelope) * self.volume;
                frame[0] = self.last[0] * gain;
                frame[1] = self.last[1] * gain;
                self.silent_frames = 0;
            } else {
                frame[0] = 0.0;
                frame[1] = 0.0;
                self.silent_frames += 1;
            }
        }
        shared.read.store(read, Ordering::Release);

        // The clock: where the next sample is in the file, and when it is heard.
        let frames = (out.len() / CHANNELS) as u64;
        let running = self.envelope > 0.0 && consumed;
        let position = self.pts + ((read - self.start) as i128 * 1_000_000 / shared.rate as i128) as i64;
        let heard_at = shared.ns(plays_at) + (frames * 1_000_000_000 / shared.rate as u64) as i64;
        if self.epoch != 0 {
            shared.clock_seq.fetch_add(1, Ordering::AcqRel);
            shared.clock_pts.store(position, RELAXED);
            shared.clock_at.store(heard_at, RELAXED);
            shared.clock_running.store(running, RELAXED);
            shared.clock_epoch.store(self.epoch, RELAXED);
            shared.clock_seq.fetch_add(1, Ordering::AcqRel);
        }
        if running != self.running {
            self.running = running;
            notes |= note::CLOCK;
        }
        if !self.drained && !flushing && end != NO_END && read >= end && self.envelope == 0.0 {
            self.drained = true;
            notes |= note::DRAINED;
        }
        if shared.wants_room.load(RELAXED)
            && write - read <= shared.capacity / 2
            && shared.wants_room.swap(false, RELAXED)
        {
            notes |= note::ROOM;
        }
        let idle = self.envelope == 0.0 && !flushing && (!wanted || self.drained);
        if idle && self.silent_frames >= self.settle && !shared.quiet.swap(true, RELAXED) {
            notes |= note::QUIET;
        }
        if notes != 0 {
            shared.note(notes);
        }
    }

    /// A new device stream is about to call this renderer after the old one died with sound
    /// in it (unplugged): the new device starts from silence and ramps, whatever the old one
    /// was cut off at.
    pub fn restart(&mut self) {
        self.envelope = 0.0;
        self.last = [0.0; 2];
    }

    /// Whether the envelope is at zero (nothing but silence is being handed out).
    pub fn is_silent(&self) -> bool {
        self.envelope == 0.0
    }
}

/// The envelope's curve: a raised cosine, flat at both ends.
fn shape(envelope: f32) -> f32 {
    if envelope >= 1.0 {
        1.0
    } else {
        0.5 - 0.5 * (std::f32::consts::PI * envelope).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const RATE: u32 = 48_000;
    const BLOCK: usize = 480;
    /// The steepest the raised cosine gets, per sample, at full scale: `π / 2 / ramp`.
    const MAX_STEP: f32 = std::f32::consts::FRAC_PI_2 / (RATE as f32 * RAMP_MS as f32 / 1000.0) * 1.02;

    struct Rig {
        feed: Feed,
        renderer: Renderer,
        out: Vec<f32>,
        clock: Instant,
        /// The source: frame `n` of the file is `source(n)`.
        source: fn(u64) -> f32,
        fed: u64,
    }

    fn dc(_: u64) -> f32 {
        1.0
    }

    fn sine(n: u64) -> f32 {
        (n as f32 * 2.0 * std::f32::consts::PI * 440.0 / RATE as f32).sin()
    }

    impl Rig {
        fn new(source: fn(u64) -> f32) -> Rig {
            let shared = Shared::new(RATE, Box::new(|| {}));
            let mut feed = Feed::new(shared.clone());
            feed.begin(1, 0);
            shared.set_gate(true);
            Rig {
                feed,
                renderer: Renderer::new(shared),
                out: Vec::new(),
                clock: Instant::now(),
                source,
                fed: 0,
            }
        }

        fn shared(&self) -> Arc<Shared> {
            self.feed.shared().clone()
        }

        /// Keeps the ring topped up to `frames` buffered.
        fn feed(&mut self, frames: usize) {
            while self.feed.buffered() < frames {
                let value = (self.source)(self.fed);
                if self.feed.write(&[value, value]) == 0 {
                    break;
                }
                self.fed += 1;
            }
        }

        /// Renders `blocks` device blocks, feeding first when `keep` is given.
        fn run(&mut self, blocks: usize, keep: Option<usize>) {
            for _ in 0..blocks {
                if let Some(keep) = keep {
                    self.feed(keep);
                }
                let mut block = [0.0f32; BLOCK * CHANNELS];
                self.renderer.render(&mut block, self.clock);
                self.clock += Duration::from_millis(10);
                self.out.extend(block.iter().step_by(CHANNELS));
            }
        }

        /// The largest jump between consecutive output samples from `from` on.
        fn max_step(&self, from: usize) -> f32 {
            self.out[from.saturating_sub(1)..]
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0, f32::max)
        }
    }

    /// The whole output never jumps by more than the ramp's steepest step (a constant source:
    /// every change in the output is the envelope's), and `check` sees it.
    fn assert_smooth(rig: &Rig, what: &str) {
        let step = rig.max_step(0);
        assert!(
            step <= MAX_STEP,
            "{what}: a jump of {step} between two samples (the ramp's steepest is {MAX_STEP})"
        );
        // The device had silence before the first sample it was handed.
        assert!(rig.out[0].abs() <= MAX_STEP, "{what}: starts from silence, not {}", rig.out[0]);
    }

    #[test]
    fn starting_rises_from_exactly_zero_without_a_step() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.run(20, Some(RATE as usize / 2));
        assert_smooth(&rig, "start");
        assert_eq!(*rig.out.last().unwrap(), 1.0, "reaches full level");
        let rise = rig.out.iter().position(|&x| x == 1.0).unwrap() - rig.out.iter().position(|&x| x > 0.0).unwrap();
        assert!((470..=490).contains(&rise), "the ramp is 10 ms: {rise} samples");
    }

    #[test]
    fn pausing_falls_to_exactly_zero_and_resuming_continues_on_the_next_sample() {
        let mut rig = Rig::new(|n| n as f32 / 100_000.0);
        rig.shared().set_playing(true);
        rig.run(20, Some(RATE as usize / 2));
        rig.shared().set_playing(false);
        let paused_at = rig.out.len();
        rig.run(10, Some(RATE as usize / 2));
        assert_eq!(*rig.out.last().unwrap(), 0.0);
        assert!(rig.out[paused_at + 500..].iter().all(|&x| x == 0.0), "silent within 10 ms of the pause");
        let read_at_pause = rig.shared().read.load(RELAXED);
        rig.run(10, Some(RATE as usize / 2));
        assert_eq!(rig.shared().read.load(RELAXED), read_at_pause, "nothing is consumed while paused");
        rig.shared().set_playing(true);
        let resumed_at = rig.out.len();
        rig.run(10, Some(RATE as usize / 2));
        // The source is a slow ramp: the first full-level sample after the resume continues it.
        let full = resumed_at + rig.out[resumed_at..].iter().position(|&x| x > 0.0).unwrap() + 480;
        let expected = (read_at_pause + 480) as f32 / 100_000.0;
        assert!((rig.out[full] - expected).abs() < 2e-5, "{} vs {expected}", rig.out[full]);
        let mut dc_rig = Rig::new(dc);
        dc_rig.shared().set_playing(true);
        dc_rig.run(20, Some(24_000));
        dc_rig.shared().set_playing(false);
        dc_rig.run(10, Some(24_000));
        dc_rig.shared().set_playing(true);
        dc_rig.run(10, Some(24_000));
        assert_smooth(&dc_rig, "pause and resume");
    }

    #[test]
    fn a_seek_fades_the_old_position_out_and_the_new_one_in() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.run(20, Some(24_000));
        // Seek: a new epoch; the old samples in the ring are dropped once silent.
        let before = rig.out.len();
        rig.feed.begin(2, 30_000_000);
        rig.source = |_| -1.0;
        rig.run(30, Some(24_000));
        assert_smooth(&rig, "seek");
        let after = &rig.out[before..];
        assert!(after.contains(&0.0), "silent between the two positions");
        assert_eq!(*after.last().unwrap(), -1.0, "the new position at full level");
        let zero = after.iter().position(|&x| x == 0.0).unwrap();
        assert!(zero <= 482, "the old position is gone within 10 ms: {zero}");
        assert!(after[..zero].iter().all(|&x| x > 0.0) && after[zero..].iter().all(|&x| x <= 0.0));
        let (pts, running, epoch) = rig.shared().now(rig.clock).unwrap();
        assert!(running && epoch == 2 && pts > 30_000_000, "the clock is the new position's: {pts}");
    }

    #[test]
    fn the_end_of_the_file_reaches_zero_on_its_last_sample() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.run(10, Some(24_000));
        rig.feed.finish();
        let end = rig.shared().end.load(RELAXED);
        rig.run(80, None);
        assert_smooth(&rig, "end");
        assert_eq!(*rig.out.last().unwrap(), 0.0);
        assert_eq!(rig.shared().read.load(RELAXED), end, "every sample of the file was played");
        assert!(rig.shared().take_notes() & note::DRAINED != 0);
    }

    #[test]
    fn a_file_shorter_than_the_ramp_is_a_bump_not_a_click() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.feed(100);
        rig.feed.finish();
        rig.run(10, None);
        assert_smooth(&rig, "a 2 ms file");
        assert_eq!(*rig.out.last().unwrap(), 0.0);
        assert!(rig.out.iter().any(|&x| x > 0.0));
    }

    #[test]
    fn an_underrun_fades_out_before_the_ring_is_empty_and_back_in_after() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.run(20, Some(24_000));
        // The producer stops (the network, a busy machine): the ring runs dry.
        rig.run(80, None);
        // (A frame or two may stay for when the producer is back: nothing is lost.)
        assert!(rig.feed.buffered() <= 2, "{} frames left", rig.feed.buffered());
        assert_eq!(*rig.out.last().unwrap(), 0.0);
        // A trickle below the rebuffer threshold does not start it again.
        rig.feed(1000);
        rig.run(5, None);
        assert_eq!(*rig.out.last().unwrap(), 0.0);
        rig.run(20, Some(24_000));
        assert_eq!(*rig.out.last().unwrap(), 1.0);
        assert_smooth(&rig, "underrun");
    }

    #[test]
    fn volume_and_mute_are_slewed() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.run(20, Some(24_000));
        for volume in [0.0, 1.0, 0.3, 0.0, 0.8] {
            rig.shared().set_volume(volume);
            rig.run(5, Some(24_000));
            assert!((rig.out.last().unwrap() - volume).abs() < 1e-6);
        }
        assert_smooth(&rig, "volume");
    }

    #[test]
    fn a_device_change_hands_over_at_zero() {
        let mut rig = Rig::new(dc);
        rig.shared().set_playing(true);
        rig.run(20, Some(24_000));
        // The old device's stream is paused down, then dropped once quiet.
        rig.shared().set_playing(false);
        rig.run(40, Some(24_000));
        assert!(rig.renderer.is_silent() && rig.shared().is_quiet());
        // The new device's stream calls the same renderer (`output.rs` hands it over once
        // the old stream is gone): it starts from the silence the old one ended in.
        rig.shared().set_playing(true);
        rig.run(20, Some(24_000));
        assert_eq!(*rig.out.last().unwrap(), 1.0);
        assert_smooth(&rig, "device change");
    }

    /// Frame `n` of the file is `n / 2^20`: every sample says where in the file it is.
    fn index(n: u64) -> f32 {
        n as f32 / 1_048_576.0
    }

    /// The output device is changed while playing: the old device is handed the fade to zero
    /// and silence, the new one starts from zero **on the sample that is due then** (the file
    /// went on by the wall clock meanwhile: nothing is played late), and no two samples in a
    /// row differ by more than the ramp allows.
    #[test]
    fn a_move_to_another_device_resumes_on_the_sample_that_is_due() {
        let mut rig = Rig::new(index);
        let shared = rig.shared();
        shared.set_playing(true);
        rig.run(30, Some(24_000));
        // The move begins: where the file is now, by the clock the pictures follow from here.
        let (at, i0) = (rig.clock, rig.out.len());
        let (pts, running, _) = shared.now(at).unwrap();
        assert!(running);
        let f0 = (pts as i128 * RATE as i128 / 1_000_000) as u64;
        shared.sync_to(pts, at);
        shared.set_moving(true);
        rig.run(40, Some(24_000));
        assert!(rig.renderer.is_silent() && shared.is_quiet(), "the old device ends in silence");
        let closed = rig.out.len();
        assert!(rig.out[i0 + 480..closed].iter().all(|&x| x == 0.0), "silence after the 10 ms fade");
        // The new device's stream: opened 70 ms later (nothing calls the renderer meanwhile).
        rig.clock += Duration::from_millis(70);
        let gap = 70 * RATE as usize / 1000;
        rig.renderer.restart();
        shared.set_moving(false);
        rig.run(30, Some(24_000));
        assert!(!shared.sync_pending());
        // Past the ramp, every sample is the one that would have played had nothing happened.
        let opened = closed;
        for i in opened + 960..rig.out.len() {
            let frame = f0 + (i - i0) as u64 + gap as u64;
            assert_eq!(rig.out[i], index(frame), "output sample {i} is frame {frame} of the file");
        }
        // And the way there is the ramp: the index source moves by 1e-6 a sample by itself.
        let step = rig.out[..].windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max);
        let largest = *rig.out.iter().max_by(|a, b| a.total_cmp(b)).unwrap();
        assert!(step <= MAX_STEP * largest + 2e-6, "a jump of {step} (the signal's level is {largest})");
        // The clock is the sound's again, and agrees with the wall clock it took over from.
        let (now, running, _) = shared.now(rig.clock).unwrap();
        let wall = pts + (rig.clock - at).as_micros() as i64;
        assert!(running && (now - wall).abs() <= 25, "the audio clock is {now}, the wall clock {wall}");
    }

    #[test]
    fn a_shut_gate_holds_the_sound_until_the_picture_is_there() {
        let mut rig = Rig::new(dc);
        rig.shared().set_gate(false);
        rig.shared().set_playing(true);
        rig.run(20, Some(24_000));
        assert!(rig.out.iter().all(|&x| x == 0.0));
        assert_eq!(rig.shared().read.load(RELAXED), 0);
        rig.shared().set_gate(true);
        rig.run(20, Some(24_000));
        assert_eq!(*rig.out.last().unwrap(), 1.0);
        assert_smooth(&rig, "gate");
    }

    #[test]
    fn a_sine_through_every_transition_never_jumps_more_than_the_sine_and_the_ramp() {
        let mut rig = Rig::new(sine);
        let shared = rig.shared();
        shared.set_playing(true);
        rig.run(20, Some(24_000));
        shared.set_playing(false);
        rig.run(7, Some(24_000));
        shared.set_playing(true);
        rig.run(13, Some(24_000));
        rig.feed.begin(2, 5_000_000);
        rig.run(21, Some(24_000));
        shared.set_volume(0.2);
        rig.run(9, Some(24_000));
        shared.set_volume(1.0);
        rig.run(60, None); // underrun
        rig.run(20, Some(24_000));
        rig.feed.finish();
        rig.run(80, None);
        // The sine's own steepest step plus the envelope's.
        let limit = 2.0 * std::f32::consts::PI * 440.0 / RATE as f32 + MAX_STEP;
        assert!(rig.max_step(0) <= limit, "{} > {limit}", rig.max_step(0));
        assert_eq!(*rig.out.last().unwrap(), 0.0);
    }

    #[test]
    fn the_clock_follows_the_samples_and_stands_still_when_silent() {
        let mut rig = Rig::new(dc);
        let shared = rig.shared();
        assert!(shared.now(rig.clock).is_none());
        shared.set_playing(true);
        rig.run(100, Some(24_000));
        let (pts, running, _) = shared.now(rig.clock).unwrap();
        let consumed = shared.read.load(RELAXED) as i64 * 1_000_000 / RATE as i64;
        assert!(running);
        assert_eq!(pts, consumed, "the sample heard now is the next one to render");
        let later = shared.now(rig.clock + Duration::from_millis(4)).unwrap().0;
        assert_eq!(later - pts, 4000, "between callbacks it runs with the wall clock");
        shared.set_playing(false);
        rig.run(10, Some(24_000));
        let (stopped, running, _) = shared.now(rig.clock).unwrap();
        assert!(!running);
        assert_eq!(shared.now(rig.clock + Duration::from_secs(5)).unwrap().0, stopped);
    }

    #[test]
    fn it_goes_quiet_only_after_the_tail_has_been_played_out() {
        let mut rig = Rig::new(dc);
        let shared = rig.shared();
        shared.set_playing(true);
        rig.run(20, Some(24_000));
        shared.take_notes();
        shared.set_playing(false);
        rig.run(5, Some(24_000));
        assert!(!shared.is_quiet(), "50 ms after a pause the device still holds the fade-out");
        rig.run(30, Some(24_000));
        assert!(shared.is_quiet());
        assert!(shared.take_notes() & note::QUIET != 0);
    }
}
