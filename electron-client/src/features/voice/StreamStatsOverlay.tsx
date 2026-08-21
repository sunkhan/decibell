import { useStreamStatsStore } from "./streamStats";

/// Live decode-stats overlay (toggled from the stream controls). Reads the
/// stats the persistent StreamVideoPlayer publishes for `username`. Mounted only
/// while the overlay is on, so it re-renders (~2/s) only when actually shown.
export default function StreamStatsOverlay({ username }: { username: string }) {
  const stats = useStreamStatsStore((s) => s.statsByUser[username]);
  if (!stats) return null;

  const rows: [string, string][] = [
    ["Codec", stats.codecLabel],
    ["Resolution", stats.width > 0 ? `${stats.width}×${stats.height}` : "—"],
    ["Input", `${stats.inputFps} fps`],
    ["Decode", `${stats.fps} fps`],
    ["Queue", String(stats.queue)],
    ["Dropped", String(stats.dropped)],
  ];

  return (
    <div className="pointer-events-none absolute left-3 top-3 z-20 min-w-[150px] rounded-md border border-white/10 bg-black/70 px-3 py-2 font-mono text-[11px] leading-relaxed text-white/90 shadow-float backdrop-blur-sm">
      {rows.map(([k, v]) => (
        <div key={k} className="flex justify-between gap-4">
          <span className="text-white/50">{k}</span>
          <span
            className={
              k === "Dropped" && stats.dropped > 0
                ? "text-warning"
                : k === "Queue" && stats.queue > 3
                  ? "text-warning"
                  : ""
            }
          >
            {v}
          </span>
        </div>
      ))}
    </div>
  );
}
