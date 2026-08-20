import { create } from "zustand";

/// Live decode stats for a watched stream, published by the (single persistent)
/// StreamVideoPlayer and read by the stats overlay. Keyed by streamer username
/// so the overlay can look up the currently-focused stream.
export interface StreamStats {
  /// Friendly codec label, e.g. "H.264", "HEVC", "AV1".
  codecLabel: string;
  /// Source (decoded) resolution.
  width: number;
  height: number;
  /// Measured decoded frames per second (rolling ~1s window).
  fps: number;
  /// Current WebCodecs decode queue depth (backpressure indicator).
  queue: number;
  /// Total frames dropped by the renderer since this player mounted
  /// (backpressure sheds + decoder errors).
  dropped: number;
}

interface StreamStatsState {
  statsByUser: Record<string, StreamStats | undefined>;
  publishStats: (username: string, stats: StreamStats) => void;
  clearStats: (username: string) => void;
}

export const useStreamStatsStore = create<StreamStatsState>((set) => ({
  statsByUser: {},
  publishStats: (username, stats) =>
    set((s) => ({ statsByUser: { ...s.statsByUser, [username]: stats } })),
  clearStats: (username) =>
    set((s) => {
      if (!(username in s.statsByUser)) return s;
      const next = { ...s.statsByUser };
      delete next[username];
      return { statsByUser: next };
    }),
}));
