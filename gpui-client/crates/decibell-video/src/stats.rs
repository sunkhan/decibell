//! Per-stream counters, written lock-free on the hot paths and read on demand.
//!
//! Nothing here runs by itself: no timer, no thread. The stats overlay (the renderer's
//! `StreamVideoPlayer.tsx` publishes codec, size, input fps, fps, queue and dropped) reads a
//! [`StreamStats`] when it repaints and gets rates from two snapshots ([`StatsSampler`]).
//!
//! `DECIBELL_VIDEO_TRACE=<file>` adds a per-frame ring (arrival, decoded, published, presented)
//! that is appended to the file as CSV when the stream's decoder stops.

use std::io::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use crate::codec::{Codec, DecodePath};

const RELAXED: Ordering = Ordering::Relaxed;

/// Durations in microseconds, in 16 buckets per octave (each 4-6 % wide, so a percentile is
/// within 3 % of the truth): 512 counters, 2 KB.
pub(crate) struct Histogram {
    buckets: [AtomicU32; Histogram::BUCKETS],
}

impl Histogram {
    const SUB_BITS: u32 = 4;
    const BUCKETS: usize = 32 << Histogram::SUB_BITS;

    pub const fn new() -> Self {
        Self {
            buckets: [const { AtomicU32::new(0) }; Histogram::BUCKETS],
        }
    }

    fn bucket(micros: u64) -> usize {
        let value = micros.clamp(1, u32::MAX as u64) as u32;
        let octave = 31 - value.leading_zeros();
        let sub = if octave >= Self::SUB_BITS {
            value >> (octave - Self::SUB_BITS)
        } else {
            value << (Self::SUB_BITS - octave)
        } & ((1 << Self::SUB_BITS) - 1);
        ((octave << Self::SUB_BITS) + sub) as usize
    }

    /// The middle of a bucket, in microseconds.
    fn value(bucket: usize) -> f64 {
        let subs = (1 << Self::SUB_BITS) as f64;
        let octave = (bucket >> Self::SUB_BITS) as i32;
        let sub = (bucket & ((1 << Self::SUB_BITS) - 1)) as f64;
        2f64.powi(octave) * (1.0 + (sub + 0.5) / subs)
    }

    pub fn record(&self, micros: u64) {
        self.buckets[Self::bucket(micros)].fetch_add(1, RELAXED);
    }

    /// The given percentiles (0..=100) in milliseconds; all 0.0 when nothing was recorded.
    pub fn percentiles<const N: usize>(&self, wanted: [f64; N]) -> [f64; N] {
        let counts: [u32; Self::BUCKETS] = std::array::from_fn(|i| self.buckets[i].load(RELAXED));
        let total: u64 = counts.iter().map(|&count| count as u64).sum();
        let mut out = [0.0; N];
        if total == 0 {
            return out;
        }
        for (slot, percentile) in out.iter_mut().zip(wanted) {
            let rank = ((percentile / 100.0) * total as f64).ceil().max(1.0) as u64;
            let mut seen = 0;
            for (bucket, &count) in counts.iter().enumerate() {
                seen += count as u64;
                if seen >= rank {
                    *slot = Self::value(bucket) / 1000.0;
                    break;
                }
            }
        }
        out
    }
}

/// One frame's journey, in nanoseconds since the stream's epoch (0: did not happen).
#[derive(Default)]
struct TraceEntry {
    serial: AtomicU64,
    arrived: AtomicU64,
    decoded: AtomicU64,
    published: AtomicU64,
    presented: AtomicU64,
}

/// The newest frames' timestamps, indexed by serial. Always present (small) so that
/// "presented" can be matched to its frame; large when a trace file was asked for.
struct Trace {
    entries: Box<[TraceEntry]>,
}

impl Trace {
    fn new(len: usize) -> Self {
        Self {
            entries: (0..len).map(|_| TraceEntry::default()).collect(),
        }
    }

    fn entry(&self, serial: u64) -> &TraceEntry {
        &self.entries[(serial % self.entries.len() as u64) as usize]
    }
}

pub(crate) fn trace_path() -> Option<&'static std::path::Path> {
    static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os("DECIBELL_VIDEO_TRACE").map(Into::into))
        .as_deref()
}

/// The live counters of one stream.
pub(crate) struct Counters {
    epoch: Instant,
    /// Frames pushed for this stream while someone displays it.
    pub received: AtomicU64,
    /// Frames pushed while nobody displays the stream (not decoded, not counted as dropped).
    pub unwatched: AtomicU64,
    /// Frames shed before the decoder: queue overflow and everything gated until a keyframe.
    pub shed: AtomicU64,
    pub keyframe_requests: AtomicU64,
    /// Packets handed to the decoder.
    pub packets: AtomicU64,
    pub decoded: AtomicU64,
    /// Pictures handed to the renderer.
    pub published: AtomicU64,
    /// Distinct pictures that reached a rendered frame.
    pub presented: AtomicU64,
    /// Decoded pictures not handed over because every slot was still in use.
    pub no_slot: AtomicU64,
    pub errors: AtomicU64,
    /// Decoders opened (1 for a quiet stream; more: codec, description or recovery).
    pub opens: AtomicU64,
    /// Times the stream moved from hardware to software decode after a failure.
    pub fallbacks: AtomicU64,
    pub device_losses: AtomicU64,
    /// Bytes of encoded video handed to the decoder.
    pub bytes: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    /// `Codec::index() + 1`; 0: none yet.
    codec: AtomicU32,
    /// 1 hardware, 2 software; 0: no decoder.
    path: AtomicU32,
    /// Packet handed to the decoder -> picture published.
    pub decode: Histogram,
    /// Frame arrived at the hub -> picture published.
    pub latency: Histogram,
    /// Between two pictures reaching a rendered frame.
    pub present_interval: Histogram,
    last_presented_ns: AtomicU64,
    trace: Trace,
}

impl Counters {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            received: AtomicU64::new(0),
            unwatched: AtomicU64::new(0),
            shed: AtomicU64::new(0),
            keyframe_requests: AtomicU64::new(0),
            packets: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
            published: AtomicU64::new(0),
            presented: AtomicU64::new(0),
            no_slot: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            opens: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            device_losses: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            codec: AtomicU32::new(0),
            path: AtomicU32::new(0),
            decode: Histogram::new(),
            latency: Histogram::new(),
            present_interval: Histogram::new(),
            last_presented_ns: AtomicU64::new(0),
            trace: Trace::new(if trace_path().is_some() { 1 << 15 } else { 64 }),
        }
    }

    pub fn now_ns(&self) -> u64 {
        // +1: 0 means "did not happen".
        self.epoch.elapsed().as_nanos() as u64 + 1
    }

    pub fn ns_of(&self, instant: Instant) -> u64 {
        instant.saturating_duration_since(self.epoch).as_nanos() as u64 + 1
    }

    pub fn set_decoder(&self, codec: Codec, path: Option<DecodePath>) {
        self.codec.store(codec.index() as u32 + 1, RELAXED);
        self.path.store(
            match path {
                Some(DecodePath::Hardware) => 1,
                Some(DecodePath::Software) => 2,
                None => 0,
            },
            RELAXED,
        );
    }

    pub fn clear_decoder(&self) {
        self.path.store(0, RELAXED);
    }

    pub fn set_size(&self, width: u32, height: u32) {
        self.width.store(width, RELAXED);
        self.height.store(height, RELAXED);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width.load(RELAXED), self.height.load(RELAXED))
    }

    /// A frame left the queue for the decoder. Returns its serial (the tag its picture is
    /// published with).
    pub fn frame_started(&self, arrived: Instant, bytes: usize) -> u64 {
        let serial = self.packets.fetch_add(1, RELAXED) + 1;
        self.bytes.fetch_add(bytes as u64, RELAXED);
        let entry = self.trace.entry(serial);
        entry.serial.store(serial, RELAXED);
        entry.arrived.store(self.ns_of(arrived), RELAXED);
        entry.decoded.store(0, RELAXED);
        entry.published.store(0, RELAXED);
        entry.presented.store(0, RELAXED);
        serial
    }

    pub fn frame_decoded(&self, serial: u64) {
        self.decoded.fetch_add(1, RELAXED);
        let entry = self.trace.entry(serial);
        if entry.serial.load(RELAXED) == serial {
            entry.decoded.store(self.now_ns(), RELAXED);
        }
    }

    pub fn frame_published(&self, serial: u64, arrived: Instant, handed: Instant) {
        self.published.fetch_add(1, RELAXED);
        let now = Instant::now();
        self.decode
            .record(now.saturating_duration_since(handed).as_micros() as u64);
        self.latency
            .record(now.saturating_duration_since(arrived).as_micros() as u64);
        let entry = self.trace.entry(serial);
        if entry.serial.load(RELAXED) == serial {
            entry.published.store(self.ns_of(now), RELAXED);
        }
    }

    /// Called on the render thread the first time a rendered frame draws picture `serial`.
    pub fn frame_presented(&self, serial: u64) {
        self.presented.fetch_add(1, RELAXED);
        let now = self.now_ns();
        let previous = self.last_presented_ns.swap(now, RELAXED);
        if previous != 0 && now > previous {
            self.present_interval.record((now - previous) / 1000);
        }
        let entry = self.trace.entry(serial);
        if entry.serial.load(RELAXED) == serial {
            entry.presented.store(now, RELAXED);
        }
    }

    /// A pause in the pictures is not an interval between two of them.
    pub fn presentation_paused(&self) {
        self.last_presented_ns.store(0, RELAXED);
    }

    pub fn snapshot(&self, queue: usize) -> StreamStats {
        let published = self.published.load(RELAXED);
        let presented = self.presented.load(RELAXED);
        let [decode_p50, decode_p99] = self.decode.percentiles([50.0, 99.0]);
        let [latency_p50, latency_p99] = self.latency.percentiles([50.0, 99.0]);
        let [interval_p50, interval_p99] = self.present_interval.percentiles([50.0, 99.0]);
        StreamStats {
            codec: (self.codec.load(RELAXED) as usize)
                .checked_sub(1)
                .and_then(Codec::from_index),
            path: match self.path.load(RELAXED) {
                1 => Some(DecodePath::Hardware),
                2 => Some(DecodePath::Software),
                _ => None,
            },
            width: self.width.load(RELAXED),
            height: self.height.load(RELAXED),
            received: self.received.load(RELAXED),
            unwatched: self.unwatched.load(RELAXED),
            dropped: self.shed.load(RELAXED) + self.no_slot.load(RELAXED),
            decoded: self.decoded.load(RELAXED),
            published,
            presented,
            replaced: published.saturating_sub(presented),
            errors: self.errors.load(RELAXED),
            keyframe_requests: self.keyframe_requests.load(RELAXED),
            decoder_opens: self.opens.load(RELAXED),
            fallbacks: self.fallbacks.load(RELAXED),
            device_losses: self.device_losses.load(RELAXED),
            bytes: self.bytes.load(RELAXED),
            queue,
            decode_ms_p50: decode_p50,
            decode_ms_p99: decode_p99,
            latency_ms_p50: latency_p50,
            latency_ms_p99: latency_p99,
            present_interval_ms_p50: interval_p50,
            present_interval_ms_p99: interval_p99,
        }
    }

    /// Appends the trace ring to `DECIBELL_VIDEO_TRACE` (if set): one line per frame, times in
    /// microseconds since the stream was created, empty where a step did not happen.
    pub fn dump_trace(&self, key: &str) {
        let Some(path) = trace_path() else {
            return;
        };
        let mut rows: Vec<[u64; 5]> = self
            .trace
            .entries
            .iter()
            .map(|entry| {
                [
                    entry.serial.load(RELAXED),
                    entry.arrived.load(RELAXED),
                    entry.decoded.load(RELAXED),
                    entry.published.load(RELAXED),
                    entry.presented.load(RELAXED),
                ]
            })
            .filter(|row| row[0] != 0)
            .collect();
        rows.sort_unstable();
        let mut text = String::with_capacity(rows.len() * 48 + 64);
        text.push_str("# stream,serial,arrived_us,decoded_us,published_us,presented_us\n");
        for row in rows {
            text.push_str(key);
            text.push_str(&format!(",{}", row[0]));
            for ns in &row[1..] {
                match ns {
                    0 => text.push(','),
                    ns => text.push_str(&format!(",{:.1}", (*ns - 1) as f64 / 1000.0)),
                }
            }
            text.push('\n');
        }
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(text.as_bytes()));
        if let Err(error) = written {
            log::warn!("video: writing the trace to {}: {error}", path.display());
        }
        // The next run of this stream's decoder starts a new trace.
        for entry in self.trace.entries.iter() {
            entry.serial.store(0, RELAXED);
        }
    }
}

/// A stream's numbers at one moment. Counts are totals since the stream was created.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamStats {
    pub codec: Option<Codec>,
    /// `None`: no decoder is open (nothing arrived, nobody displays it, or it went idle).
    pub path: Option<DecodePath>,
    /// The picture's size (not the coded size).
    pub width: u32,
    pub height: u32,
    /// Frames that arrived while the stream was displayed.
    pub received: u64,
    /// Frames that arrived while nobody displayed it (ignored).
    pub unwatched: u64,
    /// Frames lost on our side: shed at the queue (and gated until the next keyframe), or
    /// decoded with no free slot.
    pub dropped: u64,
    pub decoded: u64,
    /// Pictures handed to the renderer.
    pub published: u64,
    /// Distinct pictures that reached a rendered frame.
    pub presented: u64,
    /// Published but overtaken by a newer picture before a frame was drawn.
    pub replaced: u64,
    pub errors: u64,
    pub keyframe_requests: u64,
    pub decoder_opens: u64,
    pub fallbacks: u64,
    pub device_losses: u64,
    pub bytes: u64,
    /// Frames waiting for the decoder now (0..=3).
    pub queue: usize,
    /// Packet handed to the decoder -> picture published, milliseconds.
    pub decode_ms_p50: f64,
    pub decode_ms_p99: f64,
    /// Frame arrived at the hub -> picture published, milliseconds.
    pub latency_ms_p50: f64,
    pub latency_ms_p99: f64,
    /// Between two pictures reaching a rendered frame, milliseconds.
    pub present_interval_ms_p50: f64,
    pub present_interval_ms_p99: f64,
}

/// Rates between two reads of a stream's stats: what the overlay's "fps" lines show. Keep one
/// per overlay and call [`StatsSampler::sample`] when it repaints (the renderer does so twice
/// a second while the overlay is open).
pub struct StatsSampler {
    last: Option<(Instant, StreamStats)>,
}

/// Per-second rates over the time since the previous sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamRates {
    /// Frames arriving (the renderer's `inputFps`).
    pub input_fps: f64,
    /// Pictures decoded (the renderer's `fps`).
    pub decoded_fps: f64,
    pub presented_fps: f64,
    pub kbit_per_s: f64,
}

impl StatsSampler {
    pub fn new() -> Self {
        Self { last: None }
    }

    pub fn sample(&mut self, stats: &StreamStats) -> StreamRates {
        let now = Instant::now();
        let rates = match &self.last {
            Some((then, previous)) => {
                let seconds = now.duration_since(*then).as_secs_f64().max(1e-3);
                let rate = |new: u64, old: u64| new.saturating_sub(old) as f64 / seconds;
                StreamRates {
                    input_fps: rate(stats.received, previous.received),
                    decoded_fps: rate(stats.decoded, previous.decoded),
                    presented_fps: rate(stats.presented, previous.presented),
                    kbit_per_s: rate(stats.bytes, previous.bytes) * 8.0 / 1000.0,
                }
            }
            None => StreamRates::default(),
        };
        self.last = Some((now, stats.clone()));
        rates
    }
}

impl Default for StatsSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_percentiles_are_within_a_bucket() {
        let histogram = Histogram::new();
        for micros in 1..=10_000u64 {
            histogram.record(micros);
        }
        let [p50, p99] = histogram.percentiles([50.0, 99.0]);
        assert!((p50 - 5.0).abs() / 5.0 < 0.04, "p50 {p50}");
        assert!((p99 - 9.9).abs() / 9.9 < 0.04, "p99 {p99}");
        assert_eq!(Histogram::new().percentiles([50.0]), [0.0]);
    }

    #[test]
    fn buckets_are_monotonic() {
        let mut last = 0;
        for micros in [
            1,
            2,
            3,
            4,
            5,
            7,
            8,
            100,
            1000,
            8333,
            16_667,
            1 << 20,
            u64::MAX,
        ] {
            let bucket = Histogram::bucket(micros);
            assert!(bucket >= last && bucket < Histogram::BUCKETS, "{micros}");
            last = bucket;
        }
    }

    #[test]
    fn presented_is_matched_to_its_frame() {
        let counters = Counters::new();
        let arrived = Instant::now();
        let serial = counters.frame_started(arrived, 100);
        counters.frame_decoded(serial);
        counters.frame_published(serial, arrived, arrived);
        counters.frame_presented(serial);
        let stats = counters.snapshot(0);
        assert_eq!(
            (
                stats.decoded,
                stats.published,
                stats.presented,
                stats.replaced
            ),
            (1, 1, 1, 0)
        );
        assert!(counters.trace.entry(serial).presented.load(RELAXED) > 0);
    }
}
