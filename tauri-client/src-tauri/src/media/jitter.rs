use std::collections::HashMap;
use std::time::Instant;

// ── Adaptive jitter buffer (NetEQ-light) ─────────────────────────────────────
//
// Holds incoming packets for a short time before decoding so late/out-of-order
// packets can be reordered. The target occupancy adapts to observed network
// jitter: a stable link plays at ~40ms depth, a jittery link grows to ~300ms.
// When the buffer accumulates more than the target (plus hysteresis), drain()
// silently drops a frame instead of playing it — trading a brief artifact for
// permanently lower latency, rather than accumulating hundreds of ms of delay.
// Missing packets use Opus PLC / FEC at the caller.

pub const FRAME_DUR_SEC: f64 = 0.020; // 20ms per Opus frame at 48kHz

/// Absolute floor on occupancy before playback starts / resumes.
pub const JITTER_MIN_DEPTH: usize = 2;  // 40ms — a single late packet can still land on time
/// Soft ceiling on adaptive target depth.
pub const JITTER_MAX_DEPTH: usize = 15; // 300ms — above this, audio feels laggy regardless
/// Hard safety cap. If the buffer grows past this we force-drain.
pub const JITTER_MAX: usize = 30;

/// Extra slack above target_depth before we start silently dropping frames
/// to bring latency back down.
const SHRINK_HYSTERESIS: usize = 3;

/// Consecutive PLC frames that trigger a re-sync reset.
const PLC_RESET_THRESHOLD: u32 = 10;

pub struct JitterBuffer {
    packets: HashMap<u16, Vec<u8>>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
    consecutive_losses: u32,

    // ── Adaptive depth state ──
    last_arrival: Option<Instant>,
    /// RFC-3550-style jitter estimate in seconds.
    jitter_sec: f64,
    /// Current adaptive occupancy target.
    target_depth: usize,

    // ── Stats (read externally for diagnostics) ──
    pub plc_frames: u64,
    pub dropped_frames: u64,
    /// Frames dequeued from a real packet (not PLC). Combined with
    /// plc_frames over a window this gives a true packet-loss rate
    /// for the user-panel telemetry popover.
    pub decoded_frames: u64,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            packets: HashMap::new(),
            next_seq: 0,
            initialized: false,
            ready: false,
            consecutive_losses: 0,
            last_arrival: None,
            jitter_sec: 0.0,
            target_depth: JITTER_MIN_DEPTH,
            plc_frames: 0,
            dropped_frames: 0,
            decoded_frames: 0,
        }
    }

    /// Current target occupancy in packets — for diagnostics.
    pub fn target(&self) -> usize { self.target_depth }
    /// Estimated one-way jitter in milliseconds.
    pub fn jitter_ms(&self) -> f64 { self.jitter_sec * 1000.0 }

    /// Update RFC-3550 jitter estimate and recompute target depth.
    /// J = J + (|D(i-1,i)| - J) / 16
    fn on_arrival(&mut self, now: Instant) {
        if let Some(prev) = self.last_arrival {
            let iat = now.duration_since(prev).as_secs_f64();
            let d = (iat - FRAME_DUR_SEC).abs();
            self.jitter_sec += (d - self.jitter_sec) / 16.0;
            // Target: roughly 2× observed jitter, floored at MIN, ceilinged at MAX.
            let target_frames = (self.jitter_sec * 2.0 / FRAME_DUR_SEC).ceil() as usize + JITTER_MIN_DEPTH;
            self.target_depth = target_frames.clamp(JITTER_MIN_DEPTH, JITTER_MAX_DEPTH);
        }
        self.last_arrival = Some(now);
    }

    /// Insert a packet. Ignores packets behind the play cursor.
    pub fn push(&mut self, seq: u16, data: Vec<u8>) {
        self.on_arrival(Instant::now());

        if !self.initialized {
            self.next_seq = seq;
            self.initialized = true;
        }
        // Detect sequence reset (user left and rejoined): if the incoming seq
        // appears to be far behind next_seq, it's actually a fresh sequence
        // starting from 0. Reinitialize the buffer to accept the new stream.
        let diff = seq.wrapping_sub(self.next_seq);
        if diff >= 32768 {
            self.packets.clear();
            self.next_seq = seq;
            self.ready = false;
        }
        let diff = seq.wrapping_sub(self.next_seq);
        if diff < 32768 {
            self.packets.insert(seq, data);
        }
        if !self.ready && self.packets.len() >= self.target_depth {
            self.ready = true;
        }
        // Force-drain excess so the buffer can't grow unbounded.
        while self.packets.len() > JITTER_MAX {
            if !self.packets.contains_key(&self.next_seq) {
                if let Some(&earliest) = self.packets.keys()
                    .min_by_key(|&&s| s.wrapping_sub(self.next_seq))
                {
                    self.next_seq = earliest;
                } else {
                    break;
                }
            }
            self.packets.remove(&self.next_seq);
            self.next_seq = self.next_seq.wrapping_add(1);
            self.dropped_frames += 1;
        }
    }

    /// Pop the next frame. Returns:
    /// - `Some(Some(data))` — packet present, decode normally
    /// - `Some(None)` — packet missing, caller should do PLC
    /// - `None` — buffer not ready (initial fill or post-reset re-buffering)
    pub fn drain(&mut self) -> Option<Option<Vec<u8>>> {
        if !self.ready { return None; }

        // Auto-recovery: if we've produced N consecutive PLC frames the audio
        // is already unintelligible. Reset and re-buffer from scratch.
        if self.consecutive_losses >= PLC_RESET_THRESHOLD {
            self.reset();
            return None;
        }

        // Latency cap: if the buffer has grown past target + hysteresis, fast-
        // forward by dropping one ready frame without emitting it. A single
        // 20ms skip is far less disruptive than carrying hundreds of ms of
        // extra delay for the rest of the session.
        if self.packets.len() > self.target_depth + SHRINK_HYSTERESIS {
            if self.packets.remove(&self.next_seq).is_some() {
                self.dropped_frames += 1;
            }
            self.next_seq = self.next_seq.wrapping_add(1);
            // Fall through to emit the next frame this drain call.
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let result = self.packets.remove(&seq);
        if result.is_some() {
            self.consecutive_losses = 0;
            self.decoded_frames += 1;
        } else {
            self.consecutive_losses += 1;
            self.plc_frames += 1;
        }
        Some(result)
    }

    /// Reset the buffer to its initial state, forcing a re-buffering period.
    pub fn reset(&mut self) {
        self.packets.clear();
        self.initialized = false;
        self.ready = false;
        self.consecutive_losses = 0;
        // Keep jitter estimate — network conditions don't change on reset.
    }

    /// Peek at the next packet (next_seq) without consuming it.
    /// Used for FEC: when current packet is missing, check if the next
    /// packet is available to decode with fec=true.
    pub fn peek_next(&self) -> Option<&Vec<u8>> {
        self.packets.get(&self.next_seq)
    }
}
