import { create } from "zustand";

/// Live decode stats for a watched stream, published by the (single persistent)
/// StreamVideoPlayer and read by the stats overlay. Keyed by streamer uid
/// so the overlay can look up the currently-focused stream.
export interface StreamStats {
  /// Friendly codec label, e.g. "H.264", "HEVC", "AV1".
  codecLabel: string;
  /// Source (decoded) resolution.
  width: number;
  height: number;
  /// Frames arriving at the player per second (before drop/decode). For your
  /// own stream this is the encode/capture rate; for a remote stream it's the
  /// network-received rate.
  inputFps: number;
  /// Measured decoded frames per second (rolling ~0.5s window).
  fps: number;
  /// Current WebCodecs decode queue depth (backpressure indicator).
  queue: number;
  /// Total frames dropped by the renderer since this player mounted
  /// (backpressure sheds + decoder errors).
  dropped: number;
}

interface StreamStatsState {
  statsByUser: Record<number, StreamStats | undefined>;
  publishStats: (uid: number, stats: StreamStats) => void;
  clearStats: (uid: number) => void;
}

export const useStreamStatsStore = create<StreamStatsState>((set) => ({
  statsByUser: {},
  publishStats: (uid, stats) =>
    set((s) => ({ statsByUser: { ...s.statsByUser, [uid]: stats } })),
  clearStats: (uid) =>
    set((s) => {
      if (!(uid in s.statsByUser)) return s;
      const next = { ...s.statsByUser };
      delete next[uid];
      return { statsByUser: next };
    }),
}));
