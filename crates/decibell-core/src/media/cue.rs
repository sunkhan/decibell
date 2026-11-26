//! Interface sounds (join / leave, mute, the call ring, …) for a client with
//! no audio layer of its own (the native client; Electron plays its own
//! through Web Audio and never calls this).
//!
//! A sound is PCM the client loaded (`Sound::from_wav`: the renderer's own
//! files), played at unity gain as the renderer's `AudioBufferSourceNode`
//! into `destination` does. Where it plays:
//!   - while a voice session or call has an output stream, **in that stream's
//!     mix** (`audio_device::OutputMix`): the callback owns a `CueVoices` and
//!     adds its samples to the voice sum. Never a second device stream beside
//!     the live one, never a restart of it;
//!   - otherwise on a short-lived output stream of its own (thread
//!     `decibell-cue`) that opens, plays and closes once the device has been
//!     fed 250 ms of silence after the last sound ended.
//!
//! The ring loops inside the callback (the buffer's end runs into its start,
//! as `loop = true` does); nothing is timed per repeat. Stopping it fades
//! over 5 ms, where the renderer's `stop()` cuts.
//!
//! Nothing here runs or is allocated until the first sound is played.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Thread;

use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};
use rubato::Resampler;

use super::audio_device::make_sinc_resampler;

// ── Sounds ───────────────────────────────────────────────────────────────────

struct SoundData {
    /// Mono.
    pcm: Arc<[i16]>,
    rate: u32,
    /// The sound at other device rates, made on first use.
    resampled: Mutex<Vec<(u32, Arc<[i16]>)>>,
}

/// A loaded interface sound. Cheap to clone; keep it for the app's life.
#[derive(Clone)]
pub struct Sound(Arc<SoundData>);

impl Sound {
    /// Mono PCM at `rate`.
    pub fn from_pcm(pcm: Vec<i16>, rate: u32) -> Sound {
        Sound(Arc::new(SoundData { pcm: pcm.into(), rate: rate.max(1), resampled: Mutex::new(Vec::new()) }))
    }

    /// A RIFF / WAVE file of 16-bit PCM, mono or stereo (stereo is averaged,
    /// as Web Audio down-mixes it for a mono destination).
    pub fn from_wav(bytes: &[u8]) -> Result<Sound, String> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err("not a WAVE file".into());
        }
        let mut at = 12;
        let mut fmt: Option<(u16, u32)> = None;
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let body = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
            match id {
                b"fmt " => {
                    if body.len() < 16 {
                        return Err("short fmt chunk".into());
                    }
                    let tag = u16::from_le_bytes([body[0], body[1]]);
                    let channels = u16::from_le_bytes([body[2], body[3]]);
                    let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                    let bits = u16::from_le_bytes([body[14], body[15]]);
                    // 1 = PCM; 0xFFFE = extensible (its sub-format is PCM for 16-bit files).
                    if (tag != 1 && tag != 0xFFFE) || bits != 16 || !(1..=2).contains(&channels) || rate == 0 {
                        return Err(format!("unsupported WAVE format (tag {tag}, {channels} ch, {bits} bit)"));
                    }
                    fmt = Some((channels, rate));
                }
                b"data" => {
                    let (channels, rate) = fmt.ok_or("data before fmt")?;
                    let samples = body.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]));
                    let pcm: Vec<i16> = if channels == 1 {
                        samples.collect()
                    } else {
                        let all: Vec<i16> = samples.collect();
                        all.chunks_exact(2).map(|f| ((f[0] as i32 + f[1] as i32) / 2) as i16).collect()
                    };
                    if pcm.is_empty() {
                        return Err("empty sound".into());
                    }
                    return Ok(Sound::from_pcm(pcm, rate));
                }
                _ => {}
            }
            at += 8 + len + (len & 1);
        }
        Err("no data chunk".into())
    }

    pub fn rate(&self) -> u32 {
        self.0.rate
    }

    /// Length in samples at its own rate.
    pub fn len(&self) -> usize {
        self.0.pcm.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.pcm.is_empty()
    }

    /// The sound's samples at `rate`: its own when the rates match (the file,
    /// sample for sample), else resampled once (the voice path's sinc
    /// resampler) and kept. A client can call this off its UI thread to pay for it early.
    pub fn at_rate(&self, rate: u32) -> Arc<[i16]> {
        if rate == self.0.rate {
            return Arc::clone(&self.0.pcm);
        }
        let mut cache = self.0.resampled.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, pcm)) = cache.iter().find(|(r, _)| *r == rate) {
            return Arc::clone(pcm);
        }
        let pcm: Arc<[i16]> = resample(&self.0.pcm, self.0.rate, rate).into();
        cache.push((rate, Arc::clone(&pcm)));
        pcm
    }
}

fn resample(pcm: &[i16], from: u32, to: u32) -> Vec<i16> {
    const CHUNK: usize = 1024;
    let mut rs = make_sinc_resampler(from, to, CHUNK, 1);
    let want = (pcm.len() as u64 * to as u64).div_ceil(from as u64) as usize;
    // This resampler's first output frame is the input's first instant (its
    // `output_delay()` is not in what it returns; the test
    // `resampling_keeps_time_and_pitch` holds it to that), so nothing is
    // skipped at the start.
    let mut out: Vec<i16> = Vec::with_capacity(want + CHUNK);
    let mut input: Vec<f64> = Vec::new();
    let mut at = 0;
    while out.len() < want {
        let need = rs.input_frames_next();
        input.clear();
        // Past the end the sound is silence.
        input.extend((at..at + need).map(|i| pcm.get(i).map_or(0.0, |&s| s as f64 / 32768.0)));
        at += need;
        match rs.process(&[&input], None) {
            Ok(chunk) => out.extend(chunk[0].iter().map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)),
            Err(_) => break,
        }
    }
    out.truncate(want);
    out
}

// ── The callback's side ──────────────────────────────────────────────────────

/// Sounds one stream plays at once; a ninth is dropped (the renderer's
/// context has no such limit, and never comes near it).
const MAX_VOICES: usize = 8;

enum Cmd {
    Play { id: u64, pcm: Arc<[i16]>, looping: bool },
    /// Fade out and end (the ring's stop). Unknown ids are ignored.
    Stop { id: u64 },
}

struct Voice {
    id: u64,
    pcm: Arc<[i16]>,
    pos: usize,
    looping: bool,
    /// Samples left of the stop fade, once stopped.
    fade_left: Option<u32>,
}

struct BusShared {
    /// Sounds sent and not finished (queued for the callback, or playing).
    busy: AtomicUsize,
    /// Frames rendered since `busy` reached zero.
    idle_frames: AtomicU64,
    callbacks: AtomicU64,
}

/// The sounds playing on one output stream. Owned by its callback: `begin`
/// once per callback, `sample` per output frame, `end` after the last.
/// Allocates nothing and frees nothing (the control side keeps every
/// buffer alive).
pub(crate) struct CueVoices {
    cmds: HeapCons<Cmd>,
    voices: [Option<Voice>; MAX_VOICES],
    live: usize,
    shared: Arc<BusShared>,
    /// The stop fade, in frames: 5 ms.
    fade_len: u32,
    /// Woken when the last sound ends, and again once `linger` idle frames
    /// have been rendered after it.
    waiter: Thread,
    linger: u64,
    lingered: bool,
}

impl CueVoices {
    /// Take the commands sent since the last callback.
    #[inline]
    pub fn begin(&mut self) {
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        while let Some(cmd) = self.cmds.try_pop() {
            match cmd {
                Cmd::Play { id, pcm, looping } => match self.voices.iter_mut().find(|v| v.is_none()) {
                    Some(slot) => {
                        *slot = Some(Voice { id, pcm, pos: 0, looping, fade_left: None });
                        self.live += 1;
                    }
                    None => {
                        self.shared.busy.fetch_sub(1, Ordering::AcqRel);
                    }
                },
                Cmd::Stop { id } => {
                    for v in self.voices.iter_mut().flatten() {
                        if v.id == id && v.fade_left.is_none() {
                            v.fade_left = Some(self.fade_len);
                        }
                    }
                }
            }
        }
    }

    /// The next output frame's sum of every playing sound (i16 scale).
    #[inline]
    pub fn sample(&mut self) -> i32 {
        if self.live == 0 {
            return 0;
        }
        let mut sum = 0i32;
        for slot in self.voices.iter_mut() {
            let Some(v) = slot else { continue };
            let mut s = v.pcm[v.pos] as i32;
            let mut done = false;
            if let Some(left) = v.fade_left {
                s = s * left as i32 / self.fade_len.max(1) as i32;
                if left == 0 {
                    done = true;
                } else {
                    v.fade_left = Some(left - 1);
                }
            }
            sum += s;
            v.pos += 1;
            if v.pos >= v.pcm.len() {
                if v.looping {
                    v.pos = 0;
                } else {
                    done = true;
                }
            }
            if done {
                *slot = None;
                self.live -= 1;
                self.shared.busy.fetch_sub(1, Ordering::AcqRel);
            }
        }
        sum
    }

    /// After a callback of `frames` frames.
    #[inline]
    pub fn end(&mut self, frames: usize) {
        if self.shared.busy.load(Ordering::Acquire) > 0 {
            if self.shared.idle_frames.load(Ordering::Relaxed) != 0 {
                self.shared.idle_frames.store(0, Ordering::Relaxed);
            }
            self.lingered = false;
            return;
        }
        let before = self.shared.idle_frames.fetch_add(frames as u64, Ordering::AcqRel);
        if before == 0 {
            self.waiter.unpark(); // the last sound just ended
        }
        if !self.lingered && before + frames as u64 >= self.linger {
            self.lingered = true;
            self.waiter.unpark();
        }
    }
}

// ── The control side ─────────────────────────────────────────────────────────

/// The sending end of one stream's `CueVoices`.
pub(crate) struct CueBus {
    prod: Mutex<HeapProd<Cmd>>,
    rate: u32,
    shared: Arc<BusShared>,
    /// Every buffer sent, so the callback never drops the last reference to
    /// one (a free on the audio thread).
    keep: Mutex<Vec<Arc<[i16]>>>,
}

impl CueBus {
    /// A bus for a stream at `rate`, and the voices its callback owns.
    /// `waiter`: see `CueVoices::waiter`; `linger_frames`: how much silence
    /// after the last sound counts as played out.
    pub fn new(rate: u32, waiter: Thread, linger_frames: u64) -> (Arc<CueBus>, CueVoices) {
        let (prod, cons) = HeapRb::<Cmd>::new(32).split();
        let shared = Arc::new(BusShared { busy: AtomicUsize::new(0), idle_frames: AtomicU64::new(0), callbacks: AtomicU64::new(0) });
        let voices = CueVoices {
            cmds: cons,
            voices: Default::default(),
            live: 0,
            shared: Arc::clone(&shared),
            fade_len: (rate / 200).max(1),
            waiter,
            linger: linger_frames,
            lingered: false,
        };
        (Arc::new(CueBus { prod: Mutex::new(prod), rate, shared, keep: Mutex::new(Vec::new()) }), voices)
    }

    fn play(&self, sound: &Sound, id: u64, looping: bool) -> bool {
        let pcm = sound.at_rate(self.rate);
        if pcm.is_empty() {
            return false;
        }
        {
            let mut keep = self.keep.lock().unwrap_or_else(|e| e.into_inner());
            if !keep.iter().any(|k| Arc::ptr_eq(k, &pcm)) {
                keep.push(Arc::clone(&pcm));
            }
        }
        self.shared.busy.fetch_add(1, Ordering::AcqRel);
        let sent = self.prod.lock().unwrap_or_else(|e| e.into_inner()).try_push(Cmd::Play { id, pcm, looping }).is_ok();
        if !sent {
            self.shared.busy.fetch_sub(1, Ordering::AcqRel);
        }
        sent
    }

    fn stop(&self, id: u64) {
        let _ = self.prod.lock().unwrap_or_else(|e| e.into_inner()).try_push(Cmd::Stop { id });
    }

    /// Sounds queued or playing.
    pub fn busy(&self) -> usize {
        self.shared.busy.load(Ordering::Acquire)
    }

    fn idle_frames(&self) -> u64 {
        self.shared.idle_frames.load(Ordering::Acquire)
    }

    fn callbacks(&self) -> u64 {
        self.shared.callbacks.load(Ordering::Relaxed)
    }
}

/// The cue stream's own mix: only sounds, converted to the device's format.
/// One sound alone comes out exactly as its samples.
pub(crate) struct CueMix {
    n: usize,
    voices: CueVoices,
}

impl CueMix {
    pub(crate) fn new(out_ch: u16, voices: CueVoices) -> Self {
        CueMix { n: out_ch.max(1) as usize, voices }
    }

    pub(crate) fn render<T>(&mut self, data: &mut [T])
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        self.voices.begin();
        for frame in data.chunks_exact_mut(self.n) {
            let s = T::from_sample((self.voices.sample() as f32 / 32768.0).clamp(-1.0, 1.0));
            for ch in frame.iter_mut() {
                *ch = s;
            }
        }
        self.voices.end(data.len() / self.n);
    }
}

// ── Routing ──────────────────────────────────────────────────────────────────

/// The cue stream with no voice session: its thread opens the device, then
/// sleeps until the callback reports the sounds played out, and closes it.
struct Standalone {
    generation: u64,
    device: Option<String>,
    /// Set by the thread once the device is open.
    bus: Option<Arc<CueBus>>,
    /// Sounds asked for while it was opening.
    pending: Vec<(Sound, u64, bool)>,
}

struct Ring {
    id: u64,
    sound: Sound,
    device: Option<String>,
}

#[derive(Default)]
struct Router {
    /// The live voice / call output's bus, while there is one.
    live: Option<Arc<CueBus>>,
    standalone: Option<Standalone>,
    ring: Option<Ring>,
    next_id: u64,
    generation: u64,
}

static ROUTER: Mutex<Option<Router>> = Mutex::new(None);

fn with_router<R>(f: impl FnOnce(&mut Router) -> R) -> R {
    let mut g = ROUTER.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Router::default))
}

impl Router {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn route(&mut self, sound: &Sound, id: u64, looping: bool, device: Option<&str>) {
        if let Some(bus) = &self.live {
            bus.play(sound, id, looping);
            return;
        }
        if let Some(s) = self.standalone.as_mut().filter(|s| s.device.as_deref() == device) {
            match &s.bus {
                Some(bus) => {
                    bus.play(sound, id, looping);
                }
                None => s.pending.push((sound.clone(), id, looping)),
            }
            return;
        }
        // None, or one on another device (it finishes what it has and closes).
        self.generation += 1;
        let generation = self.generation;
        self.standalone = Some(Standalone { generation, device: device.map(str::to_string), bus: None, pending: vec![(sound.clone(), id, looping)] });
        let device = device.map(str::to_string);
        let spawned = std::thread::Builder::new()
            .name("decibell-cue".into())
            .spawn(move || standalone_thread(generation, device));
        if let Err(e) = spawned {
            log::warn!("[cue] couldn't start the cue thread: {}", e);
            self.standalone = None;
        }
    }

    fn stop_everywhere(&mut self, id: u64) {
        if let Some(bus) = &self.live {
            bus.stop(id);
        }
        if let Some(s) = &mut self.standalone {
            s.pending.retain(|(_, pid, _)| *pid != id);
            if let Some(bus) = &s.bus {
                bus.stop(id);
            }
        }
    }

    /// The ring moves to wherever sounds play now (a voice output appeared or
    /// went): it ends where it was (5 ms fade) and starts over there.
    fn move_ring(&mut self) {
        let Some(ring) = self.ring.take() else { return };
        self.stop_everywhere(ring.id);
        let id = self.id();
        self.route(&ring.sound, id, true, ring.device.as_deref());
        self.ring = Some(Ring { id, ..ring });
    }
}

/// Play `sound` once. `device`: where the cue stream opens when no voice
/// output is live (None = the OS default, which is what the renderer's
/// `AudioContext` plays on); in voice it plays on the voice output.
pub fn play(sound: &Sound, device: Option<&str>) {
    with_router(|r| {
        let id = r.id();
        r.route(sound, id, false, device);
    });
}

/// Loop `sound` until `stop_ring` (the renderer's `loopSound`). A ring
/// already playing is replaced.
pub fn start_ring(sound: &Sound, device: Option<&str>) {
    with_router(|r| {
        if let Some(old) = r.ring.take() {
            r.stop_everywhere(old.id);
        }
        let id = r.id();
        r.route(sound, id, true, device);
        r.ring = Some(Ring { id, sound: sound.clone(), device: device.map(str::to_string) });
    });
}

/// End the ring (5 ms fade). Idempotent.
pub fn stop_ring() {
    with_router(|r| {
        if let Some(ring) = r.ring.take() {
            r.stop_everywhere(ring.id);
        }
    });
}

/// A voice output with this bus went live: sounds play in its mix from now.
pub(crate) fn register_live(bus: &Arc<CueBus>) {
    with_router(|r| {
        // Stop the ring where it plays before the route changes.
        let ring = r.ring.take();
        if let Some(ring) = &ring {
            r.stop_everywhere(ring.id);
        }
        r.live = Some(Arc::clone(bus));
        if let Some(ring) = ring {
            let id = r.id();
            r.route(&ring.sound, id, true, ring.device.as_deref());
            r.ring = Some(Ring { id, ..ring });
        }
    });
}

/// That output is going away: later sounds play elsewhere; what it is
/// playing finishes on it (the pipeline waits for `busy() == 0`), except the
/// ring, which moves.
pub(crate) fn retire_live(bus: &Arc<CueBus>) {
    with_router(|r| {
        if r.live.as_ref().is_some_and(|l| Arc::ptr_eq(l, bus)) {
            if let Some(ring) = &r.ring {
                bus.stop(ring.id);
            }
            r.live = None;
            r.move_ring();
        }
    });
}

/// 250 ms of silence after the last sound: the device's own buffer (40 ms
/// asked for; the sound server's on top) has played out, and a sound that
/// follows shortly (mute then unmute) reuses the open stream.
fn linger_frames(rate: u32) -> u64 {
    rate as u64 / 4
}

/// How a cue stream is opened: `make(rate, channels)` builds the mix the
/// stream's callback renders. Returns the open stream (dropped to close it).
type Opener = fn(Option<&str>, Box<dyn FnOnce(u32, u16) -> CueMix + Send>) -> Option<Box<dyn std::any::Any>>;

fn open_device(device: Option<&str>, make: Box<dyn FnOnce(u32, u16) -> CueMix + Send>) -> Option<Box<dyn std::any::Any>> {
    let host = cpal::default_host();
    super::audio_device::build_cue_output_stream(&host, device, make).map(|s| Box::new(s) as Box<dyn std::any::Any>)
}

#[cfg(not(test))]
fn opener() -> Opener {
    open_device
}

#[cfg(test)]
fn opener() -> Opener {
    tests::TEST_OPENER.lock().unwrap().unwrap_or(open_device)
}

fn standalone_thread(generation: u64, device: Option<String>) {
    let me = std::thread::current();
    let bus_slot: Arc<Mutex<Option<Arc<CueBus>>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&bus_slot);
    let opened_at = std::time::Instant::now();
    let stream = opener()(
        device.as_deref(),
        Box::new(move |rate, channels| {
            let (bus, voices) = CueBus::new(rate, me, linger_frames(rate));
            *slot.lock().unwrap() = Some(bus);
            CueMix::new(channels, voices)
        }),
    );
    let bus = bus_slot.lock().unwrap().take();
    let (Some(stream), Some(bus)) = (stream, bus) else {
        log::warn!("[cue] no output device for interface sounds ({:?})", device);
        with_router(|r| {
            if r.standalone.as_ref().is_some_and(|s| s.generation == generation) {
                r.standalone = None;
            }
        });
        return;
    };
    log::debug!("[cue] output stream open in {:?} ({} Hz)", opened_at.elapsed(), bus.rate);
    // Hand over what was asked for meanwhile; from here senders use the bus.
    let ours = with_router(|r| match r.standalone.as_mut().filter(|s| s.generation == generation) {
        Some(s) => {
            for (sound, id, looping) in s.pending.drain(..) {
                bus.play(&sound, id, looping);
            }
            s.bus = Some(Arc::clone(&bus));
            true
        }
        None => false,
    });
    if ours {
        let mut seen = bus.callbacks();
        loop {
            // The callback wakes this thread when the sounds have played
            // out. The timeout only catches a device whose callback stopped
            // (unplugged): nothing would wake us then.
            const DEAD_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
            let parked = std::time::Instant::now();
            std::thread::park_timeout(DEAD_AFTER);
            let done = bus.busy() == 0 && bus.idle_frames() >= linger_frames(bus.rate);
            let mut dead = false;
            if parked.elapsed() >= DEAD_AFTER {
                let now = bus.callbacks();
                dead = now == seen;
                seen = now;
            }
            if !(done || dead) {
                continue;
            }
            // Senders push under the router's lock: checked again inside it.
            let closed = with_router(|r| {
                if !dead && bus.busy() > 0 {
                    return false;
                }
                if r.standalone.as_ref().is_some_and(|s| s.generation == generation) {
                    r.standalone = None;
                    // A ring on a dead device starts over on a new stream.
                    if dead && r.live.is_none() {
                        r.move_ring();
                    }
                }
                true
            });
            if closed {
                break;
            }
        }
    }
    drop(stream);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    pub(crate) static TEST_OPENER: Mutex<Option<Opener>> = Mutex::new(None);
    /// Tests that use the router (a process-wide static) run one at a time.
    pub(crate) static ROUTER_TESTS: Mutex<()> = Mutex::new(());

    /// The renderer's files (`electron-client/src/assets/sounds`), which the
    /// native client ships as its own assets.
    pub(crate) const FILES: [(&str, &[u8]); 12] = [
        ("mute", include_bytes!("../../../../electron-client/src/assets/sounds/mute.wav")),
        ("unmute", include_bytes!("../../../../electron-client/src/assets/sounds/unmute.wav")),
        ("deafen", include_bytes!("../../../../electron-client/src/assets/sounds/deafen.wav")),
        ("undeafen", include_bytes!("../../../../electron-client/src/assets/sounds/undeafen.wav")),
        ("user_join", include_bytes!("../../../../electron-client/src/assets/sounds/user_join.wav")),
        ("user_leave", include_bytes!("../../../../electron-client/src/assets/sounds/user_leave.wav")),
        ("stream_start", include_bytes!("../../../../electron-client/src/assets/sounds/stream_start.wav")),
        ("stream_stop", include_bytes!("../../../../electron-client/src/assets/sounds/stream_stop.wav")),
        ("connect", include_bytes!("../../../../electron-client/src/assets/sounds/connect.wav")),
        ("disconnect", include_bytes!("../../../../electron-client/src/assets/sounds/disconnect.wav")),
        ("call_ring", include_bytes!("../../../../electron-client/src/assets/sounds/call_ring.wav")),
        ("call_end", include_bytes!("../../../../electron-client/src/assets/sounds/call_end.wav")),
    ];

    pub(crate) fn file(name: &str) -> Sound {
        Sound::from_wav(FILES.iter().find(|(n, _)| *n == name).unwrap().1).unwrap()
    }

    /// The samples of a canonical 44-byte-header WAVE file.
    fn raw_samples(bytes: &[u8]) -> Vec<i16> {
        let at = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
        bytes[at..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
    }

    /// Play `sound` on `bus` directly (no router).
    pub(crate) fn play_on(bus: &Arc<CueBus>, sound: &Sound) {
        assert!(bus.play(sound, 1_000_000, false));
    }

    fn bus(rate: u32) -> (Arc<CueBus>, CueMix) {
        let (bus, voices) = CueBus::new(rate, std::thread::current(), linger_frames(rate));
        (bus, CueMix::new(2, voices))
    }

    fn render(mix: &mut CueMix, frames: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = [0f32; 2 * 441];
        let mut left = frames;
        while left > 0 {
            let n = left.min(441);
            mix.render(&mut buf[..2 * n]);
            for f in buf[..2 * n].chunks_exact(2) {
                assert_eq!(f[0], f[1], "both channels carry the sound");
                out.push(f[0]);
            }
            left -= n;
        }
        out
    }

    /// Every file of the renderer loads, and a stream at the file's rate
    /// plays exactly its samples at unity gain, then silence; first and last
    /// sample are (near) zero, so a sound opens and closes without a step.
    #[test]
    fn each_file_plays_sample_for_sample() {
        println!("sound: rate, ms, peak dBFS, first, last");
        for (name, bytes) in FILES {
            let sound = Sound::from_wav(bytes).unwrap();
            let raw = raw_samples(bytes);
            assert_eq!(sound.len(), raw.len(), "{name}");
            let (bus, mut mix) = bus(sound.rate());
            assert!(bus.play(&sound, 1, false));
            let out = render(&mut mix, raw.len() + 500);
            for (i, &s) in raw.iter().enumerate() {
                assert_eq!(out[i], s as f32 / 32768.0, "{name} sample {i}");
            }
            assert!(out[raw.len()..].iter().all(|&s| s == 0.0), "{name}: silence after the end");
            assert_eq!(bus.busy(), 0);
            let peak = raw.iter().map(|s| s.unsigned_abs()).max().unwrap();
            println!(
                "{name}: {} Hz, {} ms, {:.1}, {}, {}",
                sound.rate(),
                raw.len() as u64 * 1000 / sound.rate() as u64,
                20.0 * (peak as f32 / 32768.0).log10(),
                raw[0],
                raw[raw.len() - 1]
            );
            // A step of under 1 % of full scale at either end.
            assert!(raw[0].unsigned_abs() < 328 && raw[raw.len() - 1].unsigned_abs() < 328, "{name} starts / ends off zero");
        }
    }

    /// The ring loops with the buffer's end running into its start, for as
    /// long as nobody stops it; the stop is a 5 ms linear fade from wherever
    /// it is, then silence.
    #[test]
    fn the_ring_loops_until_stopped_and_fades_out() {
        let ring = file("call_ring");
        let raw = ring.at_rate(ring.rate());
        let (bus, mut mix) = bus(ring.rate());
        assert!(bus.play(&ring, 7, true));
        let n = raw.len();
        let out = render(&mut mix, 2 * n + 1000);
        for (i, &o) in out.iter().enumerate() {
            assert_eq!(o, raw[i % n] as f32 / 32768.0, "sample {i}");
        }
        assert_eq!(bus.busy(), 1);
        bus.stop(7);
        let fade = (ring.rate() / 200) as usize;
        let tail = render(&mut mix, fade + 600);
        let at = (2 * n + 1000) % n;
        for i in 0..=fade {
            let want = (raw[(at + i) % n] as i32 * (fade - i) as i32 / fade as i32) as f32 / 32768.0;
            assert_eq!(tail[i], want, "fade sample {i}");
        }
        assert!(tail[fade..].iter().all(|&s| s == 0.0));
        assert_eq!(bus.busy(), 0);
    }

    /// Sounds overlap by summing, as the renderer's nodes do at the
    /// destination; a stop for one leaves the other.
    #[test]
    fn sounds_overlap() {
        let (a, b) = (file("mute"), file("deafen"));
        let (ra, rb) = (a.at_rate(48_000), b.at_rate(48_000));
        let (bus, mut mix) = bus(48_000);
        bus.play(&a, 1, false);
        let first = render(&mut mix, 100);
        bus.play(&b, 2, false);
        let both = render(&mut mix, rb.len() + 10);
        assert_eq!(first[50], ra[50] as f32 / 32768.0);
        for (i, &o) in both.iter().enumerate() {
            let sa = ra.get(100 + i).copied().unwrap_or(0) as i32;
            let sb = rb.get(i).copied().unwrap_or(0) as i32;
            assert_eq!(o, ((sa + sb) as f32 / 32768.0).clamp(-1.0, 1.0), "sample {i}");
        }
    }

    /// A sound at another rate than the device's is resampled in time and
    /// in tune: a 440 Hz tone at 44.1 kHz comes out as the same tone at
    /// 48 kHz, starting at the same instant (error under -50 dB).
    #[test]
    fn resampling_keeps_time_and_pitch() {
        let n = 44_100;
        let tone = |rate: f64, i: usize| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate).sin() * 12_000.0;
        let sound = Sound::from_pcm((0..n).map(|i| tone(44_100.0, i) as i16).collect(), 44_100);
        let out = sound.at_rate(48_000);
        assert_eq!(out.len(), 48_000);
        let mut err = 0f64;
        // Away from the edges, where the tone starts and stops abruptly.
        for i in 200..47_800 {
            err += (out[i] as f64 - tone(48_000.0, i)).powi(2);
        }
        let rms = (err / 47_600.0).sqrt();
        assert!(rms < 12_000.0 / 316.0, "resampling error {rms:.1} (tone 12000)");
        // The two 44.1 kHz files of the renderer keep their length in time.
        for name in ["call_ring", "call_end"] {
            let s = file(name);
            assert_eq!(s.rate(), 44_100);
            assert_eq!(s.at_rate(48_000).len(), (s.len() * 480).div_ceil(441));
        }
    }

    // ── The router and the stream without a voice session ────────────────

    /// A device for the tests: a thread renders 10 ms callbacks as fast as
    /// it can into `PLAYED` until the stream is dropped.
    struct FakeStream(Arc<std::sync::atomic::AtomicBool>, Option<std::thread::JoinHandle<()>>);
    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
            let _ = self.1.take().unwrap().join();
            OPEN.fetch_sub(1, Ordering::AcqRel);
        }
    }
    pub(crate) static PLAYED: Mutex<Vec<f32>> = Mutex::new(Vec::new());
    static OPEN: AtomicUsize = AtomicUsize::new(0);
    static OPENED: AtomicUsize = AtomicUsize::new(0);

    fn fake_open(_device: Option<&str>, make: Box<dyn FnOnce(u32, u16) -> CueMix + Send>) -> Option<Box<dyn std::any::Any>> {
        let mut mix = make(48_000, 1);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        OPEN.fetch_add(1, Ordering::AcqRel);
        OPENED.fetch_add(1, Ordering::AcqRel);
        let t = std::thread::spawn(move || {
            let mut buf = [0f32; 480];
            while !stop_t.load(Ordering::Acquire) {
                mix.render(&mut buf[..]);
                PLAYED.lock().unwrap().extend_from_slice(&buf);
                std::thread::sleep(Duration::from_micros(200));
            }
        });
        Some(Box::new(FakeStream(stop, Some(t))))
    }

    /// A router as at start, whatever an earlier (failed) test left.
    fn fresh_router() {
        stop_ring();
        with_router(|r| r.live = None);
        wait_closed();
    }

    fn wait_closed() {
        let deadline = Instant::now() + Duration::from_secs(10);
        while OPEN.load(Ordering::Acquire) > 0 || with_router(|r| r.standalone.is_some()) {
            assert!(Instant::now() < deadline, "the cue stream never closed");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn played_without_silence() -> Vec<f32> {
        let p = PLAYED.lock().unwrap();
        let first = p.iter().position(|&s| s != 0.0).unwrap_or(0);
        let last = p.iter().rposition(|&s| s != 0.0).unwrap_or(0);
        p[first..=last].to_vec()
    }

    /// With no voice output: one sound opens a stream, plays whole, and the
    /// stream closes by itself (no thread left); two sounds in a row share
    /// the stream; the ring plays until stopped and then the stream closes.
    #[test]
    fn without_voice_a_stream_opens_plays_and_closes() {
        let _one = ROUTER_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_OPENER.lock().unwrap() = Some(fake_open);
        fresh_router();
        PLAYED.lock().unwrap().clear();
        let opened = OPENED.load(Ordering::Acquire);

        let mute = file("mute");
        let raw = mute.at_rate(48_000);
        play(&mute, None);
        wait_closed();
        // The file's leading and trailing zeros are cut with the silence.
        let first = raw.iter().position(|&s| s != 0).unwrap();
        let last = raw.iter().rposition(|&s| s != 0).unwrap();
        let want: Vec<f32> = raw[first..=last].iter().map(|&s| s as f32 / 32768.0).collect();
        assert_eq!(played_without_silence(), want, "the whole sound, once");
        assert_eq!(OPENED.load(Ordering::Acquire), opened + 1);

        // Two sounds close together: one stream.
        PLAYED.lock().unwrap().clear();
        play(&mute, None);
        play(&file("unmute"), None);
        wait_closed();
        assert_eq!(OPENED.load(Ordering::Acquire), opened + 2);

        // The ring: still looping after three lengths, gone after the stop.
        PLAYED.lock().unwrap().clear();
        let ring = file("call_ring");
        start_ring(&ring, None);
        let ring_len = ring.at_rate(48_000).len();
        let deadline = Instant::now() + Duration::from_secs(20);
        // Three lengths of it played (the fake device writes silence until
        // the sound reaches it).
        let rung = || {
            let p = PLAYED.lock().unwrap();
            p.iter().position(|&s| s != 0.0).is_some_and(|start| p.len() >= start + 3 * ring_len)
        };
        while !rung() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(with_router(|r| r.ring.is_some() && r.standalone.is_some()));
        {
            let p = PLAYED.lock().unwrap();
            let r = ring.at_rate(48_000);
            let start = p.iter().position(|&s| s != 0.0).unwrap();
            let offset = r.iter().position(|&s| s != 0).unwrap();
            for i in 0..2 * ring_len {
                assert_eq!(p[start + i], r[(offset + i) % ring_len] as f32 / 32768.0, "ring sample {i}");
            }
        }
        stop_ring();
        stop_ring(); // idempotent
        wait_closed();
        assert_eq!(OPENED.load(Ordering::Acquire), opened + 3);
        *TEST_OPENER.lock().unwrap() = None;
    }

    /// While a voice output is live the sound goes to its bus and no stream
    /// is opened; the ring moves onto the voice output when one appears and
    /// back to a stream of its own when it goes.
    #[test]
    fn with_a_live_voice_output_nothing_else_opens() {
        let _one = ROUTER_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_OPENER.lock().unwrap() = Some(fake_open);
        fresh_router();
        let opened = OPENED.load(Ordering::Acquire);
        let (live, mut mix) = bus(48_000);
        register_live(&live);
        play(&file("deafen"), None);
        assert_eq!(live.busy(), 1);
        let out = render(&mut mix, 2_000);
        assert!(out.iter().any(|&s| s != 0.0));
        start_ring(&file("call_ring"), None);
        assert_eq!(live.busy(), 2);
        assert_eq!(OPENED.load(Ordering::Acquire), opened, "no stream beside the live one");

        // The voice output goes: the ring fades there and restarts alone.
        retire_live(&live);
        let _ = render(&mut mix, 48_000);
        assert_eq!(live.busy(), 0, "the one-shot finished and the ring faded out on the old output");
        let deadline = Instant::now() + Duration::from_secs(5);
        while OPENED.load(Ordering::Acquire) == opened {
            assert!(Instant::now() < deadline, "the ring did not move to its own stream");
            std::thread::sleep(Duration::from_millis(2));
        }
        // A new voice output: the ring moves onto it, the stream closes.
        let (live2, mut mix2) = bus(48_000);
        register_live(&live2);
        assert_eq!(live2.busy(), 1);
        assert!(render(&mut mix2, 4_800).iter().any(|&s| s != 0.0));
        wait_closed();
        stop_ring();
        let _ = render(&mut mix2, 4_800);
        assert_eq!(live2.busy(), 0);
        retire_live(&live2);
        *TEST_OPENER.lock().unwrap() = None;
    }
}
