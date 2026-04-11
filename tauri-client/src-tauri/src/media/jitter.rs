use std::collections::HashMap;

// ── Jitter buffer ────────────────────────────────────────────────────────────
//
// Holds incoming packets for a short time before decoding, so that late/
// out-of-order packets can be reordered. When a packet is truly lost, the
// Opus decoder's PLC (Packet Loss Concealment) fills the gap smoothly.

pub const JITTER_DEPTH: usize = 3; // packets to buffer before starting playback (60ms)
pub const JITTER_MAX: usize = 30;  // safety cap — force-drain if buffer grows past this

pub struct JitterBuffer {
    packets: HashMap<u16, Vec<u8>>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
    /// Consecutive drain() calls that returned a missing packet (PLC).
    /// When this exceeds the threshold, the buffer resets to re-sync.
    consecutive_losses: u32,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self { packets: HashMap::new(), next_seq: 0, initialized: false, ready: false, consecutive_losses: 0 }
    }

    /// Insert a packet. Ignores packets behind the play cursor.
    pub fn push(&mut self, seq: u16, data: Vec<u8>) {
        if !self.initialized {
            self.next_seq = seq;
            self.initialized = true;
        }
        // Detect sequence reset (user left and rejoined): if the incoming seq
        // appears to be far behind next_seq, it's actually a fresh sequence
        // starting from 0. Reinitialize the buffer to accept the new stream.
        let diff = seq.wrapping_sub(self.next_seq);
        if diff >= 32768 {
            // seq is "behind" next_seq by more than half the u16 range —
            // this is a wraparound/reset, not a late packet.
            self.packets.clear();
            self.next_seq = seq;
            self.ready = false;
        }
        let diff = seq.wrapping_sub(self.next_seq);
        if diff < 32768 {
            self.packets.insert(seq, data);
        }
        if !self.ready && self.packets.len() >= JITTER_DEPTH {
            self.ready = true;
        }
        // Force-drain excess so the buffer can't grow unbounded.
        // If next_seq points to a gap (no packet), jump to the earliest actual
        // entry first — otherwise the while loop would spin through thousands
        // of empty sequence numbers before hitting a real packet.
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
        }
    }

    /// Pop the next frame. Returns:
    /// - `Some(Some(data))` — packet present, decode normally
    /// - `Some(None)` — packet missing, caller should do PLC
    /// - `None` — buffer not ready (initial fill or post-reset re-buffering)
    pub fn drain(&mut self) -> Option<Option<Vec<u8>>> {
        if !self.ready { return None; }

        // Auto-recovery: if we've produced 10+ consecutive PLC frames (200ms),
        // the audio is already unintelligible. Reset and re-buffer from scratch
        // so playback can resume cleanly once packets arrive.
        if self.consecutive_losses >= 10 {
            self.reset();
            return None;
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let result = self.packets.remove(&seq);
        if result.is_some() {
            self.consecutive_losses = 0;
        } else {
            self.consecutive_losses += 1;
        }
        Some(result)
    }

    /// Reset the buffer to its initial state, forcing a re-buffering period.
    /// Called automatically after prolonged packet loss.
    pub fn reset(&mut self) {
        self.packets.clear();
        self.initialized = false;
        self.ready = false;
        self.consecutive_losses = 0;
    }

    /// Peek at the next packet (next_seq) without consuming it.
    /// Used for FEC: when current packet is missing, check if the next
    /// packet is available to decode with fec=true.
    pub fn peek_next(&self) -> Option<&Vec<u8>> {
        self.packets.get(&self.next_seq)
    }
}
