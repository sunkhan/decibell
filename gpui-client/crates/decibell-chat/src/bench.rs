//! The scroll benchmark: the fixture channel scrolled into history at a constant speed, every
//! frame recorded on the UI thread (`decibell --bench-scroll <seconds> [--speed <px/s>]`,
//! mirroring `tools/perf/electron-scroll.mjs`).
//!
//! The list drives itself: while a [`Recording`] is attached, each frame's prepaint moves the
//! position by `speed × dt` (a precise delta, as a touchpad would) and asks for the next frame,
//! so frames come at the display's pace or, when one takes too long, late. A frame's record is
//! its start time, the µs of the list's prepaint (measuring rows that scrolled in) and paint,
//! the rows laid out for it and, when the binary counts allocations ([`set_alloc_probe`]), the
//! allocations of the list's two phases and of the whole frame.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Result;

static ALLOC_PROBE: OnceLock<fn() -> u64> = OnceLock::new();

/// Lets the benchmark read the process's allocation count (a counting global allocator in the
/// binary that runs it: `examples/chat_bench.rs`).
pub fn set_alloc_probe(probe: fn() -> u64) {
    let _ = ALLOC_PROBE.set(probe);
}

fn allocations() -> u64 {
    ALLOC_PROBE.get().map_or(0, |probe| probe())
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: Instant,
    pub prepaint_us: u32,
    pub paint_us: u32,
    /// Rows laid out in this frame.
    pub layouts: u32,
    /// Allocations inside the list's prepaint, while it painted its rows, and while it
    /// registered its four listeners; and since the previous frame started (everything on every
    /// thread: GPUI's own work, the views around the list, the model, the fixture's pages).
    pub prepaint_allocs: u32,
    pub rows_allocs: u32,
    pub listener_allocs: u32,
    pub frame_allocs: u32,
    /// µs spent between this frame and the previous one in the list's deferred work (pages,
    /// trims) and in background measuring slices.
    pub between_us: u32,
}

pub struct Recording {
    speed: f64,
    last: Option<Instant>,
    layouts_before: u64,
    mark: u64,
    allocs_at_previous: u64,
    pending: Option<Sample>,
    pub samples: Vec<Sample>,
}

impl Recording {
    pub fn new(speed: f64) -> Self {
        // Room for a minute at 300 Hz: recording must not allocate while it measures.
        Self { speed, last: None, layouts_before: 0, mark: 0, allocs_at_previous: allocations(), pending: None, samples: Vec::with_capacity(20_000) }
    }

    /// A frame's prepaint begins: how far to move for it.
    pub(crate) fn frame_started(&mut self, layouts: u64) -> f64 {
        let now = Instant::now();
        let allocs = allocations();
        let delta = self.last.map_or(0., |last| self.speed * now.duration_since(last).as_secs_f64());
        self.last = Some(now);
        self.layouts_before = layouts;
        self.pending = Some(Sample {
            at: now,
            prepaint_us: 0,
            paint_us: 0,
            layouts: 0,
            prepaint_allocs: 0,
            rows_allocs: 0,
            listener_allocs: 0,
            frame_allocs: (allocs - self.allocs_at_previous) as u32,
            between_us: 0,
        });
        self.allocs_at_previous = allocs;
        self.mark = allocs;
        delta
    }

    /// A phase of the frame ended: its allocations go to `phase` (0 prepaint, 1 rows, 2 listeners;
    /// 3 only restarts the count, at the start of paint).
    pub(crate) fn phase(&mut self, phase: usize) {
        let now = allocations();
        if let Some(sample) = &mut self.pending {
            let made = (now - self.mark) as u32;
            match phase {
                0 => sample.prepaint_allocs = made,
                1 => sample.rows_allocs = made,
                2 => sample.listener_allocs = made,
                _ => {}
            }
        }
        self.mark = now;
    }

    pub(crate) fn frame_painted(&mut self, prepaint_us: u32, paint_us: u32, layouts: u64, between_us: u32) {
        let Some(mut sample) = self.pending.take() else { return };
        sample.prepaint_us = prepaint_us;
        sample.paint_us = paint_us;
        sample.between_us = between_us;
        sample.layouts = (layouts - self.layouts_before) as u32;
        if self.samples.len() < self.samples.capacity() {
            self.samples.push(sample);
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// The figures of one run, as JSON.
pub fn summarize(recording: &Recording, speed: f64, seconds: f64, refresh_hz: Option<f64>) -> serde_json::Value {
    let samples = &recording.samples;
    let mut intervals: Vec<f64> = samples.windows(2).map(|pair| pair[1].at.duration_since(pair[0].at).as_secs_f64() * 1e3).collect();
    let raw = intervals.clone();
    intervals.sort_by(f64::total_cmp);
    // The display's period: given, else the median interval (a list that keeps up runs at it).
    let period = refresh_hz.map_or_else(|| percentile(&intervals, 0.5), |hz| 1e3 / hz);
    let missed: usize = raw.iter().map(|interval| ((interval / period).round() as usize).saturating_sub(1)).sum();
    let late = raw.iter().filter(|interval| **interval > 1.5 * period).count();
    let column = |f: fn(&Sample) -> f64| {
        let mut values: Vec<f64> = samples.iter().map(f).collect();
        values.sort_by(f64::total_cmp);
        serde_json::json!({ "p50": percentile(&values, 0.5), "p95": percentile(&values, 0.95), "p99": percentile(&values, 0.99), "max": values.last().copied().unwrap_or(0.) })
    };
    // Frames in which no row was laid out: the steady state the "no allocation" rule is about.
    let steady: Vec<&Sample> = samples.iter().skip(20).filter(|sample| sample.layouts == 0).collect();
    let steady_allocs = |f: fn(&Sample) -> u32| {
        let mut values: Vec<u32> = steady.iter().map(|sample| f(sample)).collect();
        values.sort_unstable();
        serde_json::json!({ "min": values.first().copied().unwrap_or(0), "p50": values.get(values.len() / 2).copied().unwrap_or(0), "max": values.last().copied().unwrap_or(0) })
    };
    serde_json::json!({
        "scenario": "scroll",
        "speed_px_s": speed,
        "seconds": seconds,
        "frames": samples.len(),
        "period_ms": period,
        "refresh_hz": 1e3 / period.max(1e-9),
        "interval_ms": { "p50": percentile(&intervals, 0.5), "p95": percentile(&intervals, 0.95), "p99": percentile(&intervals, 0.99), "max": intervals.last().copied().unwrap_or(0.) },
        "missed_vsyncs": missed,
        "late_frames": late,
        "list_prepaint_us": column(|sample| sample.prepaint_us as f64),
        "list_paint_us": column(|sample| sample.paint_us as f64),
        "rows_laid_out": samples.iter().map(|sample| sample.layouts as u64).sum::<u64>(),
        "frames_with_layouts": samples.iter().filter(|sample| sample.layouts > 0).count(),
        "allocation_counting": ALLOC_PROBE.get().is_some(),
        "steady_frames": steady.len(),
        "steady_prepaint_allocs": steady_allocs(|sample| sample.prepaint_allocs),
        "steady_rows_allocs": steady_allocs(|sample| sample.rows_allocs),
        "steady_listener_allocs": steady_allocs(|sample| sample.listener_allocs),
        "steady_frame_allocs": steady_allocs(|sample| sample.frame_allocs),
        "late": raw.iter().enumerate().filter(|(_, interval)| **interval > 1.5 * period).take(40).map(|(index, interval)| {
            let sample = &samples[index + 1];
            serde_json::json!({ "frame": index + 1, "interval_ms": interval, "prepaint_us": sample.prepaint_us, "paint_us": sample.paint_us, "rows_laid_out": sample.layouts, "between_us": sample.between_us })
        }).collect::<Vec<_>>(),
    })
}

/// The same recording on the chat pane that is on screen in the signed-in app (the channel the
/// perf scenario opened: `tools/perf/scenarios.sh gpui scroll`): scrolls into history at
/// `speed` px/s for `seconds` and returns the summary. `None`: no chat pane is shown.
pub async fn run_live(app: gpui::AsyncApp, seconds: f64, speed: f64, refresh_hz: Option<f64>) -> Option<serde_json::Value> {
    let list = app.update(|cx| cx.try_global::<crate::pane::ShownList>().and_then(|shown| shown.0.upgrade()))?;
    app.update(|cx| list.update(cx, |list, cx| list.begin_bench(speed, cx)));
    app.background_executor().timer(Duration::from_secs_f64(seconds)).await;
    let recording = app.update(|cx| list.update(cx, |list, _| list.end_bench()))?;
    Some(summarize(&recording, speed, seconds, refresh_hz))
}

/// Opens the fixture channel in its own window and scrolls into history at each of `speeds`
/// for `seconds`. Returns one summary per speed.
#[cfg(feature = "fixtures")]
pub async fn run(app: gpui::AsyncApp, seconds: f64, speeds: &[f64], refresh_hz: Option<f64>) -> Result<serde_json::Value> {
    use crate::fixture::Which;
    use crate::selftests::Harness;
    let h = Harness::open_pages(&app, Which::General, 1)?;
    let mut runs = Vec::new();
    for speed in speeds {
        h.rest().await?;
        // Each run starts from the present, warm.
        h.with(|list, cx| list.jump_to_present(cx))?;
        h.rest().await?;
        h.with(|list, cx| list.begin_bench(*speed, cx))?;
        h.cx.sleep(Duration::from_secs_f64(seconds)).await;
        let recording = h.with(|list, _| list.end_bench())?;
        if let Some(recording) = recording {
            let mut summary = summarize(&recording, *speed, seconds, refresh_hz);
            let (rows, (bytes, layouts)) = h.with(|list, _| (list.row_count(), list.retained()))?;
            summary["rows_in_window"] = serde_json::json!(rows);
            summary["list_retained_bytes"] = serde_json::json!(bytes);
            summary["rows_holding_a_layout"] = serde_json::json!(layouts);
            runs.push(summary);
        }
    }
    // `DECIBELL_BENCH_HOLD=<seconds>`: keep the scrolled channel open and idle for that long
    // (for `tools/perf/measure.sh --pid`), then leave it and hold again (what leaving frees).
    if let Some(hold) = std::env::var("DECIBELL_BENCH_HOLD").ok().and_then(|hold| hold.parse::<f64>().ok()) {
        h.rest().await?;
        eprintln!("decibell: bench-hold scrolled pid={}", std::process::id());
        h.cx.sleep(Duration::from_secs_f64(hold)).await;
        h.cx.update(|_, cx| h.pane.update(cx, |pane, cx| pane.switch(Which::Features, cx)))?;
        h.rest().await?;
        eprintln!("decibell: bench-hold left pid={}", std::process::id());
        h.cx.sleep(Duration::from_secs_f64(hold)).await;
    }
    h.close();
    Ok(serde_json::json!({ "client": "gpui", "runs": runs }))
}
