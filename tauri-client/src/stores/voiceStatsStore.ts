// Time-series ring buffer for the user-panel connection telemetry popover.
// Backend emits a sample every ~2s while connected to a voice channel
// (see VoiceEvent::ConnectionStats in pipeline.rs). We keep up to 5
// minutes of history so the popover can plot the recent ping + packet
// loss curves Discord-style.

import { create } from "zustand";

export interface ConnectionStatsSample {
  /** Epoch milliseconds (Date.now() when the IPC event landed). */
  ts: number;
  /** Round-trip latency in ms. null until the first PING reply lands. */
  pingMs: number | null;
  /** Audio packet loss percentage over the last sample window. */
  lossPct: number;
}

// Backend cadence is 2s; 5min = 300s = 150 samples. Hard cap keeps the
// store bounded and the SVG cheap to render.
const MAX_SAMPLES = 200;
// If samples stop arriving for this long (e.g. brief IPC stall), drop
// the entire history on the next push so the graph doesn't draw a long
// horizontal segment between two unrelated voice sessions.
const STALE_GAP_MS = 30_000;

interface VoiceStatsState {
  samples: ConnectionStatsSample[];
  pushSample: (s: ConnectionStatsSample) => void;
  clear: () => void;
}

export const useVoiceStatsStore = create<VoiceStatsState>((set, get) => ({
  samples: [],
  pushSample: (sample) => {
    const prev = get().samples;
    const last = prev[prev.length - 1];
    const reset = !!last && sample.ts - last.ts > STALE_GAP_MS;
    const base = reset ? [] : prev;
    const next = base.length >= MAX_SAMPLES
      ? [...base.slice(base.length - MAX_SAMPLES + 1), sample]
      : [...base, sample];
    set({ samples: next });
  },
  clear: () => set({ samples: [] }),
}));
